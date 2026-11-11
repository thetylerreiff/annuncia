//! The recording HUD: a black island that grows out of the notch (or, on screens without one,
//! a capsule under the menu bar). Status sits left of the camera housing, controls right of it.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, BoxShadow, Context, Div,
    Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, WindowHandle, div, hsla,
    point, prelude::FluentBuilder as _, pulsating_between, px, rgb,
};

use crate::app::{self, Annuncia, End, Notice, Phase, Severity, TranscriptState};
use crate::hotkeys::display_binding;
use crate::models::Transcription;
use crate::native::{self, MenuBarGeometry, OverlayLevel, TopLeftRect};
use crate::ui::icons::icon;
use crate::ui::{Theme, tip};

/// Each side of the notch: the timer on the left, pause and stop on the right.
const WING: f64 = 84.;
/// While the pointer is over a recording's HUD, each side widens for Restart and Discard.
const WING_EXPANDED: f64 = 116.;
/// The inverted corners where the island meets the top edge of the screen.
const EAR: f64 = 10.;
const CAPSULE_WIDTH: f64 = 168.;
const CAPSULE_EXPANDED: f64 = 232.;
const CAPSULE_HEIGHT: f64 = 30.;
const BUTTON: f32 = 24.;
const RECORD: u32 = 0xff453a;
const SAVED_DRAWER: f64 = 38.;
/// The strip under the bar that spells out an armed control ("Click again to discard"). It sits
/// below rather than in the bar, which can't grow or change without moving the controls.
const HINT_STRIP: f64 = 20.;
/// Notice text sets its own line height, so the drawer can be sized before layout.
const NOTICE_LINE: f32 = 15.;
/// The most lines a notice gets, title and error chain included.
const NOTICE_LINES: usize = 6;
const DRAWER_WIDTH: f64 = 370.;

pub struct Hud {
    state: Entity<Annuncia>,
    geometry: MenuBarGeometry,
    applied: Option<TopLeftRect>,
    hovered: bool,
    /// Restart and Discard throw the recording away, so each takes a second click before this
    /// passes.
    armed: Option<(Armed, Instant)>,
    /// When the transcript was last copied, for a moment of "Copied" feedback.
    copied_at: Option<Instant>,
    /// When the saved file was last copied, likewise.
    file_copied_at: Option<Instant>,
}

/// A control that throws the take away, waiting for its second click.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Armed {
    Restart,
    Discard,
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
    let rect = frame_for(&geometry, 0., 0., false);
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
                armed: None,
                copied_at: None,
                file_copied_at: None,
            }
        })
    })
    .inspect_err(|err| eprintln!("annuncia: hud window: {err}"))
    .ok()
}

