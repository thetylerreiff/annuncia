//! The library: every recording Annuncia has saved, newest first, grouped by day, with quick
//! access to each one's transcript and file. Clicking a recording opens it in a detail view
//! with a player and its transcript.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone as _};
use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, Entity, FocusHandle, Focusable,
    FontWeight, HighlightStyle, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, ObjectFit, ParentElement as _, Pixels, Point, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    StyledText, Subscription, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowOptions, anchored, canvas, deferred, div, hsla, img,
    prelude::FluentBuilder as _, px, relative, size, surface,
};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::NSSearchField;

use crate::app::{self, Annuncia, TranscriptState};
use crate::library::{self, Entry};
use crate::native;
use crate::player::Player;
use crate::transcript::{Transcript, timestamp};
use crate::ui::icons::icon;
use crate::ui::settings_view::{self, SettingsView};
use crate::ui::transcript_view::{self, dithered_note, paragraph_of};
use crate::ui::{Theme, button};

const PADDING: f32 = 24.;
const GAP: f32 = 16.;
const MIN_CARD: f32 = 220.;
const HEADER: f32 = 52.;
const SEARCH_WIDTH: f32 = 260.;
/// The settings cog at the header's right end, beside the search field.
const COG: f32 = 28.;
/// Matches listed per recording before "and N more".
const SHOWN_MATCHES: usize = 3;

pub struct LibraryView {
    state: Entity<Annuncia>,
    focus: FocusHandle,
    /// The library revision `items` reflects.
    revision: Option<u64>,
    items: Vec<Item>,
    /// The recording whose transcript was just copied, for the button's feedback.
    copied: Option<PathBuf>,
    /// The recording open in the detail view.
    detail: Option<Detail>,
    /// Native, since GPUI has no text input; installed once the window exists.
    search: Option<Retained<NSSearchField>>,
    query: String,
    /// Settings, while the window shows it instead of the recordings.
    settings: Option<(Entity<SettingsView>, Subscription)>,
    /// The right-click (or "⋯") menu, while it's open.
    menu: Option<Menu>,
    /// Why the last rename or Trash from the grid didn't happen.
    error: Option<SharedString>,
}

/// A small popover of actions on one or more recordings.
struct Menu {
    paths: Vec<PathBuf>,
    /// Where its top-left corner goes, in window coordinates.
    at: Point<Pixels>,
    /// `None` draws a separator.
    actions: Vec<Option<Action>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Open,
    Edit,
    CopyFile,
    CopyTranscript,
    Transcribe,
    TranscribeAgain,
    Reveal,
    Rename,
    Trash,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Action::Open => "Open",
            Action::Edit => "Edit",
            Action::CopyFile => "Copy file",
            Action::CopyTranscript => "Copy transcript",
            Action::Transcribe => "Transcribe",
            Action::TranscribeAgain => "Transcribe again",
            Action::Reveal => "Show in Finder",
            Action::Rename => "Rename…",
            Action::Trash => "Move to Trash",
        }
    }
}

struct Detail {
    path: PathBuf,
    player: Player,
    playhead: f64,
    /// The scrubber's bounds, measured as it paints.
    bar: Rc<Cell<Option<Bounds<Pixels>>>>,
    scrubbing: bool,
    transcript_scroll: ScrollHandle,
    /// The paragraph the transcript last scrolled to while following playback.
    followed_paragraph: Option<usize>,
    /// Why the last rename or Trash didn't happen.
    error: Option<SharedString>,
}

struct Item {
    entry: Entry,
    thumbnail: Option<PathBuf>,
    duration: Option<f64>,
    /// The cached transcript, for the card's copy button and for search.
    transcript: Option<Arc<Transcript>>,
}

/// `focus` must be false unless the app is active (see `app::when_active`).
pub fn open(
    state: Entity<Annuncia>,
    focus: bool,
    cx: &mut App,
) -> Option<WindowHandle<LibraryView>> {
    let bounds = Bounds::centered(None, size(px(980.), px(660.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Annuncia".into()),
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(16.), px(19.))),
        }),
        window_background: WindowBackgroundAppearance::Opaque,
        window_min_size: Some(size(px(560.), px(400.))),
        focus,
        show: true,
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let weak = state.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = weak.update(cx, |s, cx| {
                s.library_closed();
                // In case Settings was recording a shortcut.
                s.resume_hotkeys(cx);
            });
            true
        });
        cx.new(|cx| {
            app::observe_state(&state, window, cx);
            let focus = cx.focus_handle();
            window.focus(&focus);
            // Redraw tick for the detail view's player.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(16))
                        .await;
                    if this
                        .update(cx, |this: &mut LibraryView, cx| this.tick(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            cx.defer_in(window, |this: &mut LibraryView, window, _| {
                this.search = native::install_search_field(
                    window,
                    "Search transcripts",
                    f64::from(SEARCH_WIDTH),
                    f64::from((HEADER - 24.) / 2.),
                    f64::from(PADDING + COG + 8.),
                );
            });
            LibraryView {
                state,
                focus,
                revision: None,
                items: Vec::new(),
                copied: None,
                detail: None,
                search: None,
                query: String::new(),
                settings: None,
                menu: None,
                error: None,
            }
        })
    })
    .inspect_err(|err| eprintln!("annuncia: library window: {err}"))
    .ok()
}

