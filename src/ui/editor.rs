//! The editor that opens after a recording: a preview, and a waveform timeline where the
//! recording can be split into clips, each with its own trim handles, and clips deleted. Saving re-encodes the kept
//! parts so every cut is frame-exact, then replaces the recording (the original goes to the
//! Trash).

use std::cell::Cell;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _, Pixels, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions, canvas, div, hsla,
    prelude::FluentBuilder as _, px, relative, size, surface,
};
use objc2::MainThreadMarker;

use crate::app::{Annuncia, TranscriptState};
use crate::cleanup::{self, Cleanup};
use crate::edit::Edit;
use crate::media::{self, Encode, Source};
use crate::models::Transcription;
use crate::player::Player;
use crate::speech;
use crate::transcript::{Piece, Transcript, timestamp};
use crate::ui::hud::format_elapsed;
use crate::ui::icons::icon;
use crate::ui::transcript_view::{self, dithered_note, paragraph_of};
use crate::ui::{Theme, button, keycap};

/// Waveform bars across the timeline.
const BARS: usize = 220;
/// Trim handles sit just inside their clip's kept part, so the two at a split read as `][`.
const HANDLE_WIDTH: f32 = 12.;
/// Extra reach (in points) around a handle for grabbing it.
const HANDLE_SLOP: f32 = 4.;
const TIMELINE_HEIGHT: f32 = 72.;
/// Undo steps kept; the oldest go first.
const UNDO_LIMIT: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    /// A clip's start or end handle.
    Start(usize),
    End(usize),
    Scrub,
}

/// A state of the edit that undo returns to.
struct Snapshot {
    edit: Edit,
    selected: Option<usize>,
}

pub struct Editor {
    state: Entity<Annuncia>,
    path: PathBuf,
    focus: FocusHandle,
    player: Player,
    /// `None` until the file's length and waveform have loaded.
    edit: Option<Edit>,
    waveform: Vec<f32>,
    playhead: f64,
    /// Selected clip.
    selected: Option<usize>,
    drag: Option<Drag>,
    timeline: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Export progress in thousandths while saving.
    saving: Option<Arc<AtomicU32>>,
    error: Option<SharedString>,
    /// "Copied" feedback on the transcript button.
    copied: bool,
    transcript_scroll: ScrollHandle,
    /// Stretches without speech, from the audio; where pauses get trimmed.
    silences: Vec<Range<f64>>,
    /// What the last clean up did, shown in the toolbar.
    cleanup_note: Option<SharedString>,
    /// The edit has been changed by hand, so automatic clean up leaves it alone.
    touched: bool,
    /// Automatic clean up ran; `true` once it included filler words.
    auto_cleaned: Option<bool>,
    /// The paragraph the transcript last scrolled to while following playback.
    followed_paragraph: Option<usize>,
    /// Earlier states of the edit for ⌘Z, oldest first, and the ones it undid for ⇧⌘Z.
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The edit as a handle drag found it, recorded as one undo step when the drag ends.
    drag_before: Option<Snapshot>,
}

/// `focus` must be false unless the app is active (see `app::when_active`).
pub fn open(
    state: Entity<Annuncia>,
    path: PathBuf,
    focus: bool,
    cx: &mut App,
) -> Option<WindowHandle<Editor>> {
    let mtm = MainThreadMarker::new()?;
    let bounds = Bounds::centered(None, size(px(1180.), px(760.)), cx);
    let title = path.file_stem().map_or("Edit recording".into(), |s| {
        s.to_string_lossy().into_owned()
    });
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(14.), px(14.))),
        }),
        window_background: WindowBackgroundAppearance::Opaque,
        window_min_size: Some(size(px(820.), px(520.))),
        focus,
        show: true,
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let weak = state.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = weak.update(cx, |s, _| s.editor_closed());
            true
        });
        cx.new(|cx| Editor::new(state, path, mtm, window, cx))
    })
    .inspect_err(|err| eprintln!("annuncia: editor window: {err}"))
    .ok()
}

