use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ObjectFit, ParentElement as _, Render, Styled as _, StyledImage as _, Window,
    WindowHandle, canvas, div, fill, hsla, img, point, px, rgb, size, surface,
};
use objc2::rc::Retained;
use objc2_core_foundation::CFRetained;
use objc2_core_video::CVPixelBuffer;
use objc2_quartz_core::CALayer;

use crate::app::{self, Annuncia};
use crate::camera::FrameSlot;
use crate::native::{self, OverlayLevel, TopLeftRect};
use crate::orb;
use crate::popout::{HEADROOM, PopoutSlot};

pub struct Bubble {
    state: Entity<Annuncia>,
    frames: Arc<FrameSlot>,
    /// Set for the pop-out bubble, whose video is a native layer under this view.
    popout: Option<Popout>,
    /// The animated avatar's latest frame, while it's showing.
    orb: Option<Arc<orb::Frame>>,
}

/// How often the animated avatar moves on.
const ORB_FRAME: Duration = Duration::from_millis(40);

struct Popout {
    slot: Arc<PopoutSlot>,
    /// Installed once the window exists.
    layer: Option<Retained<CALayer>>,
    /// The frame on screen, kept alive while the layer shows it.
    shown: Option<CFRetained<CVPixelBuffer>>,
    seen: u64,
}

/// `anchor` places the circle; the pop-out bubble's window extends above it for the head.
pub fn open(
    state: Entity<Annuncia>,
    diameter: f32,
    anchor: Option<TopLeftRect>,
    popout: bool,
    cx: &mut App,
) -> Option<WindowHandle<Bubble>> {
    let geometry = native::menu_bar_geometry()?;
    let d = f64::from(diameter);
    let margin = 40.;
    let circle = anchor.unwrap_or(TopLeftRect {
        x: geometry.screen.x + margin,
        y: geometry.screen.y + geometry.screen.h - d - margin,
        w: d,
        h: d,
    });
    let headroom = if popout { d * HEADROOM } else { 0. };
    let rect = TopLeftRect {
        y: circle.y - headroom,
        h: circle.h + headroom,
        ..circle
    };
    let mut options = app::overlay_options(rect, false);
    options.is_movable = true;
    cx.open_window(options, |window, cx| {
        native::setup_window(
            window,
            cx,
            native::WindowSetup {
                // The pop-out bubble draws its own ring and has no circular edge to clip to.
                shadow: !popout,
                corner_radius: (!popout).then_some(d / 2.),
                ..app::overlay_setup(OverlayLevel::Floating, rect)
            },
        );
        cx.new(|cx| Bubble::new(state, popout, window, cx))
    })
    .inspect_err(|err| eprintln!("annuncia: bubble window: {err}"))
    .ok()
}

