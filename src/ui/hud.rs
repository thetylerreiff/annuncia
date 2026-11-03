//! The recording HUD: a black island that grows out of the notch (or, on screens without one,
//! a capsule under the menu bar). Status sits left of the camera housing, controls right of it.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, Context, Div, Entity,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, WindowHandle, div, hsla,
    prelude::FluentBuilder as _, pulsating_between, px, rgb,
};

use crate::app::{self, Annuncia, End, Phase, TranscriptState};
use crate::hotkeys::display_binding;
use crate::models::Transcription;
use crate::native::{self, MenuBarGeometry, OverlayLevel, TopLeftRect};
use crate::ui::icons::icon;

/// Each side of the notch: just enough for the timer on the left and two buttons on the right.
const WING: f64 = 72.;
const CAPSULE_WIDTH: f64 = 168.;
const CAPSULE_HEIGHT: f64 = 30.;
const SAVED_DRAWER: f64 = 38.;
/// Restart and Discard, shown while the pointer is over a recording's HUD.
const CONTROLS_DRAWER: f64 = 36.;
const NOTICE_DRAWER: f64 = 72.;
const DRAWER_WIDTH: f64 = 370.;

pub struct Hud {
    state: Entity<Annuncia>,
    geometry: MenuBarGeometry,
    applied: Option<TopLeftRect>,
    hovered: bool,
    /// Restart throws the recording away, so it takes a second click before this passes.
    confirm_restart_until: Option<Instant>,
    /// When the transcript was last copied, for a moment of "Copied" feedback.
    copied_at: Option<Instant>,
}

pub fn format_elapsed(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn open(state: Entity<Annuncia>, cx: &mut App) -> Option<WindowHandle<Hud>> {
    let geometry = native::menu_bar_geometry()?;
    let rect = frame_for(&geometry, 0., 0.);
    cx.open_window(app::overlay_options(rect, false), |window, cx| {
        native::setup_window(
            window,
            cx,
            app::overlay_setup(OverlayLevel::AboveMenuBar, rect),
        );
        cx.new(|cx| {
            app::observe_state(&state, window, cx);
            Hud {
                state,
                geometry,
                applied: Some(rect),
                hovered: false,
                confirm_restart_until: None,
                copied_at: None,
            }
        })
    })
    .inspect_err(|err| eprintln!("annuncia: hud window: {err}"))
    .ok()
}

/// `min_width` makes room for a drawer wider than the bar (the saved file's three buttons).
fn frame_for(g: &MenuBarGeometry, drawer: f64, min_width: f64) -> TopLeftRect {
    match g.notch {
        Some(notch) => {
            let w = (notch.w + 2. * WING).max(min_width);
            TopLeftRect {
                x: notch.x + notch.w / 2. - w / 2.,
                y: notch.y,
                w,
                h: notch.h + drawer,
            }
        }
        None => {
            let w = CAPSULE_WIDTH.max(min_width);
            TopLeftRect {
                x: g.screen.x + (g.screen.w - w) / 2.,
                y: g.screen.y + g.menu_bar_height + 6.,
                w,
                h: CAPSULE_HEIGHT + drawer,
            }
        }
    }
}

fn white(a: f32) -> Hsla {
    hsla(0., 0., 1., a)
}

impl Render for Hud {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let recording = matches!(state.phase, Phase::Recording { .. });
        let (drawer_height, min_width) = if state.notice.is_some() {
            (NOTICE_DRAWER, DRAWER_WIDTH)
        } else if matches!(state.phase, Phase::Saved { .. }) {
            (SAVED_DRAWER, DRAWER_WIDTH)
        } else if recording && self.hovered {
            (CONTROLS_DRAWER, 0.)
        } else {
            (0., 0.)
        };
        if !recording {
            self.confirm_restart_until = None;
        }
        let rect = frame_for(&self.geometry, drawer_height, min_width);
        if self.applied != Some(rect) {
            self.applied = Some(rect);
            native::set_frame(window, cx, rect);
        }

        let (left, right) = self.wings(cx);
        let notch = self.geometry.notch;
        let bar_height = px(notch.map_or(CAPSULE_HEIGHT, |n| n.h) as f32);

        let bar = div()
            .h(bar_height)
            .w_full()
            .flex()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .pl(px(10.))
                    .gap(px(6.))
                    .child(left),
            )
            .when_some(notch, |d, n| d.child(div().w(px(n.w as f32)).h_full()))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_end()
                    .pr(px(6.))
                    .gap(px(2.))
                    .child(right),
            );

        let drawer = (drawer_height > 0.).then(|| self.drawer(drawer_height, cx));
        let state = self.state.clone();
        div()
            .id("hud")
            .size_full()
            .flex()
            .flex_col()
            .bg(gpui::black())
            .text_color(white(0.92))
            .text_size(px(12.))
            .map(|d| match notch {
                Some(_) => d.rounded_bl(px(12.)).rounded_br(px(12.)),
                None => d.rounded(px(15.)),
            })
            // GPUI reports the first hover but, since this window never becomes key, not the
            // pointer leaving, so it never reports a second one. Pointer movement does arrive,
            // so it opens the drawer, and `watch_pointer` closes it.
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                state.update(cx, |s, _| s.hold_saved());
                if !this.hovered {
                    this.hovered = true;
                    this.watch_pointer(cx);
                    cx.notify();
                }
            }))
            .child(bar)
            .children(drawer)
    }
}