impl Editor {
    fn new(
        state: Entity<Annuncia>,
        path: PathBuf,
        mtm: MainThreadMarker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        // The transcript arrives from the app while the editor is open (and may let automatic
        // clean up add filler words).
        cx.observe(&state, |this: &mut Self, _, cx| {
            this.auto_clean_up(cx);
            cx.notify();
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus);

        let player = Player::new(&path, mtm);
        // Show the first frame right away.
        player.seek(0., true);

        let load = path.clone();
        let task = cx.background_spawn(async move {
            let duration = media::duration(&load);
            // One read of the audio feeds both the waveform and the pause map.
            let pcm = media::audio(&load, speech::SAMPLE_RATE as f64).unwrap_or_default();
            let waveform = media::peaks(&pcm, BARS);
            let silences = speech::silences(&pcm, 0.3);
            (duration, waveform, silences)
        });
        cx.spawn(async move |this, cx| {
            let (duration, waveform, silences) = task.await;
            let _ = this.update(cx, |this, cx| {
                if duration.is_finite() && duration > 0. {
                    this.edit = Some(Edit::new(duration));
                } else {
                    this.error = Some("Couldn't read this recording".into());
                }
                this.waveform = waveform;
                this.silences = silences;
                this.auto_clean_up(cx);
                cx.notify();
            });
        })
        .detach();

        // Redraw tick: new frames, the moving playhead, skipping cut sections, save progress.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        Self {
            state,
            path,
            focus,
            player,
            edit: None,
            waveform: Vec::new(),
            playhead: 0.,
            selected: None,
            drag: None,
            timeline: Rc::new(Cell::new(None)),
            saving: None,
            error: None,
            copied: false,
            transcript_scroll: ScrollHandle::new(),
            silences: Vec::new(),
            cleanup_note: None,
            touched: false,
            auto_cleaned: None,
            followed_paragraph: None,
            undo: Vec::new(),
            redo: Vec::new(),
            drag_before: None,
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = self.player.poll();
        if self.player.is_playing() && self.drag.is_none() {
            let t = self.player.time();
            match self.edit.as_ref().map(|e| e.playable_from(t)) {
                // Playing into a cut section (or before the start handle): jump past it.
                Some(Some(next)) if next > t + 0.05 => self.player.seek(next, true),
                Some(None) => {
                    self.player.pause();
                    if let Some(end) = self.edit.as_ref().map(Edit::kept_end) {
                        self.seek(end, true);
                    }
                    changed = true;
                }
                _ => {}
            }
            if (t - self.playhead).abs() > 1e-3 {
                self.playhead = t;
                changed = true;
            }
        }
        if self.saving.is_some() {
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = &self.edit else { return };
        if self.player.is_playing() {
            self.player.pause();
        } else {
            match edit.playable_from(self.playhead) {
                Some(t) if t > self.playhead + 0.05 => self.seek(t, true),
                // At the end: start over from the first kept moment.
                None => {
                    if let Some(t) = edit.playable_from(0.) {
                        self.seek(t, true);
                    }
                }
                _ => {}
            }
            self.player.play();
        }
        cx.notify();
    }

    fn seek(&mut self, t: f64, exact: bool) {
        self.playhead = t;
        self.player.seek(t, exact);
    }

    fn split(&mut self, cx: &mut Context<Self>) {
        self.touched = true;
        let t = self.playhead;
        let before = self.snapshot();
        if let Some(edit) = &mut self.edit
            && edit.split(t)
        {
            self.selected = Some(edit.clip_at(t));
            self.record(before);
            cx.notify();
        }
    }

    fn toggle_selected(&mut self, cx: &mut Context<Self>) {
        self.touched = true;
        let before = self.snapshot();
        let Some(edit) = &mut self.edit else { return };
        let i = self.selected.unwrap_or_else(|| edit.clip_at(self.playhead));
        // Cutting the last kept clip would leave nothing to save.
        let kept_clips = edit.clips().iter().filter(|c| !c.removed).count();
        if kept_clips == 1 && !edit.is_removed(i) {
            return;
        }
        edit.toggle_removed(i);
        self.selected = Some(i);
        self.record(before);
        cx.notify();
    }

    fn transcript(&self, cx: &App) -> Option<Arc<Transcript>> {
        match self.state.read(cx).transcripts.get(&self.path)? {
            TranscriptState::Ready(t) => Some(t.clone()),
            _ => None,
        }
    }

    /// Replaces the edit with automatic cuts: dead air, long pauses, and (when `fillers` and a
    /// transcript is ready) filler words.
    fn apply_clean_up(&mut self, fillers: bool, cx: &mut Context<Self>) {
        let transcript = fillers.then(|| self.transcript(cx)).flatten();
        let before = self.snapshot();
        let Some(edit) = &mut self.edit else { return };
        let plan = cleanup::plan(
            edit.duration,
            &self.silences,
            transcript.as_ref().map(|t| t.pieces.as_slice()),
        );
        cleanup::apply(&plan, edit);
        let spots =
            plan.cuts.len() + usize::from(plan.start > 0.) + usize::from(plan.end < edit.duration);
        self.cleanup_note = Some(if spots == 0 {
            "Nothing to clean up".into()
        } else {
            format!(
                "Cleaned up {spots} {}, {} shorter",
                if spots == 1 { "spot" } else { "spots" },
                timestamp(plan.saved(edit.duration))
            )
            .into()
        });
        self.selected = None;
        self.record(before);
        cx.notify();
    }

    /// The Clean up button: everything it can find, replacing the current edit.
    fn clean_up(&mut self, cx: &mut Context<Self>) {
        self.touched = true;
        self.apply_clean_up(true, cx);
    }

    /// Runs the clean up chosen in Settings once the recording has loaded, and again with
    /// filler words when the transcript arrives, as long as the edit hasn't been touched.
    fn auto_clean_up(&mut self, cx: &mut Context<Self>) {
        if self.touched || self.edit.is_none() {
            return;
        }
        let with_fillers = match self.state.read(cx).settings.cleanup {
            Cleanup::Off => return,
            Cleanup::Pauses => false,
            Cleanup::PausesAndFillers => self.transcript(cx).is_some(),
        };
        if self.auto_cleaned.is_none() || (with_fillers && self.auto_cleaned == Some(false)) {
            self.auto_cleaned = Some(with_fillers);
            self.apply_clean_up(with_fillers, cx);
        }
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.touched = true;
        self.cleanup_note = None;
        let before = self.snapshot();
        if let Some(edit) = &mut self.edit {
            edit.reset();
        }
        self.selected = None;
        self.record(before);
        cx.notify();
    }

    fn snapshot(&self) -> Option<Snapshot> {
        Some(Snapshot {
            edit: self.edit.clone()?,
            selected: self.selected,
        })
    }

    /// Makes `before` the state ⌘Z returns to, if the edit has changed since. A new change
    /// drops whatever ⇧⌘Z would have redone.
    fn record(&mut self, before: Option<Snapshot>) {
        let Some(before) = before else { return };
        if self.edit.as_ref() == Some(&before.edit) {
            return;
        }
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
    }

    /// ⌘Z, or ⇧⌘Z when `redo`.
    fn undo(&mut self, redo: bool, cx: &mut Context<Self>) {
        if self.drag.is_some() {
            return;
        }
        let Some(edit) = &mut self.edit else { return };
        let (from, to) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let Some(snapshot) = from.pop() else { return };
        to.push(Snapshot {
            edit: std::mem::replace(edit, snapshot.edit),
            selected: std::mem::replace(&mut self.selected, snapshot.selected),
        });
        // Undoing automatic clean up mustn't let it run again.
        self.touched = true;
        self.cleanup_note = None;
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.player.pause();
        self.state.update(cx, |s, _| s.editor_closed());
        window.remove_window();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.edit else { return };
        if self.saving.is_some() {
            return;
        }
        if !edit.is_edited() {
            self.close(window, cx);
            return;
        }
        let kept = edit.kept();
        if kept.is_empty() {
            self.error = Some("Everything is cut. Restore a section to save.".into());
            cx.notify();
            return;
        }
        self.player.pause();
        self.error = None;
        let progress = Arc::new(AtomicU32::new(0));
        self.saving = Some(progress.clone());
        let codec = self.state.read(cx).settings.codec;
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            let stem = path
                .file_stem()
                .map_or("Annuncia".into(), |s| s.to_string_lossy().into_owned());
            let temp = path.with_file_name(format!("{stem} (saving).mp4"));
            let _ = std::fs::remove_file(&temp);
            media::export(
                &[Source {
                    path: path.clone(),
                    ranges: Some(kept),
                }],
                &temp,
                Encode::Reencode(codec),
                |p| progress.store((p * 1000.) as u32, Ordering::Relaxed),
            )
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&temp);
            })?;
            // Nothing is deleted outright: the unedited recording goes to the Trash.
            let _ = crate::native::trash(&path);
            std::fs::rename(&temp, &path)?;
            anyhow::Ok(path)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(path) => {
                    this.state.update(cx, |s, cx| s.edit_saved(path, cx));
                    window.remove_window();
                }
                Err(err) => {
                    this.saving = None;
                    this.error = Some(format!("{err:#}").into());
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        // ⌘ chords never fall through to the single-letter tools below (⌘C isn't Clean up).
        if keystroke.modifiers.platform {
            match keystroke.key.as_str() {
                "s" => self.save(window, cx),
                "z" => self.undo(keystroke.modifiers.shift, cx),
                _ => {}
            }
            return;
        }
        let step = if keystroke.modifiers.shift { 5. } else { 1. };
        match keystroke.key.as_str() {
            "space" => self.toggle_play(cx),
            "s" => self.split(cx),
            "backspace" | "delete" => self.toggle_selected(cx),
            // Trim the clip under the playhead to it.
            "c" => self.clean_up(cx),
            "i" | "o" => {
                self.touched = true;
                let t = self.playhead;
                let before = self.snapshot();
                if let Some(edit) = &mut self.edit {
                    let i = edit.clip_at(t);
                    if keystroke.key == "i" {
                        edit.set_start(i, t);
                    } else {
                        edit.set_end(i, t);
                    }
                    self.selected = Some(i);
                }
                self.record(before);
                cx.notify();
            }
            "left" | "right" => {
                let Some(duration) = self.edit.as_ref().map(|e| e.duration) else {
                    return;
                };
                let delta = if keystroke.key == "left" { -step } else { step };
                self.seek((self.playhead + delta).clamp(0., duration), true);
                cx.notify();
            }
            "escape" => self.close(window, cx),
            "enter" => self.save(window, cx),
            _ => {}
        }
    }