impl Bubble {
    fn new(
        state: Entity<Annuncia>,
        popout: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        app::observe_state(&state, window, cx);
        let frames = state.read(cx).frames.clone();
        let popout = popout.then(|| Popout {
            slot: state.read(cx).popout_frames.clone(),
            layer: None,
            shown: None,
            seen: u64::MAX,
        });
        if popout.is_some() {
            // The native layer goes in once the window's views exist.
            cx.defer_in(window, |this: &mut Self, window, _| {
                if let Some(popout) = &mut this.popout {
                    popout.layer = native::install_video_layer(window);
                }
            });
        }
        let watched = frames.clone();
        // Redraw when the capture queue delivers a new frame (~30 fps), or step the animated
        // avatar along.
        cx.spawn(async move |this, cx| {
            let mut seen = u64::MAX;
            // The avatar follows your voice twice over: a quick pulse that rises and falls with
            // each syllable, and a slow energy that speeds the drift a little while you talk.
            let (mut clock, mut pulse, mut energy, mut last) = (0f32, 0f32, 0f32, Instant::now());
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let Ok(orb) = this.update(cx, |this, cx| {
                    if let Some(popout) = &mut this.popout {
                        // The layer updates itself; GPUI has nothing to redraw.
                        let seq = popout.slot.seq();
                        if seq != popout.seen
                            && let (Some(layer), Some(frame)) =
                                (&popout.layer, popout.slot.latest())
                        {
                            popout.seen = seq;
                            native::show_frame(layer, &frame);
                            popout.shown = Some(frame);
                        }
                        return None;
                    }
                    let orb = this.orb_cells(cx);
                    if orb.is_none() {
                        this.orb = None;
                    }
                    let seq = watched.seq();
                    if seq != seen {
                        seen = seq;
                        cx.notify();
                    }
                    orb
                }) else {
                    break;
                };
                let Some((cells, voice)) = orb else {
                    continue;
                };
                let dt = last.elapsed();
                if dt < ORB_FRAME {
                    continue;
                }
                last = Instant::now();
                let dt = dt.as_secs_f32().min(0.1);
                // Each rate is the share of the gap closed per 40 ms, rising, then falling.
                let ease = |value: &mut f32, rise: f32, fall: f32| {
                    let rate = if voice > *value { rise } else { fall };
                    *value += (voice - *value) * (1. - (1. - rate).powf(dt / 0.04));
                };
                ease(&mut pulse, 0.7, 0.3);
                ease(&mut energy, 0.25, 0.06);
                clock += dt * (1. + energy * 0.5);
                let (t, pulse) = (clock, pulse);
                let frame = cx
                    .background_executor()
                    .spawn(async move { orb::frame(cells, t, pulse) })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.orb = Some(Arc::new(frame));
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            state,
            frames,
            popout,
            orb: None,
        }
    }

    /// When the animated avatar should show (camera off, no photo): its size in cells, and the
    /// voice level to move with.
    fn orb_cells(&self, cx: &App) -> Option<(usize, f32)> {
        let state = self.state.read(cx);
        let settings = &state.settings;
        (!settings.camera_enabled && settings.avatar_image.is_none()).then(|| {
            let cells = (settings.bubble_size.diameter() / orb::CELL).round() as usize;
            let voice = state.mic_level.as_ref().map_or(0., |m| m.level());
            (cells, voice)
        })
    }
}

impl Render for Bubble {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.clone();
        let hide = div()
            .absolute()
            .bottom(px(14.))
            .w_full()
            .flex()
            .justify_center()
            .opacity(0.)
            .group_hover("bubble", |s| s.opacity(1.))
            .child(
                div()
                    .id("hide")
                    .px(px(9.))
                    .py(px(3.))
                    .rounded(px(6.))
                    .bg(hsla(0., 0., 0., 0.6))
                    .hover(|s| s.bg(hsla(0., 0., 0., 0.8)))
                    .cursor_pointer()
                    .text_size(px(11.))
                    .text_color(hsla(0., 0., 1., 0.95))
                    // Act on mouse down and stop it here, so it doesn't start a drag.
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        state.update(cx, |s, cx| s.hide_bubble(cx));
                    })
                    .child("Hide"),
            );

        if self.popout.is_some() {
            // The video (ring included) is the layer underneath; this view only adds the
            // controls, and is otherwise transparent so clicks there reach what's behind.
            return div()
                .id("bubble")
                .group("bubble")
                .size_full()
                .relative()
                .cursor_grab()
                .on_mouse_down(MouseButton::Left, |_, window, _| {
                    native::begin_window_drag(window)
                })
                .child(hide);
        }

        let settings = &self.state.read(cx).settings;
        let content = if settings.camera_enabled {
            match self.frames.latest() {
                Some(frame) => surface(frame)
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .into_any_element(),
                None => placeholder("Starting camera…").into_any_element(),
            }
        } else if let Some(path) = settings.avatar_image.clone() {
            img(path)
                .size_full()
                .rounded_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        } else {
            avatar(self.orb.clone()).into_any_element()
        };
        let showing_avatar = !settings.camera_enabled && settings.avatar_image.is_none();

        div()
            .id("bubble")
            .group("bubble")
            .size_full()
            .rounded_full()
            .overflow_hidden()
            .bg(if showing_avatar {
                Hsla::from(rgb(INK))
            } else {
                hsla(0., 0., 0.08, 1.)
            })
            .cursor_grab()
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                native::begin_window_drag(window)
            })
            .child(content)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .border(px(2.5))
                    .border_color(hsla(0., 0., 1., 0.9)),
            )
            .child(hide)
    }
}

/// The app icon's ink, under the avatar's three lighter tones, darkest to silver.
const INK: u32 = 0x0d0d24;
const TONES: [u32; 3] = [0x3a3a78, 0x9696d4, 0xd4d4f2];

/// The animated avatar: the frame's runs, in their tones, on the bubble's ink.
fn avatar(frame: Option<Arc<orb::Frame>>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let Some(frame) = frame else {
                return;
            };
            let cell = bounds.size.width / frame.cells as f32;
            let tones = TONES.map(|tone| Hsla::from(rgb(tone)));
            for &(row, first, end, tone) in &frame.runs {
                let origin = point(
                    bounds.origin.x + cell * f32::from(first),
                    bounds.origin.y + cell * f32::from(row),
                );
                let extent = size(cell * f32::from(end - first), cell);
                window.paint_quad(fill(
                    Bounds::new(origin, extent),
                    tones[usize::from(tone) - 1],
                ));
            }
        },
    )
    .size_full()
}

fn placeholder(label: &'static str) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .text_color(hsla(0., 0., 1., 0.6))
        .child(label)
}