impl Focusable for LibraryView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl LibraryView {
    /// Picks up added or replaced recordings, then fills in thumbnails and lengths off the
    /// main thread, one recording at a time so the grid fills in as it goes.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        let revision = state.library_revision;
        if self.revision == Some(revision) {
            return;
        }
        self.revision = Some(revision);
        let mut old = std::mem::take(&mut self.items);
        self.items = state
            .library
            .recordings
            .iter()
            .map(|entry| {
                // Keep what's known so the grid doesn't flash while it reloads.
                match old.iter().position(|i| i.entry.path == entry.path) {
                    Some(i) => Item {
                        entry: entry.clone(),
                        ..old.swap_remove(i)
                    },
                    None => Item {
                        entry: entry.clone(),
                        thumbnail: None,
                        duration: None,
                        transcript: None,
                    },
                }
            })
            .collect();
        let paths: Vec<PathBuf> = self.items.iter().map(|i| i.entry.path.clone()).collect();
        cx.spawn(async move |this, cx| {
            for path in paths {
                let video = path.clone();
                let (thumbnail, duration, transcript) = cx
                    .background_spawn(async move {
                        (
                            library::thumbnail(&video),
                            crate::media::duration(&video),
                            crate::transcript::load_cached(&video).map(Arc::new),
                        )
                    })
                    .await;
                let Ok(current) = this.update(cx, |this, cx| {
                    if this.revision != Some(revision) {
                        return false;
                    }
                    if let Some(item) = this.items.iter_mut().find(|i| i.entry.path == path) {
                        item.thumbnail = thumbnail;
                        item.duration = Some(duration);
                        item.transcript = transcript;
                        cx.notify();
                    }
                    true
                }) else {
                    return;
                };
                if !current {
                    return;
                }
            }
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.modifiers.platform && ks.key == "w" {
            self.state.update(cx, |s, cx| {
                s.library_closed();
                s.resume_hotkeys(cx);
            });
            window.remove_window();
            return;
        }
        if ks.modifiers.platform && ks.key == "," {
            self.show_settings(None, window, cx);
            return;
        }
        // Settings handles its own keys.
        if self.settings.is_some() {
            return;
        }
        if self.menu.is_some() && ks.key == "escape" {
            self.close_menu(cx);
            return;
        }
        if ks.modifiers.platform && ks.key == "f" && self.detail.is_none() {
            native::focus_search_field(window, self.search.as_deref());
            return;
        }
        if self.detail.is_none() || ks.modifiers.platform || ks.modifiers.control {
            return;
        }
        match ks.key.as_str() {
            "escape" => self.close_detail(cx),
            "space" => self.toggle_play(cx),
            "left" => self.skip(-5., cx),
            "right" => self.skip(5., cx),
            _ => {}
        }
    }

    /// Shows Settings in place of the recordings, on `page` if given.
    pub fn show_settings(
        &mut self,
        page: Option<settings_view::Page>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(detail) = &self.detail {
            detail.player.pause();
        }
        native::focus_search_field(window, None);
        let view = match &self.settings {
            Some((view, _)) => view.clone(),
            None => {
                let state = self.state.clone();
                let view = cx.new(|cx| SettingsView::new(state, window, cx));
                let back = cx.subscribe_in(
                    &view,
                    window,
                    |this, _, _: &settings_view::Back, window, cx| this.close_settings(window, cx),
                );
                self.settings = Some((view.clone(), back));
                view
            }
        };
        if let Some(page) = page {
            view.update(cx, |v, cx| v.show_page(page, cx));
        }
        window.focus(&view.focus_handle(cx));
        cx.notify();
    }

    /// Back from Settings to the recordings (or the recording that was open).
    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((view, _)) = self.settings.take() {
            view.update(cx, |v, cx| v.release_hotkeys(cx));
            window.focus(&self.focus);
            cx.notify();
        }
    }

    /// Opens `path` at `at` seconds, playing when it's a search match.
    fn open_detail(&mut self, path: PathBuf, at: f64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        // Keys go to the player, not the search field it hides.
        native::focus_search_field(window, None);
        window.focus(&self.focus);
        self.state.update(cx, |s, cx| s.load_transcript(&path, cx));
        let player = Player::new(&path, mtm);
        // Show the first frame right away.
        player.seek(at, true);
        if at > 0. {
            player.play();
        }
        self.detail = Some(Detail {
            path,
            player,
            playhead: at,
            bar: Rc::new(Cell::new(None)),
            scrubbing: false,
            transcript_scroll: ScrollHandle::new(),
            followed_paragraph: None,
            error: None,
        });
        cx.notify();
    }