    /// Timeline time under a window x position.
    fn time_at(&self, x: Pixels) -> Option<f64> {
        let bounds = self.timeline.get()?;
        let edit = self.edit.as_ref()?;
        let width = f32::from(bounds.size.width).max(1.);
        let fraction = ((f32::from(x) - f32::from(bounds.origin.x)) / width).clamp(0., 1.);
        Some(f64::from(fraction) * edit.duration)
    }

    fn x_of(&self, t: f64) -> Option<f32> {
        let bounds = self.timeline.get()?;
        let edit = self.edit.as_ref()?;
        Some(f32::from(bounds.origin.x) + f32::from(bounds.size.width) * (t / edit.duration) as f32)
    }

    /// The handle under window x position `x`, if any.
    fn handle_at(&self, x: f32) -> Option<Drag> {
        let edit = self.edit.as_ref()?;
        let within = |lo: f32, hi: f32| x >= lo - HANDLE_SLOP && x <= hi + HANDLE_SLOP;
        edit.clips().iter().enumerate().find_map(|(i, clip)| {
            if clip.removed {
                return None;
            }
            let start = self.x_of(clip.start)?;
            let end = self.x_of(clip.end)?;
            if within(start, start + HANDLE_WIDTH) {
                Some(Drag::Start(i))
            } else if within(end - HANDLE_WIDTH, end) {
                Some(Drag::End(i))
            } else {
                None
            }
        })
    }

