//! The library: every recording Annuncia has saved, newest first, grouped by day, with quick
//! access to each one's transcript and file. Clicking a recording opens it in a detail view
//! with a player and its transcript.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use crate::hotkeys;
use crate::library::{self, Entry};
use crate::native;
use crate::player::Player;
use crate::transcript::{Transcript, timestamp};
use crate::ui::icons::icon;
use crate::ui::settings_view::{self, SettingsView};
use crate::ui::transcript_view::{self, dithered_note, paragraph_of};
use crate::ui::{Theme, button, tip};

const PADDING: f32 = 24.;
const GAP: f32 = 16.;
const MIN_CARD: f32 = 220.;
const HEADER: f32 = 52.;
const SEARCH_WIDTH: f32 = 260.;
/// The settings cog at the header's right end, beside the search field.
const COG: f32 = 28.;
/// Matches listed per recording before "Show N more".
const SHOWN_MATCHES: usize = 3;
/// How long Undo stays offered after a Trash.
const UNDO_FOR: Duration = Duration::from_secs(6);
/// The speeds the player's speed button cycles through.
const SPEEDS: [f32; 4] = [1., 1.25, 1.5, 2.];
const VOLUME_WIDTH: f32 = 64.;

/// The player's speed, volume and mute, kept for the session from one recording to the next.
#[derive(Clone, Copy)]
struct Playback {
    speed: f32,
    /// Never zero: at zero it's `muted`, and this is the volume to come back to.
    volume: f32,
    muted: bool,
}

thread_local! {
    static PLAYBACK: Cell<Playback> = const {
        Cell::new(Playback {
            speed: 1.,
            volume: 1.,
            muted: false,
        })
    };
}

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
    /// Search results showing all their matches, not just the first few.
    expanded: Vec<PathBuf>,
    /// Settings, while the window shows it instead of the recordings.
    settings: Option<(Entity<SettingsView>, Subscription)>,
    /// The right-click (or "⋯") menu, while it's open.
    menu: Option<Menu>,
    /// Why the last rename or Trash from the grid didn't happen.
    error: Option<SharedString>,
    /// The selected recordings in the grid.
    selection: Vec<PathBuf>,
    /// Where ⇧-click and ⇧-arrow selections start from.
    anchor: Option<PathBuf>,
    /// The recording the arrow keys move from: the last one clicked or moved to.
    cursor: Option<PathBuf>,
    grid_scroll: ScrollHandle,
    /// Cards per row at the last layout, for the up and down arrows.
    columns: usize,
    /// Recordings just moved to the Trash, while Undo is offered.
    undo: Option<Undo>,
}

struct Undo {
    /// Each recording's library entry and where its file went in the Trash.
    trashed: Vec<(Entry, PathBuf)>,
    /// Lets the timer that hides the bar tell whether it's still the same one.
    shown: Instant,
}