    fn close_detail(&mut self, cx: &mut Context<Self>) {
        if let Some(detail) = self.detail.take() {
            detail.player.pause();
        }
        cx.notify();
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if let Some(field) = &self.search {
            // The field only belongs to the grid.
            let hidden = self.detail.is_some() || self.settings.is_some();
            if field.isHidden() != hidden {
                field.setHidden(hidden);
            }
            let query = field.stringValue().to_string();
            if query != self.query {
                self.query = query;
                cx.notify();
            }
        }
        let Some(detail) = &mut self.detail else {
            return;
        };
        let mut changed = detail.player.poll();
        if detail.player.is_playing() && !detail.scrubbing {
            let t = detail.player.time();
            if (t - detail.playhead).abs() > 1e-3 {
                detail.playhead = t;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// The open recording's length, once the grid has read it.
    fn detail_duration(&self) -> Option<f64> {
        let detail = self.detail.as_ref()?;
        self.items
            .iter()
            .find(|i| i.entry.path == detail.path)?
            .duration
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let duration = self.detail_duration();
        let Some(detail) = &mut self.detail else {
            return;
        };
        if detail.player.is_playing() {
            detail.player.pause();
        } else {
            // At the end: start over.
            if duration.is_some_and(|d| detail.playhead >= d - 0.05) {
                detail.playhead = 0.;
                detail.player.seek(0., true);
            }
            detail.player.play();
        }
        cx.notify();
    }

    fn seek(&mut self, t: f64, exact: bool) {
        let duration = self.detail_duration().unwrap_or(f64::MAX);
        if let Some(detail) = &mut self.detail {
            detail.playhead = t.clamp(0., duration);
            detail.player.seek(detail.playhead, exact);
        }
    }

    fn skip(&mut self, by: f64, cx: &mut Context<Self>) {
        if let Some(t) = self.detail.as_ref().map(|d| d.playhead + by) {
            self.seek(t, true);
            cx.notify();
        }
    }

    fn time_at(&self, x: Pixels) -> Option<f64> {
        let bounds = self.detail.as_ref()?.bar.get()?;
        let fraction = f32::from(x - bounds.origin.x) / f32::from(bounds.size.width);
        Some(f64::from(fraction.clamp(0., 1.)) * self.detail_duration()?)
    }

    fn on_bar_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.time_at(event.position.x) {
            if let Some(detail) = &mut self.detail {
                detail.scrubbing = true;
            }
            self.seek(t, false);
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.detail.as_ref().is_some_and(|d| d.scrubbing) {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_scrub(cx);
            return;
        }
        if let Some(t) = self.time_at(event.position.x) {
            self.seek(t, false);
            cx.notify();
        }
    }

    fn end_scrub(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        if detail.scrubbing {
            detail.scrubbing = false;
            detail.player.seek(detail.playhead, true);
            cx.notify();
        }
    }

    fn rename(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let current = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let names = native::prompt_for_name(window, "Rename recording", "Rename", &current);
        cx.spawn(async move |this, cx| {
            let Ok(Some(name)) = names.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let result = this
                    .state
                    .update(cx, |s, cx| s.rename_recording(&path, &name, cx));
                match result {
                    Ok(to) => {
                        this.error = None;
                        if let Some(detail) = this.detail.as_mut().filter(|d| d.path == path) {
                            detail.path = to;
                            detail.error = None;
                        }
                    }
                    Err(err) => this.report(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn trash(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in &paths {
            match self.state.update(cx, |s, cx| s.trash_recording(path, cx)) {
                Ok(()) => {
                    if self.detail.as_ref().is_some_and(|d| &d.path == path) {
                        self.close_detail(cx);
                    }
                }
                Err(err) => self.report(err),
            }
        }
        cx.notify();
    }

    /// Shows why an action didn't happen: in the detail header if a recording is open, else
    /// in the grid's.
    fn report(&mut self, err: SharedString) {
        match &mut self.detail {
            Some(detail) => detail.error = Some(err),
            None => self.error = Some(err),
        }
    }

    fn edit(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(detail) = &self.detail {
            detail.player.pause();
        }
        self.state.update(cx, |s, cx| s.open_editor(path, cx));
    }

    /// Whether `path` has a transcript with words in it, cached or made this session.
    fn has_transcript(&self, path: &Path, cx: &App) -> bool {
        self.items
            .iter()
            .find(|i| i.entry.path == path)
            .and_then(|i| i.transcript.as_ref())
            .is_some_and(|t| !t.is_empty())
            || matches!(
                self.state.read(cx).transcripts.get(path),
                Some(TranscriptState::Ready(t)) if !t.is_empty()
            )
    }

    /// Everything a card can do, the same as the detail view can.
    fn card_actions(&self, path: &Path, cx: &App) -> Vec<Option<Action>> {
        let transcript = self.has_transcript(path, cx);
        let state = self.state.read(cx);
        let mut actions = vec![Some(Action::Open), Some(Action::Edit), None];
        actions.push(Some(Action::CopyFile));
        if transcript {
            actions.push(Some(Action::CopyTranscript));
        }
        let working = matches!(state.transcripts.get(path), Some(TranscriptState::Working));
        if state.manual_model().is_some() && !working {
            actions.push(Some(if transcript {
                Action::TranscribeAgain
            } else {
                Action::Transcribe
            }));
        }
        actions.extend([
            None,
            Some(Action::Reveal),
            Some(Action::Rename),
            None,
            Some(Action::Trash),
        ]);
        actions
    }

    fn open_card_menu(&mut self, path: PathBuf, at: Point<Pixels>, cx: &mut Context<Self>) {
        let actions = self.card_actions(&path, cx);
        self.menu = Some(Menu {
            paths: vec![path],
            at,
            actions,
        });
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    fn perform(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else {
            return;
        };
        self.error = None;
        let Some(path) = menu.paths.first().cloned() else {
            return;
        };
        match action {
            Action::Open => self.open_detail(path, 0., window, cx),
            Action::Edit => self.edit(path, cx),
            Action::CopyFile => native::copy_file_to_pasteboard(&path),
            Action::CopyTranscript => self.copy_transcript(&path, cx),
            Action::Transcribe => self.state.update(cx, |s, cx| s.transcribe_now(path, cx)),
            // Forget the transcript first, or transcribing keeps the one it has.
            Action::TranscribeAgain => self.state.update(cx, |s, cx| {
                s.transcripts.remove(&path);
                s.transcribe_now(path, cx);
            }),
            Action::Reveal => native::reveal_in_finder(&path),
            Action::Rename => self.rename(path, window, cx),
            Action::Trash => self.trash(menu.paths, cx),
        }
        cx.notify();
    }

    fn copy_transcript(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.state.read(cx).copy_transcript(path) {
            return;
        }
        self.copied = Some(path.to_path_buf());
        cx.notify();
        let path = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.copied.as_ref() == Some(&path) {
                    this.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn card(
        &self,
        index: usize,
        item: &Item,
        width: f32,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let path = item.entry.path.clone();
        let created = local_time(item.entry.created);
        let time = created
            .map(|t| t.format("%-I:%M %p").to_string())
            .unwrap_or_default();
        let (title, subtitle) = match custom_name(&path) {
            Some(name) => (name, Some(time)),
            None => (time, None),
        };
        let transcript = self.has_transcript(&path, cx);
        let copied = self.copied.as_ref() == Some(&path);
        // Mac screens are close to 16:10.
        let height = width * 10. / 16.;
        let group = SharedString::from(format!("card-{index}"));

        let thumbnail = div()
            .relative()
            .w(px(width))
            .h(px(height))
            .rounded(px(10.))
            .overflow_hidden()
            .bg(theme.tile)
            .when_some(item.thumbnail.clone(), |d, thumb| {
                // Sized explicitly: a percentage size lets the image keep its own aspect ratio.
                d.child(
                    img(thumb)
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(width))
                        .h(px(height))
                        .rounded(px(10.))
                        .object_fit(ObjectFit::Cover),
                )
            })
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(theme.hairline),
            )
            .when_some(item.duration, |d, duration| {
                d.child(
                    div()
                        .absolute()
                        .right(px(8.))
                        .bottom(px(8.))
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(5.))
                        .bg(hsla(0., 0., 0., 0.72))
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(crate::transcript::timestamp(duration)),
                )
            })
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .right(px(8.))
                    .flex()
                    .gap(px(6.))
                    .opacity(0.)
                    .group_hover(group.clone(), |s| s.opacity(1.))
                    .when(transcript, |d| {
                        let path = path.clone();
                        d.child(
                            overlay_button(
                                ("copy", index),
                                "transcript",
                                if copied { "Copied" } else { "Copy transcript" },
                                theme,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.copy_transcript(&path, cx);
                                },
                            )),
                        )
                    })
                    .child({
                        let path = path.clone();
                        overlay_button(("reveal", index), "folder", "Show in Finder", theme)
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                native::reveal_in_finder(&path);
                            })
                    })
                    .child({
                        let path = path.clone();
                        more_button(("more", index), theme).on_click(cx.listener(
                            move |this, event: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.open_card_menu(path.clone(), event.position(), cx);
                            },
                        ))
                    }),
            );

        div()
            .id(("recording", index))
            .group(group)
            .w(px(width))
            .flex()
            .flex_col()
            .gap(px(8.))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let path = path.clone();
                    move |this, event: &MouseDownEvent, _, cx| {
                        this.open_card_menu(path.clone(), event.position, cx)
                    }
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_detail(path.clone(), 0., window, cx)
            }))
            .child(thumbnail)
            .child(
                div()
                    .px(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .truncate()
                            .child(title),
                    )
                    .when_some(subtitle, |d, subtitle| {
                        d.child(
                            div()
                                .text_size(px(12.))
                                .text_color(theme.text_muted)
                                .child(subtitle),
                        )
                    }),
            )
    }
}