    fn on_timeline_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.edit.is_none() {
            return;
        }
        let drag = self
            .handle_at(f32::from(event.position.x))
            .unwrap_or(Drag::Scrub);
        if let Drag::Start(i) | Drag::End(i) = drag {
            self.drag_before = self.snapshot();
            self.selected = Some(i);
            self.touched = true;
        }
        self.drag = Some(drag);
        self.player.pause();
        self.drag_to(event.position.x);
        cx.notify();
    }

    fn drag_to(&mut self, x: Pixels) {
        let (Some(drag), Some(t)) = (self.drag, self.time_at(x)) else {
            return;
        };
        let Some(edit) = &mut self.edit else { return };
        // Show the frame at the handle being dragged.
        let at = match drag {
            Drag::Start(i) => {
                edit.set_start(i, t);
                edit.clips()[i].start
            }
            Drag::End(i) => {
                edit.set_end(i, t);
                edit.clips()[i].end
            }
            Drag::Scrub => {
                self.selected = Some(edit.clip_at(t));
                t
            }
        };
        self.seek(at, false);
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.drag.is_none() {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag();
        } else {
            self.drag_to(event.position.x);
        }
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.drag.is_some() {
            self.end_drag();
            cx.notify();
        }
    }

    fn end_drag(&mut self) {
        self.drag = None;
        let before = self.drag_before.take();
        self.record(before);
        // Land on the exact frame once the pointer lets go.
        self.player.seek(self.playhead, true);
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);

        // A hairline edge keeps the black video frame from melting into the black window.
        let preview = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .rounded(px(16.))
            .overflow_hidden()
            .bg(gpui::black())
            .border_1()
            .border_color(theme.hairline)
            .flex()
            .items_center()
            .justify_center()
            .child(match self.player.frame() {
                Some(frame) => surface(frame)
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                None => dithered_note("Loading…", theme),
            });

        div()
            .id("editor")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .size_full()
            .flex()
            .flex_col()
            .gap(px(16.))
            .pt(px(44.))
            .px(px(24.))
            .pb(px(20.))
            .bg(theme.window)
            .text_color(theme.text)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .gap(px(16.))
                    .child(preview)
                    .child(self.transcript_panel(theme, cx)),
            )
            .child(self.timeline_view(theme, cx))
            .child(self.toolbar(theme, cx))
    }
}

