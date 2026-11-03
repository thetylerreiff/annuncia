use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, MouseButton,
    ObjectFit, ParentElement as _, Render, Styled as _, StyledImage as _, Window, WindowHandle,
    div, hsla, img, px, surface,
};
use objc2::rc::Retained;
use objc2_core_foundation::CFRetained;
use objc2_core_video::CVPixelBuffer;
use objc2_quartz_core::CALayer;

use crate::app::{self, Annuncia};
use crate::camera::FrameSlot;
use crate::native::{self, OverlayLevel, TopLeftRect};
use crate::popout::{HEADROOM, PopoutSlot};

pub struct Bubble {
    state: Entity<Annuncia>,
    frames: Arc<FrameSlot>,
    /// Set for the pop-out bubble, whose video is a native layer under this view.
    popout: Option<Popout>,
}

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
        // Redraw when the capture queue delivers a new frame (~30 fps).
        cx.spawn(async move |this, cx| {
            let mut seen = u64::MAX;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let Ok(()) = this.update(cx, |this, cx| {
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
                        return;
                    }
                    let seq = watched.seq();
                    if seq != seen {
                        seen = seq;
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        })
        .detach();
        Self {
            state,
            frames,
            popout,
        }
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
            placeholder("Camera off").into_any_element()
        };

        div()
            .id("bubble")
            .group("bubble")
            .size_full()
            .rounded_full()
            .overflow_hidden()
            .bg(hsla(0., 0., 0.08, 1.))
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