impl LibraryView {
    fn detail_header(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(detail) = &self.detail else {
            return div().into_any_element();
        };
        let path = detail.path.clone();
        let created = self
            .items
            .iter()
            .find(|i| i.entry.path == path)
            .map_or(0, |i| i.entry.created);
        let title = full_title(&path, created);
        // Buttons take their own mouse downs so they don't start a window drag.
        let header_button = |id: &'static str, label: &'static str| {
            button(id, label, false, theme)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        div()
            .h(px(HEADER))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(84.))
            .pr(px(PADDING))
            .border_b_1()
            .border_color(theme.hairline)
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                native::begin_window_drag(window)
            })
            .child(
                div()
                    .id("back")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(2.))
                    .pl(px(4.))
                    .pr(px(8.))
                    .py(px(4.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .hover(|s| s.bg(theme.control_hover).text_color(theme.text))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, _, cx| this.close_detail(cx)))
                    .child(icon("chevron-left", 15., theme.text_muted))
                    .child("Library"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(title),
            )
            .when_some(detail.error.clone(), |d, error| {
                d.child(
                    div()
                        .flex_shrink()
                        .min_w_0()
                        .text_size(px(12.))
                        .text_color(theme.record)
                        .truncate()
                        .child(error),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(6.))
                    .child(header_button("edit", "Edit").on_click(cx.listener({
                        let path = path.clone();
                        move |this, _, _, cx| this.edit(path.clone(), cx)
                    })))
                    .child(header_button("reveal", "Show in Finder").on_click({
                        let path = path.clone();
                        move |_, _, _| native::reveal_in_finder(&path)
                    }))
                    .child(header_button("rename", "Rename…").on_click(cx.listener({
                        let path = path.clone();
                        move |this, _, window, cx| this.rename(path.clone(), window, cx)
                    })))
                    .child(header_button("trash", "Move to Trash").on_click(
                        cx.listener(move |this, _, _, cx| this.trash(vec![path.clone()], cx)),
                    )),
            )
            .into_any_element()
    }

    fn detail_body(&mut self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let duration = self.detail_duration();
        let Some(detail) = &self.detail else {
            return div().into_any_element();
        };
        let playing = detail.player.is_playing();
        let playhead = detail.playhead;
        let fraction = duration.map_or(0., |d| (playhead / d).clamp(0., 1.)) as f32;
        let bar = detail.bar.clone();

        // A hairline edge keeps the black video frame from melting into the black window.
        let video = div()
            .id("video")
            .flex_1()
            .min_h_0()
            .rounded(px(16.))
            .overflow_hidden()
            .bg(gpui::black())
            .border_1()
            .border_color(theme.hairline)
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx)))
            .child(match detail.player.frame() {
                Some(frame) => surface(frame)
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                None => dithered_note("Loading…", theme),
            });

        let controls = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .id("play")
                    .size(px(30.))
                    .flex_none()
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(theme.control)
                    .hover(|s| s.bg(theme.control_hover))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_play(cx)))
                    .child(icon(
                        if playing { "pause" } else { "play" },
                        13.,
                        theme.text,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(format!(
                        "{} / {}",
                        timestamp(playhead),
                        duration.map(timestamp).unwrap_or_else(|| "–".into())
                    )),
            )
            .child(
                div()
                    .id("scrubber")
                    .flex_1()
                    .h(px(20.))
                    .relative()
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_bar_down))
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(px(4.))
                            .rounded(px(2.))
                            .bg(theme.control_active)
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .h_full()
                                    .w(relative(fraction))
                                    .rounded(px(2.))
                                    .bg(theme.accent),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top(px(-4.))
                                    .left(relative(fraction))
                                    .ml(px(-6.))
                                    .size(px(12.))
                                    .rounded_full()
                                    .bg(theme.text),
                            ),
                    )
                    .child(
                        canvas(move |b, _, _| bar.set(Some(b)), |_, _, _, _| {})
                            .absolute()
                            .inset_0(),
                    ),
            );

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(16.))
            .p(px(PADDING))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(video)
                    .child(controls),
            )
            .child(self.transcript_panel(theme, cx))
            .into_any_element()
    }

    fn transcript_panel(&mut self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(path) = self.detail.as_ref().map(|d| d.path.clone()) else {
            return div().into_any_element();
        };
        let state = self.state.read(cx);
        let transcript = state.transcripts.get(&path).cloned();
        let can_transcribe = state.manual_model().is_some();
        let copied = self.copied.as_ref() == Some(&path);

        let transcribe = |label: &'static str, cx: &mut Context<Self>| {
            let path = path.clone();
            button("transcribe", label, true, theme)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state
                        .update(cx, |s, cx| s.transcribe_now(path.clone(), cx));
                }))
                .into_any_element()
        };
        let body = match &transcript {
            Some(TranscriptState::Ready(t)) if !t.is_empty() => {
                let t = t.clone();
                let detail = self.detail.as_mut().expect("detail is open");
                let paragraphs = t.paragraph_pieces();
                let current = t.piece_at(detail.playhead);
                let current_paragraph = paragraph_of(&paragraphs, current);
                // Follow playback, a paragraph at a time.
                if detail.player.is_playing() && current_paragraph != detail.followed_paragraph {
                    if let Some(ix) = current_paragraph {
                        detail.transcript_scroll.scroll_to_item(ix);
                    }
                    detail.followed_paragraph = current_paragraph;
                }
                let scroll = detail.transcript_scroll.clone();
                transcript_view::paragraphs(
                    &t,
                    &paragraphs,
                    current,
                    |_| false,
                    &scroll,
                    theme,
                    cx,
                    |this: &mut Self, t, cx| {
                        this.seek(t, true);
                        cx.notify();
                    },
                )
            }
            Some(TranscriptState::Ready(_)) => dithered_note("No speech in this recording.", theme),
            Some(TranscriptState::Working) => dithered_note("Transcribing…", theme),
            Some(TranscriptState::Failed(err)) => {
                let action = can_transcribe.then(|| transcribe("Try again", cx));
                note_with_action(err.clone(), action, theme)
            }
            None if can_transcribe => note_with_action(
                "No transcript yet.".into(),
                Some(transcribe("Transcribe", cx)),
                theme,
            ),
            None => note_with_action(
                "Download a speech model in Settings, under After recording, to transcribe \
                 recordings."
                    .into(),
                Some(
                    button("open-settings", "Open Settings", false, theme)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_settings(
                                Some(settings_view::Page::AfterRecording),
                                window,
                                cx,
                            );
                        }))
                        .into_any_element(),
                ),
                theme,
            ),
        };

        let has_text = matches!(&transcript, Some(TranscriptState::Ready(t)) if !t.is_empty());
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
            .when(has_text, |d| {
                d.child(
                    button(
                        "copy-transcript",
                        if copied { "Copied" } else { "Copy transcript" },
                        false,
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy_transcript(&path, cx);
                    })),
                )
            });

        div()
            .w(px(320.))
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
}