impl Hud {
    /// While the pointer is over the HUD, check its position directly and fold the drawer
    /// once it has been away for a moment.
    fn watch_pointer(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut away_since: Option<Instant> = None;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let Ok(inside) =
                    this.update(cx, |hud, _| hud.applied.is_some_and(native::pointer_in))
                else {
                    break;
                };
                if inside {
                    away_since = None;
                    continue;
                }
                if away_since.get_or_insert_with(Instant::now).elapsed()
                    >= Duration::from_millis(500)
                {
                    let _ = this.update(cx, |hud, cx| {
                        hud.hovered = false;
                        cx.notify();
                    });
                    break;
                }
            }
        })
        .detach();
    }

    fn wings(&self, cx: &mut Context<Self>) -> (AnyElement, AnyElement) {
        let state = self.state.read(cx);
        let record = rgb(0xff453a);
        match &state.phase {
            Phase::Countdown { until, .. } => {
                let left = until.saturating_duration_since(Instant::now()).as_secs() + 1;
                (
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(left.to_string())
                        .into_any_element(),
                    self.icon_button("cancel", "✕", white(0.7), |s, cx| s.cancel_recording(cx))
                        .into_any_element(),
                )
            }
            Phase::Starting => (
                label("Starting", white(0.6)).into_any_element(),
                div().into_any_element(),
            ),
            Phase::Recording { .. } => {
                let paused = state.phase.is_paused();
                let secs = state.phase.recorded().as_secs();
                let status = if paused {
                    icon("pause", 10., white(0.6)).into_any_element()
                } else {
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(record)
                        .with_animation(
                            "pulse",
                            Animation::new(Duration::from_millis(1400))
                                .repeat()
                                .with_easing(pulsating_between(0.35, 1.)),
                            |d, t| d.opacity(t),
                        )
                        .into_any_element()
                };
                (
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(status)
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if paused { white(0.55) } else { white(0.92) })
                                .child(format_elapsed(secs)),
                        )
                        .into_any_element(),
                    div()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .child(self.svg_button(
                            "pause",
                            if paused { "play" } else { "pause" },
                            |s, cx| s.toggle_pause(cx),
                        ))
                        .child(self.stop_button())
                        .into_any_element(),
                )
            }
            Phase::Stopping { end } => (
                label(
                    match end {
                        End::Save => "Saving",
                        End::Discard => "Discarding",
                        End::Restart => "Restarting",
                    },
                    white(0.6),
                )
                .into_any_element(),
                div().into_any_element(),
            ),
            Phase::Saved { .. } => (
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().size(px(8.)).rounded_full().bg(rgb(0x30d158)))
                    .child(label("Saved", white(0.92)))
                    .into_any_element(),
                self.icon_button("dismiss", "✕", white(0.7), |s, cx| s.dismiss_saved(cx))
                    .into_any_element(),
            ),
            Phase::Idle => (
                div()
                    .flex()
                    .items_center()
                    .child(div().size(px(8.)).rounded_full().bg(rgb(0xffd60a)))
                    .into_any_element(),
                self.icon_button("dismiss", "✕", white(0.7), |s, cx| s.dismiss_saved(cx))
                    .into_any_element(),
            ),
        }
    }

    fn drawer(&self, height: f64, cx: &mut Context<Self>) -> Div {
        let state = self.state.read(cx);
        let base = div()
            .h(px(height as f32))
            .w_full()
            .px(px(10.))
            .pb(px(6.))
            .flex()
            .items_center()
            .gap(px(6.));

        if let Some(notice) = state.notice.clone() {
            return base
                .child(
                    div()
                        .flex_1()
                        .text_size(px(11.5))
                        .text_color(white(0.85))
                        .line_clamp(4)
                        .child(notice.message),
                )
                .when_some(notice.action, |d, (label, url)| {
                    d.child(drawer_button("notice-action", label, true, move |_, _| {
                        native::open(url)
                    }))
                });
        }

        if matches!(state.phase, Phase::Recording { .. }) {
            return self.controls(base, cx);
        }

        let Phase::Saved { path, .. } = &state.phase else {
            return base;
        };
        let name: SharedString = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into();
        let (reveal, copy, edit, play) = (path.clone(), path.clone(), path.clone(), path.clone());
        let transcript = self.transcript_button(path.clone(), cx);
        let state = self.state.clone();
        base.child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.5))
                .text_color(white(0.7))
                .truncate()
                .child(name),
        )
        .child(
            div()
                .id("reveal")
                .size(px(22.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .bg(white(0.12))
                .hover(|s| s.bg(white(0.2)))
                .cursor_pointer()
                .on_click(move |_, _, _| native::reveal_in_finder(&reveal))
                .child(icon("folder", 12., white(0.85))),
        )
        .child(drawer_button("copy", "Copy", false, move |_, _| {
            native::copy_file_to_pasteboard(&copy)
        }))
        .children(transcript)
        .child(drawer_button("edit", "Edit", false, move |_, cx| {
            state.update(cx, |s, cx| s.open_editor(edit.clone(), cx))
        }))
        .child(drawer_button("play", "Open", true, move |_, _| {
            native::open(&play)
        }))
    }

    /// Copy transcript: shows progress while transcribing, then copies Markdown.
    fn transcript_button(
        &self,
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.state.read(cx);
        if state.settings.transcription == Transcription::Off {
            return None;
        }
        let label = match state.transcripts.get(&path)? {
            TranscriptState::Working => {
                return Some(
                    div()
                        .px(px(8.))
                        .py(px(3.))
                        .text_size(px(11.5))
                        .text_color(white(0.55))
                        .child("Transcribing…")
                        .into_any_element(),
                );
            }
            TranscriptState::Failed(_) => return None,
            TranscriptState::Ready(t) if t.is_empty() => {
                return Some(
                    div()
                        .px(px(8.))
                        .py(px(3.))
                        .text_size(px(11.5))
                        .text_color(white(0.55))
                        .child("No speech")
                        .into_any_element(),
                );
            }
            TranscriptState::Ready(_) => {
                if self
                    .copied_at
                    .is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
                {
                    "Copied"
                } else {
                    "Copy transcript"
                }
            }
        };
        let hud = cx.entity().downgrade();
        let state = self.state.clone();
        Some(
            drawer_button("transcript", label, false, move |_, cx| {
                if state.read(cx).copy_transcript(&path) {
                    let _ = hud.update(cx, |this, cx| {
                        this.copied_at = Some(Instant::now());
                        cx.notify();
                        cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(1550))
                                .await;
                            let _ = this.update(cx, |_, cx| cx.notify());
                        })
                        .detach();
                    });
                }
            })
            .into_any_element(),
        )
    }

    /// Restart (with a confirming second click) and Discard, under a recording's bar.
    fn controls(&self, base: Div, cx: &mut Context<Self>) -> Div {
        let confirming = self
            .confirm_restart_until
            .is_some_and(|t| Instant::now() < t);
        let hud = cx.entity().downgrade();
        let state = self.state.clone();
        let restart = div()
            .id("restart")
            .flex()
            .items_center()
            .gap(px(5.))
            .px(px(8.))
            .py(px(3.))
            .rounded(px(6.))
            .text_size(px(11.5))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .when(confirming, |d| {
                d.bg(rgb(0xff453a)).hover(|s| s.opacity(0.9))
            })
            .when(!confirming, |d| {
                d.bg(white(0.12)).hover(|s| s.bg(white(0.2)))
            })
            .on_click(move |_, _, cx| {
                if confirming {
                    state.update(cx, |s, cx| s.restart_recording(cx));
                    return;
                }
                let _ = hud.update(cx, |this, cx| {
                    this.confirm_restart_until = Some(Instant::now() + Duration::from_secs(3));
                    cx.notify();
                    // Put the label back once the confirmation window has passed.
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(3050))
                            .await;
                        let _ = this.update(cx, |_, cx| cx.notify());
                    })
                    .detach();
                });
            })
            .child(icon("restart", 11., white(0.9)))
            .child(if confirming { "Restart?" } else { "Restart" });
        let state = self.state.clone();
        base.justify_center().child(restart).child(drawer_button(
            "discard",
            "Discard",
            false,
            move |_, cx| state.update(cx, |s, cx| s.cancel_recording(cx)),
        ))
    }

    fn stop_button(&self) -> impl IntoElement {
        let state = self.state.clone();
        div()
            .id("stop")
            .size(px(20.))
            .rounded_full()
            .bg(white(0.16))
            .hover(|s| s.bg(white(0.26)))
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .on_click(move |_, _, cx| state.update(cx, |s, cx| s.stop_recording(cx)))
            .child(div().size(px(8.)).rounded(px(2.)).bg(rgb(0xff453a)))
    }

    fn svg_button(
        &self,
        id: &'static str,
        name: &'static str,
        on_click: impl Fn(&mut Annuncia, &mut Context<Annuncia>) + 'static,
    ) -> impl IntoElement {
        let state = self.state.clone();
        div()
            .id(id)
            .size(px(20.))
            .rounded_full()
            .hover(|s| s.bg(white(0.14)))
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .on_click(move |_, _, cx| state.update(cx, |s, cx| on_click(s, cx)))
            .child(icon(name, 10., white(0.85)))
    }

    fn icon_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        color: Hsla,
        on_click: impl Fn(&mut Annuncia, &mut Context<Annuncia>) + 'static,
    ) -> impl IntoElement {
        let state = self.state.clone();
        div()
            .id(id)
            .size(px(20.))
            .rounded_full()
            .hover(|s| s.bg(white(0.14)))
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(11.))
            .text_color(color)
            .on_click(move |_, _, cx| state.update(cx, |s, cx| on_click(s, cx)))
            .child(glyph)
    }
}

fn label(text: &'static str, color: Hsla) -> impl IntoElement {
    div()
        .text_color(color)
        .font_weight(FontWeight::MEDIUM)
        .child(text)
}

fn drawer_button(
    id: &'static str,
    label: impl Into<SharedString>,
    primary: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px(px(8.))
        .py(px(3.))
        .rounded(px(6.))
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .when(primary, |d| {
            d.bg(rgb(crate::ui::ACCENT)).hover(|s| s.opacity(0.9))
        })
        .when(!primary, |d| d.bg(white(0.12)).hover(|s| s.bg(white(0.2))))
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(label.into())
}

/// Shortcut hint shown in the launcher's start button.
pub fn toggle_hint(state: &Annuncia) -> String {
    display_binding(&state.settings.hotkeys.toggle_recording)
}