impl Editor {
    fn timeline_view(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(edit) = &self.edit else {
            return div()
                .h(px(TIMELINE_HEIGHT))
                .w_full()
                .rounded(px(14.))
                .border(px(3.))
                .border_color(theme.control_active)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child(self.error.clone().unwrap_or("Reading audio…".into()))
                .into_any_element();
        };
        let duration = edit.duration;
        let at = |t: f64| relative((t / duration).clamp(0., 1.) as f32);
        let span = |a: f64, b: f64| relative(((b - a) / duration).clamp(0., 1.) as f32);
        let accent = theme.accent;
        let record = theme.record;
        let dim = theme.window.opacity(0.72);
        let split = edit.clips().len() > 1;

        let bars = div()
            .absolute()
            .inset_0()
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(1.))
            .children(
                self.waveform
                    .iter()
                    .map(|peak| {
                        let height = (peak.clamp(0., 1.).sqrt() * (TIMELINE_HEIGHT - 20.)).max(2.);
                        div()
                            .flex_1()
                            .h(px(height))
                            .rounded(px(1.))
                            .bg(theme.text_muted.opacity(0.55))
                    })
                    .collect::<Vec<_>>(),
            );

        // One frame per clip over its stretch of the recording, with a small gap at splits.
        let frames = edit.clips().iter().enumerate().map(|(i, clip)| {
            let (lo, hi) = clip.span;
            let len = (hi - lo).max(1e-9);
            let inner = |t: f64| relative(((t - lo) / len).clamp(0., 1.) as f32);
            let selected = split && self.selected == Some(i);
            let frame = div()
                .size_full()
                .relative()
                .rounded(px(12.))
                .overflow_hidden()
                .border(px(3.))
                .border_color(if clip.removed { record } else { accent })
                .when(clip.removed, |d| {
                    d.bg(record.opacity(0.2))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(record)
                        .child("Deleted")
                })
                .when(!clip.removed, |d| {
                    d.when(selected, |d| d.bg(accent.opacity(0.12)))
                        // Trimmed-off ends of the clip.
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left_0()
                                .w(inner(clip.start))
                                .bg(dim),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(inner(clip.end))
                                .right_0()
                                .bg(dim),
                        )
                });
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(at(lo))
                .w(span(lo, hi))
                .when(split, |d| d.px(px(2.)))
                .child(frame)
        });

        // Each kept clip's handles sit just inside its kept part, so a split shows `][`.
        let handle = |t: f64, end: bool| {
            div()
                .absolute()
                .top(px(-3.))
                .bottom(px(-3.))
                .left(at(t))
                .when(end, |d| d.ml(px(-HANDLE_WIDTH)))
                .w(px(HANDLE_WIDTH))
                .rounded(px(5.))
                .bg(accent)
                .cursor_ew_resize()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(2.))
                        .h(px(18.))
                        .rounded(px(1.))
                        .bg(hsla(0., 0., 1., 0.9)),
                )
        };
        let handles = edit
            .clips()
            .iter()
            .filter(|c| !c.removed)
            .flat_map(|c| [handle(c.start, false), handle(c.end, true)]);

        let playhead = div()
            .absolute()
            .top(px(-12.))
            .bottom(px(-12.))
            .left(at(self.playhead))
            .ml(px(-1.))
            .w(px(2.))
            .bg(record)
            .child(
                div()
                    .absolute()
                    .top(px(-4.))
                    .left(px(-4.))
                    .size(px(10.))
                    .rounded_full()
                    .bg(record),
            );

        let bounds = self.timeline.clone();
        div()
            .id("timeline")
            .relative()
            .h(px(TIMELINE_HEIGHT))
            .w_full()
            .mt(px(10.))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_timeline_down))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(14.))
                    .overflow_hidden()
                    .bg(theme.control)
                    .child(bars),
            )
            .children(frames)
            .child(
                canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .children(handles)
            .child(playhead)
            .into_any_element()
    }

    fn toolbar(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let edit = self.edit.as_ref();
        let playing = self.player.is_playing();
        // Position and length in the edited video, not the original.
        let kept = edit.map_or(0., Edit::kept_duration);
        let position = edit.map_or(0., |e| e.edited_time(self.playhead));
        let time = format!(
            "{} / {}",
            format_elapsed(position as u64),
            format_elapsed(kept.round() as u64)
        );
        let selected_removed = edit
            .zip(self.selected)
            .is_some_and(|(e, i)| e.is_removed(i));
        let edited = edit.is_some_and(Edit::is_edited);

        let icon_text =
            |id: &'static str, name: &'static str, label: &'static str, key: &'static str| {
                tool_button(id, name, label, key, theme)
            };

        let save_label: SharedString = match &self.saving {
            Some(p) => format!("Saving {}%", p.load(Ordering::Relaxed) / 10).into(),
            None if edited => "Save".into(),
            None => "Done".into(),
        };

        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .id("play")
                    .size(px(34.))
                    .flex_none()
                    .rounded(px(9.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.control)
                    .hover(|s| s.bg(theme.control_hover))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx)))
                    .child(icon(
                        if playing { "pause" } else { "play" },
                        14.,
                        theme.text,
                    )),
            )
            .child(
                div()
                    .min_w(px(96.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(time),
            )
            .child(
                icon_text("split", "scissors", "Split", "S")
                    .on_click(cx.listener(|this, _, _, cx| this.split(cx))),
            )
            .child(
                icon_text(
                    "cut",
                    "trash",
                    if selected_removed {
                        "Restore"
                    } else {
                        "Delete"
                    },
                    "⌫",
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_selected(cx))),
            )
            .child(
                icon_text("cleanup", "wand", "Clean up", "C")
                    .on_click(cx.listener(|this, _, _, cx| this.clean_up(cx))),
            )
            .when(edited, |d| {
                d.child(
                    button("reset", "Reset", false, theme)
                        .on_click(cx.listener(|this, _, _, cx| this.reset(cx))),
                )
            })
            // The flexible gap holds the latest note or error and gives way first when space
            // runs out, so the buttons keep their full width.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .justify_end()
                    .px(px(4.))
                    .text_size(px(12.))
                    .map(|d| match (self.error.clone(), self.cleanup_note.clone()) {
                        (Some(error), _) => d.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.record)
                                .child(error),
                        ),
                        (None, Some(note)) => d.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text_muted)
                                .child(note),
                        ),
                        (None, None) => d,
                    }),
            )
            .child(
                button("cancel", "Cancel", false, theme)
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            )
            .child(
                button("save", save_label, true, theme)
                    .when(self.saving.is_some(), |b| b.opacity(0.7))
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
    }
}