impl LibraryView {
    /// The open menu, drawn over everything at the point it was opened from.
    fn menu_popover(&self, theme: Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let rows = menu
            .actions
            .iter()
            .enumerate()
            .map(|(i, action)| match *action {
                None => div()
                    .h(px(1.))
                    .mx(px(8.))
                    .my(px(4.))
                    .bg(theme.hairline)
                    .into_any_element(),
                Some(action) => div()
                    .id(("menu-item", i))
                    .px(px(10.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .whitespace_nowrap()
                    .text_size(px(13.))
                    .text_color(theme.text)
                    .hover(|s| s.bg(theme.control_hover))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.perform(action, window, cx)),
                    )
                    .child(action.label())
                    .into_any_element(),
            })
            .collect::<Vec<_>>();
        Some(
            deferred(
                anchored()
                    .position(menu.at)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("menu")
                            .occlude()
                            .min_w(px(180.))
                            .p(px(5.))
                            .flex()
                            .flex_col()
                            .rounded(px(10.))
                            .bg(theme.surface)
                            .border_1()
                            .border_color(theme.hairline)
                            .shadow_lg()
                            // Its own mouse downs mustn't start a window drag under it.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
                            .children(rows),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

impl LibraryView {
    /// Recordings whose transcript or name matches the query, each with its matches; clicking
    /// one plays the recording from there.
    fn results(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let query = self.query.trim().to_lowercase();
        let state = self.state.read(cx);
        let found: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let transcript = match state.transcripts.get(&item.entry.path) {
                    Some(TranscriptState::Ready(t)) => Some(t.clone()),
                    _ => item.transcript.clone(),
                };
                let hits = transcript
                    .as_ref()
                    .map(|t| t.search(&query))
                    .unwrap_or_default();
                let named = full_title(&item.entry.path, item.entry.created)
                    .to_lowercase()
                    .contains(&query);
                (named || !hits.is_empty()).then_some((i, transcript, hits))
            })
            .collect();

        if found.is_empty() {
            return dithered_note(format!("Nothing matches “{}”", self.query.trim()), theme);
        }
        let rows = found.into_iter().map(|(i, transcript, hits)| {
            let item = &self.items[i];
            let path = item.entry.path.clone();
            let thumbnail = div()
                .id(("result-thumb", i))
                .relative()
                .w(px(160.))
                .h(px(100.))
                .flex_none()
                .rounded(px(8.))
                .overflow_hidden()
                .bg(theme.tile)
                .cursor_pointer()
                .on_click({
                    let path = path.clone();
                    cx.listener(move |this, _, window, cx| {
                        this.open_detail(path.clone(), 0., window, cx)
                    })
                })
                .when_some(item.thumbnail.clone(), |d, thumb| {
                    d.child(
                        img(thumb)
                            .absolute()
                            .top_0()
                            .left_0()
                            .w(px(160.))
                            .h(px(100.))
                            .rounded(px(8.))
                            .object_fit(ObjectFit::Cover),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme.hairline),
                );
            let count = match hits.len() {
                0 => "Name matches".to_string(),
                1 => "1 match".to_string(),
                n => format!("{n} matches"),
            };
            let lines = transcript.map(|t| {
                hits.iter()
                    .take(SHOWN_MATCHES)
                    .enumerate()
                    .map(|(k, hit)| {
                        let at = t.pieces[hit.start].start;
                        let (text, matched) = snippet(&t, hit);
                        let path = path.clone();
                        div()
                            .id(("hit", i * SHOWN_MATCHES + k))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.control_hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_detail(path.clone(), at, window, cx)
                            }))
                            .child(
                                div()
                                    .w(px(40.))
                                    .flex_none()
                                    .text_size(px(11.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text_muted)
                                    .child(timestamp(at)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .text_color(theme.text_muted)
                                    .child(StyledText::new(text).with_highlights([(
                                        matched,
                                        HighlightStyle {
                                            color: Some(theme.text),
                                            background_color: Some(theme.accent.opacity(0.35)),
                                            ..Default::default()
                                        },
                                    )])),
                            )
                    })
                    .collect::<Vec<_>>()
            });
            let more = hits.len().saturating_sub(SHOWN_MATCHES);
            div().flex().gap(px(16.)).child(thumbnail).child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .px(px(8.))
                            .pb(px(4.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(full_title(&path, item.entry.created)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(12.))
                                    .text_color(theme.text_muted)
                                    .child(count),
                            ),
                    )
                    .children(lines.into_iter().flatten())
                    .when(more > 0, |d| {
                        d.child(
                            div()
                                .px(px(8.))
                                .pt(px(2.))
                                .text_size(px(12.))
                                .text_color(theme.text_muted)
                                .child(format!("and {more} more")),
                        )
                    }),
            )
        });
        div()
            .id("results-scroll")
            .flex_1()
            .overflow_y_scroll()
            .child(
                div()
                    .p(px(PADDING))
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .children(rows),
            )
            .into_any_element()
    }
}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh(cx);
        let theme = Theme::for_window(window);
        let root = div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_scrub(cx)),
            )
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme.window)
            .text_color(theme.text)
            .when(theme.stars, |d| d.child(super::starfield()));
        if let Some((settings, _)) = &self.settings {
            return root.child(settings.clone());
        }
        let menu = self.menu_popover(theme, cx);
        if self.detail.is_some() {
            return root
                .child(self.detail_header(theme, cx))
                .child(self.detail_body(theme, cx))
                .children(menu);
        }
        let available = f32::from(window.viewport_size().width) - PADDING * 2.;
        let columns = ((available + GAP) / (MIN_CARD + GAP)).floor().max(1.) as usize;
        let width = ((available - GAP * (columns - 1) as f32) / columns as f32).floor();

        let count = self.items.len();
        let header = div()
            .h(px(HEADER))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            // Clear of the traffic lights.
            .pl(px(92.))
            .pr(px(PADDING))
            .border_b_1()
            .border_color(theme.hairline)
            .on_mouse_down(MouseButton::Left, |_, window, _| {
                native::begin_window_drag(window)
            })
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("Library"),
            )
            .when(count > 0, |d| {
                d.child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .child(match count {
                            1 => "1 recording".to_string(),
                            n => format!("{n} recordings"),
                        }),
                )
            })
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .flex_shrink()
                        .min_w_0()
                        .text_size(px(12.))
                        .text_color(theme.record)
                        .truncate()
                        .child(error),
                )
            })
            // Room for the native search field, then the cog.
            .child(div().flex_1())
            .when(self.state.read(cx).update_ready.is_some(), |d| {
                let state = self.state.clone();
                // A quiet header control like the cog beside it, not a filled button: only the
                // accent glyph marks it, matching the row at the foot of Settings' sidebar.
                d.child(
                    div()
                        .id("install-update")
                        .h(px(COG))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(8.))
                        .rounded(px(7.))
                        .cursor_pointer()
                        .whitespace_nowrap()
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .hover(|s| s.bg(theme.control_hover).text_color(theme.text))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| state.update(cx, |s, cx| s.install_update(cx)))
                        .child(icon("restart", 13., theme.accent))
                        .child("Relaunch to update"),
                )
            })
            .child(div().w(px(SEARCH_WIDTH + 8.)).flex_none())
            .child(
                div()
                    .id("settings")
                    .size(px(COG))
                    .flex_none()
                    .rounded(px(7.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.control_hover))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.show_settings(None, window, cx)),
                    )
                    .child(icon("gear", 16., theme.text_muted)),
            );

        let body = if !self.query.trim().is_empty() {
            self.results(theme, cx)
        } else if self.items.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .child(img("images/icon.png").size(px(64.)).opacity(0.55))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.text_muted)
                        .child("Recordings you make show up here"),
                )
                .into_any_element()
        } else {
            let mut sections = Vec::new();
            let mut start = 0;
            while start < self.items.len() {
                let day = day_of(&self.items[start]);
                let end = self.items[start..]
                    .iter()
                    .position(|i| day_of(i) != day)
                    .map_or(self.items.len(), |n| start + n);
                let rows = (start..end)
                    .collect::<Vec<_>>()
                    .chunks(columns)
                    .map(|row| {
                        div().flex().gap(px(GAP)).children(
                            row.iter()
                                .map(|&i| self.card(i, &self.items[i], width, theme, cx)),
                        )
                    })
                    .collect::<Vec<_>>();
                sections.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(GAP))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(day_label(day)),
                        )
                        .children(rows),
                );
                start = end;
            }
            div()
                .id("library-scroll")
                .flex_1()
                .overflow_y_scroll()
                .child(
                    div()
                        .p(px(PADDING))
                        .flex()
                        .flex_col()
                        .gap(px(28.))
                        .children(sections),
                )
                .into_any_element()
        };

        root.child(header).child(body).children(menu)
    }
}