/// How a click or arrow key changes the selection.
#[derive(Clone, Copy)]
enum Select {
    Only,
    /// ⌘: add or remove one.
    Toggle,
    /// ⇧: everything from the anchor.
    Range,
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
    /// The volume slider's bounds, measured as it paints.
    volume_bar: Rc<Cell<Option<Bounds<Pixels>>>>,
    adjusting_volume: bool,
    /// Set by a frame step, which moves the player while it's paused: follow it.
    stepped: bool,
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
                    "Search recordings",
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
                expanded: Vec::new(),
                settings: None,
                menu: None,
                error: None,
                selection: Vec::new(),
                anchor: None,
                cursor: None,
                grid_scroll: ScrollHandle::new(),
                columns: 1,
                undo: None,
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
        let items = &self.items;
        let exists = |p: &PathBuf| items.iter().any(|i| &i.entry.path == p);
        self.selection.retain(exists);
        if !self.anchor.as_ref().is_some_and(exists) {
            self.anchor = None;
        }
        if !self.cursor.as_ref().is_some_and(exists) {
            self.cursor = None;
        }
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
        // Any key closes the menu, so it can't act on a selection the key changed. Esc does
        // nothing more.
        if self.menu.is_some() {
            self.close_menu(cx);
            if ks.key == "escape" {
                return;
            }
        }
        // ⌘-keys reach us even while the search field is being edited; there they're its own
        // (⌘A, ⌘Z, ⌘⌫), not the grid's.
        let editing = self.search_editing();
        if ks.modifiers.platform
            && !ks.modifiers.shift
            && ks.key == "z"
            && self.undo.is_some()
            && !editing
        {
            self.undo_trash(cx);
            return;
        }
        if ks.modifiers.platform && ks.key == "f" {
            // From a recording too: back to the grid, to search.
            if self.detail.is_some() {
                self.close_detail(cx);
            }
            if let Some(field) = &self.search {
                field.setHidden(false);
            }
            native::focus_search_field(window, self.search.as_deref());
            return;
        }
        if editing {
            return;
        }
        // The grid's keys, while it shows (not search results).
        if self.detail.is_none() && self.query.trim().is_empty() {
            self.on_grid_key(event, window, cx);
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
            "," => self.step(-1, cx),
            "." => self.step(1, cx),
            "j" => self.play_backwards(cx),
            "k" => {
                if let Some(detail) = &self.detail {
                    detail.player.pause();
                    cx.notify();
                }
            }
            "l" => self.play_forwards(cx),
            _ => {}
        }
    }

    /// Whether the native search field has the keyboard.
    fn search_editing(&self) -> bool {
        self.search
            .as_ref()
            .is_some_and(|field| field.currentEditor().is_some())
    }

    fn on_grid_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.modifiers.control || ks.modifiers.alt {
            return;
        }
        let key = ks.key.as_str();
        if ks.modifiers.platform {
            match key {
                "a" => {
                    self.selection = self.items.iter().map(|i| i.entry.path.clone()).collect();
                    cx.notify();
                }
                // Finder's ⌘⌫.
                "backspace" => self.trash_selection(cx),
                _ => {}
            }
            return;
        }
        match key {
            "left" | "right" | "up" | "down" => self.move_cursor(key, ks.modifiers.shift, cx),
            "enter" | "space" => {
                if let Some(path) = self.cursor_or_first_selected() {
                    self.open_detail(path, 0., key == "space", window, cx);
                }
            }
            "backspace" | "delete" => self.trash_selection(cx),
            "escape" => {
                self.selection.clear();
                cx.notify();
            }
            _ => {}
        }
    }

    fn cursor_or_first_selected(&self) -> Option<PathBuf> {
        self.cursor
            .clone()
            .filter(|c| self.selection.contains(c))
            .or_else(|| self.selected_paths().into_iter().next())
    }

    /// The selection, in grid order.
    fn selected_paths(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .map(|i| &i.entry.path)
            .filter(|p| self.selection.contains(p))
            .cloned()
            .collect()
    }

    fn select(&mut self, index: usize, how: Select, cx: &mut Context<Self>) {
        let Some(path) = self.items.get(index).map(|i| i.entry.path.clone()) else {
            return;
        };
        match how {
            Select::Only => {
                self.selection = vec![path.clone()];
                self.anchor = Some(path.clone());
            }
            Select::Toggle => {
                if let Some(i) = self.selection.iter().position(|p| *p == path) {
                    self.selection.remove(i);
                } else {
                    self.selection.push(path.clone());
                }
                self.anchor = Some(path.clone());
            }
            Select::Range => {
                let from = self
                    .anchor
                    .as_ref()
                    .and_then(|a| self.items.iter().position(|i| &i.entry.path == a))
                    .unwrap_or(index);
                self.selection = self.items[from.min(index)..=from.max(index)]
                    .iter()
                    .map(|i| i.entry.path.clone())
                    .collect();
                if self.anchor.is_none() {
                    self.anchor = Some(path.clone());
                }
            }
        }
        self.cursor = Some(path);
        cx.notify();
    }

    fn on_card_click(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Keys go to the grid, not the search field.
        native::focus_search_field(window, None);
        window.focus(&self.focus);
        let modifiers = event.modifiers();
        if modifiers.platform {
            self.select(index, Select::Toggle, cx);
        } else if modifiers.shift {
            self.select(index, Select::Range, cx);
        } else if event.click_count() >= 2 {
            if let Some(path) = self.items.get(index).map(|i| i.entry.path.clone()) {
                self.open_detail(path, 0., false, window, cx);
            }
        } else {
            self.select(index, Select::Only, cx);
        }
    }

    /// The grid as laid out: each day's recordings (item indices), in rows of `columns`.
    fn sections(&self, columns: usize) -> Vec<Vec<std::ops::Range<usize>>> {
        let mut sections = Vec::new();
        let mut start = 0;
        while start < self.items.len() {
            let day = day_of(&self.items[start]);
            let end = self.items[start..]
                .iter()
                .position(|i| day_of(i) != day)
                .map_or(self.items.len(), |n| start + n);
            sections.push(
                (start..end)
                    .step_by(columns)
                    .map(|row| row..(row + columns).min(end))
                    .collect(),
            );
            start = end;
        }
        sections
    }

    /// Moves the cursor with an arrow key, row by row within each day's rows, and scrolls it
    /// into view. With ⇧ the selection runs from the anchor to it.
    fn move_cursor(&mut self, key: &str, extend: bool, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let last = self.items.len() - 1;
        // The scroll view's children are each day's label followed by its rows. Each row here
        // keeps the child to scroll into view when it's reached.
        let mut rows = Vec::new();
        let mut child = 0;
        for section in self.sections(self.columns) {
            let label = child;
            child += 1;
            for (n, row) in section.into_iter().enumerate() {
                // Going up to a day's first row brings its label into view too.
                let reveal = if n == 0 && key == "up" { label } else { child };
                rows.push((reveal, row));
                child += 1;
            }
        }
        let current = self
            .cursor
            .as_ref()
            .and_then(|c| self.items.iter().position(|i| &i.entry.path == c));
        let next = match current {
            None => 0,
            Some(i) => {
                let r = rows
                    .iter()
                    .position(|(_, row)| row.contains(&i))
                    .unwrap_or(0);
                let column = i - rows[r].1.start;
                let target = match key {
                    "up" => r.checked_sub(1),
                    "down" => Some(r + 1).filter(|&t| t < rows.len()),
                    _ => None,
                };
                match key {
                    "left" => i.saturating_sub(1),
                    "right" => (i + 1).min(last),
                    _ => target.map_or(i, |t| (rows[t].1.start + column).min(rows[t].1.end - 1)),
                }
            }
        };
        self.select(next, if extend { Select::Range } else { Select::Only }, cx);
        if let Some((child, _)) = rows.iter().find(|(_, row)| row.contains(&next)) {
            self.grid_scroll.scroll_to_item(*child);
        }
    }

    fn trash_selection(&mut self, cx: &mut Context<Self>) {
        self.trash_and_select_next(self.selected_paths(), cx);
    }

    /// Moves `paths` to the Trash and selects the recording after them.
    fn trash_and_select_next(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let first = self
            .items
            .iter()
            .position(|i| paths.contains(&i.entry.path))
            .unwrap_or(0);
        let next = self.items[first..]
            .iter()
            .chain(self.items[..first].iter().rev())
            .map(|i| i.entry.path.clone())
            .find(|p| !paths.contains(p));
        self.error = None;
        self.trash(paths, cx);
        // Anything that couldn't be trashed stays selected.
        self.selection.retain(|p| p.exists());
        if self.selection.is_empty()
            && let Some(next) = next
        {
            self.selection = vec![next.clone()];
            self.anchor = Some(next.clone());
            self.cursor = Some(next);
        }
        cx.notify();
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
        self.menu = None;
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

    /// Opens `path` at `at` seconds, playing if `play`.
    fn open_detail(
        &mut self,
        path: PathBuf,
        at: f64,
        play: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        self.menu = None;
        // Keys go to the player, not the search field it hides.
        native::focus_search_field(window, None);
        window.focus(&self.focus);
        self.state.update(cx, |s, cx| s.load_transcript(&path, cx));
        let player = Player::new(&path, mtm);
        let playback = PLAYBACK.get();
        player.set_speed(playback.speed);
        player.set_volume(playback.volume);
        player.set_muted(playback.muted);
        // Show the first frame right away.
        player.seek(at, true);
        if play {
            player.play();
        }
        self.detail = Some(Detail {
            path,
            player,
            playhead: at,
            bar: Rc::new(Cell::new(None)),
            scrubbing: false,
            volume_bar: Rc::new(Cell::new(None)),
            adjusting_volume: false,
            stepped: false,
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
            // Clicking into the field takes the keys; the menu goes with them.
            if self.menu.is_some() && field.currentEditor().is_some() {
                self.menu = None;
                cx.notify();
            }
            let query = field.stringValue().to_string();
            if query != self.query {
                self.query = query;
                self.expanded.clear();
                cx.notify();
            }
        }
        let Some(detail) = &mut self.detail else {
            return;
        };
        let mut changed = detail.player.poll();
        if (detail.player.is_playing() || detail.stepped) && !detail.scrubbing {
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
            detail.stepped = false;
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

    fn step(&mut self, frames: isize, cx: &mut Context<Self>) {
        if let Some(detail) = &mut self.detail {
            detail.player.step(frames);
            detail.stepped = true;
            cx.notify();
        }
    }

    /// L: plays, or plays faster each time while already playing forward.
    fn play_forwards(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        let rate = detail.player.rate();
        if rate > 0. {
            if let Some(&faster) = SPEEDS.iter().find(|&&s| s > rate) {
                detail.player.play_at(faster);
            }
            cx.notify();
        } else {
            if rate < 0. {
                detail.player.pause();
            }
            self.toggle_play(cx);
        }
    }

    /// J: plays backwards, faster each time; where the file can't, skips back instead.
    fn play_backwards(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        if !detail.player.can_play_backwards() {
            self.skip(-5., cx);
            return;
        }
        let rate = detail.player.rate();
        let backwards = if rate < 0. {
            SPEEDS
                .iter()
                .find(|&&s| s > -rate)
                .copied()
                .unwrap_or(-rate)
        } else {
            1.
        };
        detail.player.play_at(-backwards);
        cx.notify();
    }

    /// Steps to the next playback speed, which then holds for the session.
    fn cycle_speed(&mut self, cx: &mut Context<Self>) {
        let mut playback = PLAYBACK.get();
        let next = SPEEDS
            .iter()
            .position(|&s| s == playback.speed)
            .map_or(0, |i| (i + 1) % SPEEDS.len());
        playback.speed = SPEEDS[next];
        PLAYBACK.set(playback);
        if let Some(detail) = &self.detail {
            detail.player.set_speed(playback.speed);
        }
        cx.notify();
    }

    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        let mut playback = PLAYBACK.get();
        playback.muted = !playback.muted;
        PLAYBACK.set(playback);
        if let Some(detail) = &self.detail {
            detail.player.set_muted(playback.muted);
        }
        cx.notify();
    }

    /// Sets the volume from a point on its slider; turning it up unmutes. Dragging it to zero
    /// mutes instead, keeping the last audible volume for Unmute to bring back.
    fn set_volume_at(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        let Some(bounds) = detail.volume_bar.get() else {
            return;
        };
        let volume = (f32::from(x - bounds.origin.x) / f32::from(bounds.size.width)).clamp(0., 1.);
        let mut playback = PLAYBACK.get();
        playback.muted = volume == 0.;
        if !playback.muted {
            playback.volume = volume;
        }
        PLAYBACK.set(playback);
        detail.player.set_volume(playback.volume);
        detail.player.set_muted(playback.muted);
        cx.notify();
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
        if self.detail.as_ref().is_some_and(|d| d.adjusting_volume) {
            if event.pressed_button == Some(MouseButton::Left) {
                self.set_volume_at(event.position.x, cx);
            } else if let Some(detail) = &mut self.detail {
                detail.adjusting_volume = false;
            }
            return;
        }
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
        detail.adjusting_volume = false;
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
                        // The selection follows the file to its new name.
                        for p in this
                            .selection
                            .iter_mut()
                            .chain(this.anchor.as_mut())
                            .chain(this.cursor.as_mut())
                            .filter(|p| **p == path)
                        {
                            *p = to.clone();
                        }
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
        let mut trashed = Vec::new();
        for path in &paths {
            let entry = self
                .items
                .iter()
                .find(|i| &i.entry.path == path)
                .map(|i| i.entry.clone());
            match self.state.update(cx, |s, cx| s.trash_recording(path, cx)) {
                Ok(to) => {
                    if self.detail.as_ref().is_some_and(|d| &d.path == path) {
                        self.close_detail(cx);
                    }
                    trashed.extend(entry.map(|e| (e, to)));
                }
                Err(err) => self.report(err),
            }
        }
        if !trashed.is_empty() {
            self.offer_undo(trashed, cx);
        }
        cx.notify();
    }

    /// Shows the Undo bar for a few seconds. Trashing more while it's up adds to it, so Undo
    /// brings back everything that went.
    fn offer_undo(&mut self, trashed: Vec<(Entry, PathBuf)>, cx: &mut Context<Self>) {
        let shown = Instant::now();
        let mut all = self.undo.take().map(|u| u.trashed).unwrap_or_default();
        all.extend(trashed);
        self.undo = Some(Undo {
            trashed: all,
            shown,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_FOR).await;
            let _ = this.update(cx, |this, cx| {
                if this.undo.as_ref().is_some_and(|u| u.shown == shown) {
                    this.undo = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Puts the recordings in the Undo bar back, and selects them.
    fn undo_trash(&mut self, cx: &mut Context<Self>) {
        let Some(undo) = self.undo.take() else {
            return;
        };
        self.error = None;
        let mut restored = Vec::new();
        for (entry, from) in undo.trashed {
            let path = entry.path.clone();
            match self
                .state
                .update(cx, |s, cx| s.restore_recording(entry, &from, cx))
            {
                Ok(()) => restored.push(path),
                Err(err) => self.report(err),
            }
        }
        if let Some(first) = restored.first() {
            self.anchor = Some(first.clone());
            self.cursor = Some(first.clone());
            self.selection = restored;
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

    /// The menu for the card at `index`: for the whole selection when the card is part of it,
    /// else for that card alone, which becomes the selection, as in Finder.
    fn open_card_menu(&mut self, index: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(path) = self.items.get(index).map(|i| i.entry.path.clone()) else {
            return;
        };
        if !self.selection.contains(&path) {
            self.select(index, Select::Only, cx);
        }
        let paths = self.selected_paths();
        let actions = match paths.as_slice() {
            [one] => self.card_actions(one, cx),
            _ => vec![Some(Action::Reveal), None, Some(Action::Trash)],
        };
        self.menu = Some(Menu { paths, at, actions });
        cx.notify();
    }

    /// Runs the model on `path` again. The transcript it has, even an empty one, is forgotten
    /// first, in memory and on disk, or transcribing would just hand it back.
    fn retranscribe(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.state.read(cx).manual_model().is_none() {
            return;
        }
        crate::transcript::forget_cached(&path);
        self.state.update(cx, |s, cx| {
            s.transcripts.remove(&path);
            s.transcribe_now(path, cx);
        });
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
            Action::Open => self.open_detail(path, 0., false, window, cx),
            Action::Edit => self.edit(path, cx),
            Action::CopyFile => native::copy_file_to_pasteboard(&path),
            Action::CopyTranscript => self.copy_transcript(&path, cx),
            Action::Transcribe | Action::TranscribeAgain => self.retranscribe(path, cx),
            Action::Reveal => native::reveal_all_in_finder(&menu.paths),
            Action::Rename => self.rename(path, window, cx),
            Action::Trash => self.trash_and_select_next(menu.paths, cx),
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
        let selected = self.selection.contains(&path);
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
                    .border_color(theme.hairline)
                    .when(selected, |d| d.border_2().border_color(theme.accent)),
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
                    .child(
                        more_button(("more", index), theme)
                            .tooltip(tip("More"))
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.open_card_menu(index, event.position(), cx);
                            })),
                    ),
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
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.open_card_menu(index, event.position, cx)
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.on_card_click(index, event, window, cx)
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
                    // The rest wait in a menu, so the header fits narrow windows.
                    .child(
                        header_button("more", "⋯")
                            .tooltip(tip("More"))
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                this.menu = Some(Menu {
                                    paths: vec![path.clone()],
                                    at: event.position(),
                                    actions: vec![
                                        Some(Action::Reveal),
                                        Some(Action::Rename),
                                        None,
                                        Some(Action::Trash),
                                    ],
                                });
                                cx.notify();
                            })),
                    ),
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
        let volume_bar = detail.volume_bar.clone();
        let playback = PLAYBACK.get();
        let volume = if playback.muted { 0. } else { playback.volume };
        // The rate actually playing, which J and L change for a while; else the speed play
        // will use.
        let rate = detail.player.rate();
        let speed = if rate == 0. { playback.speed } else { rate };
        let speed_label = if speed < 0. {
            format!("−{}x", -speed)
        } else {
            format!("{speed}x")
        };

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
            )
            .child(
                div()
                    .id("speed")
                    .h(px(24.))
                    .min_w(px(44.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if speed == 1. {
                        theme.text_muted
                    } else {
                        theme.text
                    })
                    .hover(|s| s.bg(theme.control_hover))
                    .tooltip(tip("Playback speed"))
                    .on_click(cx.listener(|this, _, _, cx| this.cycle_speed(cx)))
                    .child(speed_label),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .id("mute")
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.control_hover))
                            .tooltip(tip(if playback.muted { "Unmute" } else { "Mute" }))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_mute(cx)))
                            .child(icon(
                                "speaker",
                                14.,
                                if volume == 0. {
                                    theme.text_muted.opacity(0.5)
                                } else {
                                    theme.text
                                },
                            )),
                    )
                    .child(
                        div()
                            .id("volume")
                            .tooltip(tip("Volume"))
                            .w(px(VOLUME_WIDTH))
                            .h(px(20.))
                            .relative()
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    if let Some(detail) = &mut this.detail {
                                        detail.adjusting_volume = true;
                                    }
                                    this.set_volume_at(event.position.x, cx);
                                }),
                            )
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
                                            .w(relative(volume))
                                            .rounded(px(2.))
                                            .bg(theme.text_muted),
                                    ),
                            )
                            .child(
                                canvas(move |b, _, _| volume_bar.set(Some(b)), |_, _, _, _| {})
                                    .absolute()
                                    .inset_0(),
                            ),
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
    /// No recordings yet: how to make one, with the actual shortcut, and a button that does.
    fn empty_state(&self, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        let binding = &self.state.read(cx).settings.hotkeys.toggle_recording;
        let how = if binding.is_empty() {
            "Start one from the Annuncia icon in the menu bar.".to_string()
        } else {
            format!(
                "Press {} to start and stop a recording, from any app.",
                hotkeys::display_binding(binding)
            )
        };
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
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("No recordings yet"),
            )
            .child(
                div()
                    .max_w(px(320.))
                    .text_size(px(12.5))
                    .text_color(theme.text_muted)
                    .text_center()
                    .child(how),
            )
            .child(div().pt(px(4.)).child(
                button("new-recording", "New recording", true, theme).on_click(cx.listener(
                    |this, _, _, cx| this.state.update(cx, |s, cx| s.toggle_launcher(cx)),
                )),
            ))
            .into_any_element()
    }

    /// "Moved … to the Trash" [Undo], floating at the bottom of the window.
    fn undo_bar(&self, theme: Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let undo = self.undo.as_ref()?;
        let message = match undo.trashed.as_slice() {
            [(entry, _)] => format!(
                "Moved “{}” to the Trash",
                full_title(&entry.path, entry.created)
            ),
            all => format!("Moved {} recordings to the Trash", all.len()),
        };
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(20.))
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("undo-bar")
                        .occlude()
                        .max_w(px(520.))
                        .flex()
                        .items_center()
                        .gap(px(14.))
                        .pl(px(16.))
                        .pr(px(6.))
                        .py(px(6.))
                        .rounded(px(10.))
                        .bg(theme.surface)
                        .border_1()
                        .border_color(theme.hairline)
                        .shadow_lg()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(13.))
                                .text_color(theme.text)
                                .child(message),
                        )
                        .child(
                            button("undo", "Undo", false, theme)
                                .on_click(cx.listener(|this, _, _, cx| this.undo_trash(cx))),
                        ),
                )
                .into_any_element(),
        )
    }

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
                        this.open_detail(path.clone(), 0., false, window, cx)
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
            let expanded = self.expanded.contains(&path);
            let shown = if expanded { hits.len() } else { SHOWN_MATCHES };
            let lines = transcript.map(|t| {
                hits.iter()
                    .take(shown)
                    .enumerate()
                    .map(|(k, hit)| {
                        let at = t.pieces[hit.start].start;
                        let (text, matched) = snippet(&t, hit);
                        let path = path.clone();
                        div()
                            .id(SharedString::from(format!("hit-{i}-{k}")))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.control_hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_detail(path.clone(), at, true, window, cx)
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
            let more = hits.len().saturating_sub(shown);
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
                        // In a row of its own, so it's only as wide as its label.
                        d.child(
                            div().flex().child(
                                div()
                                    .id(("more-matches", i))
                                    .px(px(8.))
                                    .py(px(2.))
                                    .rounded(px(6.))
                                    .cursor_pointer()
                                    .text_size(px(12.))
                                    .text_color(theme.text_muted)
                                    .hover(|s| s.bg(theme.control_hover).text_color(theme.text))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.expanded.push(path.clone());
                                        cx.notify();
                                    }))
                                    .child(format!("Show {more} more")),
                            ),
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
        let undo = self.undo_bar(theme, cx);
        if self.detail.is_some() {
            return root
                .child(self.detail_header(theme, cx))
                .child(self.detail_body(theme, cx))
                .children(undo)
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
            self.empty_state(theme, cx)
        } else {
            self.columns = columns;
            // Labels and rows are the scroll view's own children, so the arrow keys can scroll
            // a row into view.
            let mut children = Vec::new();
            for (n, rows) in self.sections(columns).into_iter().enumerate() {
                let day = day_of(&self.items[rows[0].start]);
                children.push(
                    div()
                        // The gap between days, on top of the one between rows.
                        .when(n > 0, |d| d.mt(px(28. - GAP)))
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(day_label(day))
                        .into_any_element(),
                );
                for row in rows {
                    children.push(
                        div()
                            .flex()
                            .gap(px(GAP))
                            .children(row.map(|i| self.card(i, &self.items[i], width, theme, cx)))
                            .into_any_element(),
                    );
                }
            }
            div()
                .id("library-scroll")
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.grid_scroll)
                .p(px(PADDING))
                .flex()
                .flex_col()
                .gap(px(GAP))
                .children(children)
                .into_any_element()
        };

        root.child(header).child(body).children(undo).children(menu)
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