impl Editor {
    /// The transcript beside the preview: the current word follows playback, a click on a word
    /// or timestamp seeks there, and words in cut parts are dimmed.
    fn transcript_panel(&mut self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let note = |text: SharedString| dithered_note(text, theme);
        let model = state.settings.transcription;
        let body = match state.transcripts.get(&self.path) {
            Some(TranscriptState::Ready(t)) if !t.is_empty() => Ok(t.clone()),
            Some(TranscriptState::Ready(_)) => Err(note("No speech in this recording.".into())),
            Some(TranscriptState::Working) => Err(note("Transcribing…".into())),
            Some(TranscriptState::Failed(err)) => Err(note(err.clone())),
            None if model == Transcription::Off => Err(note(
                "Turn on Transcribe in Settings, under After recording, to see what you said here."
                    .into(),
            )),
            None => Err(note(
                format!(
                    "Download the {} model in Settings to transcribe this recording.",
                    model.model_name()
                )
                .into(),
            )),
        };

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .px(px(14.))
            .pt(px(12.))
            .pb(px(8.))
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Transcript"),
            )
            .children(self.transcript_button(theme, cx));

        let body = match body {
            Err(note) => note,
            Ok(transcript) => {
                let paragraphs = transcript.paragraph_pieces();
                let current = transcript.piece_at(self.playhead);
                let current_paragraph = paragraph_of(&paragraphs, current);
                // Follow playback, a paragraph at a time.
                if self.player.is_playing() && current_paragraph != self.followed_paragraph {
                    if let Some(ix) = current_paragraph {
                        self.transcript_scroll.scroll_to_item(ix);
                    }
                    self.followed_paragraph = current_paragraph;
                }
                let kept = self.edit.as_ref().map(|e| e.kept());
                let is_cut = |piece: &Piece| {
                    let mid = (piece.start + piece.end) / 2.;
                    kept.as_ref()
                        .is_some_and(|k| !k.iter().any(|r| mid >= r.start && mid < r.end))
                };
                transcript_view::paragraphs(
                    &transcript,
                    &paragraphs,
                    current,
                    is_cut,
                    &self.transcript_scroll,
                    theme,
                    cx,
                    |this: &mut Self, t, cx| {
                        this.seek(t, true);
                        cx.notify();
                    },
                )
            }
        };