/// An empty state with a button under it.
fn note_with_action(text: SharedString, action: Option<AnyElement>, theme: Theme) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .px(px(20.))
        .child(img("images/icon.png").size(px(64.)).opacity(0.55))
        .child(
            div()
                .max_w(px(240.))
                .text_size(px(12.5))
                .text_color(theme.text_muted)
                .text_center()
                .child(text),
        )
        .children(action)
        .into_any_element()
}

/// A small dark button laid over a thumbnail.
fn overlay_button(
    id: impl Into<gpui::ElementId>,
    icon_name: &'static str,
    label: &'static str,
    theme: Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(26.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(5.))
        .rounded(px(7.))
        .bg(hsla(0., 0., 0., 0.72))
        .hover(|s| s.bg(hsla(0., 0., 0.12, 0.9)))
        .text_size(px(11.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text)
        .whitespace_nowrap()
        .child(icon(icon_name, 12., theme.text))
        .child(label)
}

/// The "⋯" overlay button that opens a card's menu.
fn more_button(id: impl Into<gpui::ElementId>, theme: Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(26.))
        .w(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .bg(hsla(0., 0., 0., 0.72))
        .hover(|s| s.bg(hsla(0., 0., 0.12, 0.9)))
        .text_size(px(13.))
        .font_weight(FontWeight::BOLD)
        .text_color(theme.text)
        .child("⋯")
}

fn local_time(created: i64) -> Option<DateTime<Local>> {
    Local.timestamp_opt(created, 0).single()
}

fn day_of(item: &Item) -> Option<chrono::NaiveDate> {
    local_time(item.entry.created).map(|t| t.date_naive())
}

fn day_label(day: Option<chrono::NaiveDate>) -> String {
    let Some(day) = day else {
        return "Unknown date".into();
    };
    let today = Local::now().date_naive();
    if day == today {
        "Today".into()
    } else if Some(day) == today.pred_opt() {
        "Yesterday".into()
    } else if day.format("%Y").to_string() == today.format("%Y").to_string() {
        day.format("%A, %B %-d").to_string()
    } else {
        day.format("%B %-d, %Y").to_string()
    }
}

/// The file's name, or the full date and time while it still has the default name.
fn full_title(path: &Path, created: i64) -> String {
    custom_name(path).unwrap_or_else(|| {
        local_time(created)
            .map(|t| t.format("%A, %B %-d at %-I:%M %p").to_string())
            .unwrap_or_else(|| "Recording".into())
    })
}

/// A line of the transcript around the match in `hit` (piece indices), with the match's byte
/// range in it.
fn snippet(
    transcript: &Transcript,
    hit: &std::ops::Range<usize>,
) -> (String, std::ops::Range<usize>) {
    const BEFORE: usize = 6;
    const AFTER: usize = 14;
    let words = |range: std::ops::Range<usize>| {
        transcript.pieces[range]
            .iter()
            .map(|p| p.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let from = hit.start.saturating_sub(BEFORE);
    let to = (hit.end + AFTER).min(transcript.pieces.len());
    let mut text = if from > 0 {
        "…".to_string()
    } else {
        String::new()
    };
    let before = words(from..hit.start);
    if !before.is_empty() {
        text.push_str(&before);
        text.push(' ');
    }
    let start = text.len();
    text.push_str(&words(hit.clone()));
    let end = text.len();
    let after = words(hit.end..to);
    if !after.is_empty() {
        text.push(' ');
        text.push_str(&after);
    }
    (text, start..end)
}

/// The file's name, unless it's still the default "Annuncia <date> at <time>".
fn custom_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let is_default = stem
        .strip_prefix("Annuncia ")
        .is_some_and(|rest| NaiveDateTime::parse_from_str(rest, "%Y-%m-%d at %H.%M.%S").is_ok());
    (!is_default).then_some(stem)
}