/// `min_width` makes room for a drawer wider than the bar (the saved file's three buttons).
/// `expanded` widens the bar for a recording's hover controls.
fn frame_for(g: &MenuBarGeometry, drawer: f64, min_width: f64, expanded: bool) -> TopLeftRect {
    match g.notch {
        Some(notch) => {
            let wing = if expanded { WING_EXPANDED } else { WING };
            let w = (notch.w + 2. * wing).max(min_width) + 2. * EAR;
            TopLeftRect {
                x: notch.x + notch.w / 2. - w / 2.,
                y: notch.y,
                w,
                h: notch.h + drawer,
            }
        }
        None => {
            let capsule = if expanded {
                CAPSULE_EXPANDED
            } else {
                CAPSULE_WIDTH
            };
            let w = capsule.max(min_width);
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
        let theme = Theme::for_window(window);
        let state = self.state.read(cx);
        let recording = matches!(state.phase, Phase::Recording { .. });
        let expanded = recording && self.hovered;
        // A notice sits above the saved file's buttons rather than hiding them.
        let notice = state.notice.clone();
        let saved = matches!(state.phase, Phase::Saved { .. });
        let notice_height = notice.as_ref().map_or(0., notice_height);
        let min_width = if notice.is_some() || saved {
            DRAWER_WIDTH
        } else {
            0.
        };
        if !recording {
            self.armed = None;
        }
        let hint = self.bar_hint(theme);
        let drawer_height = notice_height
            + if saved { SAVED_DRAWER } else { 0. }
            + if hint.is_some() { HINT_STRIP } else { 0. };
        let rect = frame_for(&self.geometry, drawer_height, min_width, expanded);
        if self.applied != Some(rect) {
            self.applied = Some(rect);
            native::set_frame(window, cx, rect);
        }

        let (left, right) = self.wings(expanded, theme, cx);
        let notch = self.geometry.notch;
        let bar_height = px(notch.map_or(CAPSULE_HEIGHT, |n| n.h) as f32);

        // Around a notch the controls hug the camera housing; in a capsule they sit at the ends.
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
                    .map(|d| match notch {
                        Some(_) => d.justify_end().pr(px(2.)),
                        None => d.pl(px(10.)),
                    })
                    .child(left),
            )
            .when_some(notch, |d, n| d.child(div().w(px(n.w as f32)).h_full()))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .map(|d| match notch {
                        Some(_) => d.pl(px(4.)),
                        None => d.justify_end().pr(px(4.)),
                    })
                    .child(right),
            );

        let drawer = (drawer_height > 0.).then(|| {
            div()
                .w_full()
                .flex()
                .flex_col()
                .children(hint.map(|(text, color)| {
                    div()
                        .h(px(HINT_STRIP as f32))
                        .w_full()
                        .px(px(10.))
                        .pb(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(hint_label(text, color))
                }))
                .children(notice.map(|n| self.notice_drawer(n, notice_height, cx)))
                .children(saved.then(|| self.drawer(SAVED_DRAWER, cx)))
        });
        let body = div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .bg(gpui::black())
            .map(|d| match notch {
                Some(_) => d.rounded_bl(px(16.)).rounded_br(px(16.)),
                None => d.rounded(px(15.)),
            })
            .child(bar)
            .children(drawer);
        let ear = |name| icon(name, EAR as f32, gpui::black());
        let state = self.state.clone();
        div()
            .id("hud")
            .size_full()
            .flex()
            .text_color(white(0.92))
            .text_size(px(12.))
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
            .when(notch.is_some(), |d| d.child(ear("ear-left")))
            .child(body)
            .when(notch.is_some(), |d| d.child(ear("ear-right")))
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

    /// Words for the strip under the bar: what an armed control will do.
    fn bar_hint(&self, theme: Theme) -> Option<(SharedString, Hsla)> {
        let text = match self.armed()? {
            Armed::Restart => "Click again to restart",
            Armed::Discard => "Click again to discard",
        };
        Some((text.into(), theme.record))
    }

    fn armed(&self) -> Option<Armed> {
        self.armed
            .filter(|(_, until)| Instant::now() < *until)
            .map(|(armed, _)| armed)
    }

    fn wings(
        &self,
        expanded: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> (AnyElement, AnyElement) {
        let state = self.state.read(cx);
        let record = rgb(RECORD);
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
                // Paused, the dot goes hollow and the timer dims.
                let status = if paused {
                    div()
                        .size(px(8.))
                        .flex_none()
                        .rounded_full()
                        .border(px(1.5))
                        .border_color(record)
                        .into_any_element()
                } else {
                    div()
                        .size(px(8.))
                        .flex_none()
                        .rounded_full()
                        .bg(record)
                        .shadow(vec![BoxShadow {
                            color: hsla(3. / 360., 1., 0.61, 0.8),
                            offset: point(px(0.), px(0.)),
                            blur_radius: px(10.),
                            spread_radius: px(0.),
                        }])
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
                        .gap(px(8.))
                        .when(expanded, |d| d.child(self.armed_button(Armed::Restart, cx)))
                        .child(status)
                        .child(
                            div()
                                .min_w(px(34.))
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(if paused { white(0.5) } else { gpui::white() })
                                .child(format_elapsed(secs)),
                        )
                        .into_any_element(),
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(self.svg_button(
                            "pause",
                            if paused { "play" } else { "pause" },
                            gpui::white(),
                            |s, cx| s.toggle_pause(cx),
                        ))
                        .child(self.stop_button())
                        .when(expanded, |d| d.child(self.armed_button(Armed::Discard, cx)))
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
            Phase::Idle => {
                let error = state
                    .notice
                    .as_ref()
                    .is_some_and(|n| n.severity == Severity::Error);
                (
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().size(px(8.)).rounded_full().bg(if error {
                            theme.record
                        } else {
                            theme.warn
                        }))
                        .child(label(if error { "Error" } else { "Notice" }, white(0.92)))
                        .into_any_element(),
                    self.icon_button("dismiss", "✕", white(0.7), |s, cx| s.dismiss_saved(cx))
                        .into_any_element(),
                )
            }
        }
    }

    /// The notice: a bold title for errors, the message, and the error chain muted below it.
    fn notice_drawer(&self, notice: Notice, height: f64, cx: &mut Context<Self>) -> Div {
        let (message_lines, detail_lines) = notice_lines(&notice);
        // Idle or saved, the bar's ✕ dismisses it; otherwise that end of the bar holds the
        // recording's controls, and an error waits to be dismissed.
        let own_dismiss = notice.severity == Severity::Error
            && !matches!(self.state.read(cx).phase, Phase::Idle | Phase::Saved { .. });
        div()
            .h(px(height as f32))
            .w_full()
            .px(px(10.))
            .pb(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .text_size(px(11.5))
                    .line_height(px(NOTICE_LINE))
                    .when_some(notice.title, |d, title| {
                        d.child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(gpui::white())
                                .truncate()
                                .child(title),
                        )
                    })
                    .child(
                        div()
                            .text_color(white(0.85))
                            .line_clamp(message_lines)
                            .child(notice.message),
                    )
                    .when_some(notice.detail, |d, detail| {
                        d.child(
                            div()
                                .text_color(white(0.45))
                                .line_clamp(detail_lines)
                                .child(detail),
                        )
                    }),
            )
            .when_some(notice.action, |d, (label, url)| {
                d.child(drawer_button("notice-action", label, true, move |_, _| {
                    native::open(url)
                }))
            })
            .when(own_dismiss, |d| {
                d.child(
                    self.icon_button("notice-dismiss", "✕", white(0.7), |s, cx| {
                        s.dismiss_notice(cx)
                    }),
                )
            })
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
        let hud = cx.entity().downgrade();
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
                // Only while hovered: the pointer leaving this window isn't reported, so a
                // tooltip shown then would stay.
                .when(self.hovered, |d| d.tooltip(tip("Show in Finder")))
                .child(icon("folder", 12., white(0.85))),
        )
        // Copy leads: stop, then paste the file wherever it's going.
        .child(drawer_button(
            "copy",
            if just_copied(self.file_copied_at) {
                "Copied"
            } else {
                "Copy"
            },
            true,
            move |_, cx| {
                native::copy_file_to_pasteboard(&copy);
                let _ = hud.update(cx, |this, cx| {
                    this.file_copied_at = Some(Instant::now());
                    this.redraw_after_copied(cx);
                });
            },
        ))
        .children(transcript)
        .child(drawer_button("edit", "Edit", false, move |_, cx| {
            state.update(cx, |s, cx| s.open_editor(edit.clone(), cx))
        }))
        .child(drawer_button("play", "Open", false, move |_, _| {
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
            TranscriptState::Failed(err) => {
                return Some(
                    div()
                        .id("transcript-failed")
                        .px(px(8.))
                        .py(px(3.))
                        .text_size(px(11.5))
                        .text_color(white(0.55))
                        .when(self.hovered, |d| d.tooltip(tip(err.clone())))
                        .child("Transcript failed")
                        .into_any_element(),
                );
            }
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
                if just_copied(self.copied_at) {
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
                        this.redraw_after_copied(cx);
                    });
                }
            })
            .into_any_element(),
        )
    }

    /// Redraws now, and again once "Copied" has had its moment.
    fn redraw_after_copied(&self, cx: &mut Context<Self>) {
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1550))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// Restart and Discard throw the take away, so the first click arms one (it turns red and
    /// the strip under the bar says so) and a second click within three seconds goes ahead.
    fn armed_button(&self, which: Armed, cx: &mut Context<Self>) -> impl IntoElement {
        let confirming = self.armed() == Some(which);
        let hud = cx.entity().downgrade();
        let state = self.state.clone();
        let (id, name) = match which {
            Armed::Restart => ("restart", "restart"),
            Armed::Discard => ("discard", "x"),
        };
        div()
            .id(id)
            .size(px(BUTTON))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .when(confirming, |d| {
                d.bg(hsla(3. / 360., 1., 0.61, 0.22))
                    .hover(|s| s.bg(hsla(3. / 360., 1., 0.61, 0.32)))
            })
            .when(!confirming, |d| d.hover(|s| s.bg(white(0.12))))
            .on_click(move |_, _, cx| {
                if confirming {
                    state.update(cx, |s, cx| match which {
                        Armed::Restart => s.restart_recording(cx),
                        Armed::Discard => s.cancel_recording(cx),
                    });
                    return;
                }
                let _ = hud.update(cx, |this, cx| {
                    this.armed = Some((which, Instant::now() + Duration::from_secs(3)));
                    cx.notify();
                    // Fold the strip away once the confirmation window has passed.
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(3050))
                            .await;
                        let _ = this.update(cx, |_, cx| cx.notify());
                    })
                    .detach();
                });
            })
            .child(icon(
                name,
                14.,
                if confirming {
                    rgb(RECORD).into()
                } else {
                    white(0.5)
                },
            ))
    }

    fn stop_button(&self) -> impl IntoElement {
        let state = self.state.clone();
        div()
            .id("stop")
            .size(px(BUTTON))
            .flex_none()
            .rounded(px(6.))
            .hover(|s| s.bg(white(0.12)))
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .on_click(move |_, _, cx| state.update(cx, |s, cx| s.stop_recording(cx)))
            .child(div().size(px(8.5)).rounded(px(2.)).bg(rgb(RECORD)))
    }

    fn svg_button(
        &self,
        id: &'static str,
        name: &'static str,
        color: Hsla,
        on_click: impl Fn(&mut Annuncia, &mut Context<Annuncia>) + 'static,
    ) -> impl IntoElement {
        let state = self.state.clone();
        div()
            .id(id)
            .size(px(BUTTON))
            .flex_none()
            .rounded(px(6.))
            .hover(|s| s.bg(white(0.12)))
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .on_click(move |_, _, cx| state.update(cx, |s, cx| on_click(s, cx)))
            .child(icon(name, 14., color))
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

/// Whether a copy made at `at` still says "Copied".
fn just_copied(at: Option<Instant>) -> bool {
    at.is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
}

/// How many lines the message and the error chain get. Text can't be measured before the window
/// is sized, so this guesses from its length (about 6 px a character).
fn notice_lines(notice: &Notice) -> (usize, usize) {
    fn lines(text: &str, per_line: usize) -> usize {
        text.chars().count().div_ceil(per_line).max(1)
    }
    let per_line = if notice.action.is_some() { 36 } else { 50 };
    let title = usize::from(notice.title.is_some());
    let detail = notice
        .detail
        .as_ref()
        .map_or(0, |d| lines(d, per_line).min(2));
    let message = lines(&notice.message, per_line).min(NOTICE_LINES - title - detail);
    (message, detail)
}

fn notice_height(notice: &Notice) -> f64 {
    let (message, detail) = notice_lines(notice);
    let title = usize::from(notice.title.is_some());
    (title + message + detail) as f64 * f64::from(NOTICE_LINE) + 18.
}

/// The words in the strip under the bar.
fn hint_label(text: SharedString, color: Hsla) -> impl IntoElement {
    div()
        .min_w_0()
        .truncate()
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(text)
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