        div()
            .w(px(300.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(16.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.hairline)
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// Copy transcript, once there's one to copy (the panel body explains the other states).
    fn transcript_button(&self, theme: Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.state.read(cx).transcripts.get(&self.path)? {
            TranscriptState::Ready(t) if t.is_empty() => None,
            TranscriptState::Working | TranscriptState::Failed(_) => None,
            TranscriptState::Ready(_) => Some(
                button(
                    "transcript",
                    if self.copied {
                        "Copied"
                    } else {
                        "Copy transcript"
                    },
                    false,
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.state.read(cx).copy_transcript(&this.path) {
                        this.copied = true;
                        cx.notify();
                        cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(Duration::from_millis(1500))
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                this.copied = false;
                                cx.notify();
                            });
                        })
                        .detach();
                    }
                }))
                .into_any_element(),
            ),
        }
    }
}

/// Toolbar button: icon, label, then its shortcut.
fn tool_button(
    id: &'static str,
    name: &'static str,
    label: &'static str,
    key: &'static str,
    theme: Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .flex_none()
        .whitespace_nowrap()
        .gap(px(6.))
        .px(px(10.))
        .py(px(6.))
        .rounded(px(7.))
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .bg(theme.control)
        .hover(|s| s.bg(theme.control_hover))
        .cursor_pointer()
        .child(icon(name, 14., theme.text))
        .child(label)
        .child(keycap(key, theme.text_muted))
}
