use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Application, Bounds, Context, Entity, Global, KeyBinding, Menu, MenuItem,
    SharedString, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, point, px, size,
};

use crate::camera::{self, Camera, CameraDevice, FrameSlot};
use crate::hotkeys::{HotkeyAction, Hotkeys};
use crate::library::Library;
use crate::models::{Download, Transcription};
use crate::native::{self, OverlayLevel, TopLeftRect, WindowSetup};
use crate::popout::PopoutSlot;
use crate::recorder::{self, RecorderEvent, Session, StartRequest, Target, WindowChoice};
use crate::settings::{CaptureMode, Settings};
use crate::transcriber::Transcriber;
use crate::transcript::Transcript;
use crate::tray::{Tray, TrayAction, TrayState};
use crate::ui::bubble::Bubble;
use crate::ui::editor::{self, Editor};
use crate::ui::hud::{self, Hud};
use crate::ui::launcher::{self, Launcher};
use crate::ui::library::LibraryView;

/// Owns the controller for the life of the app.
struct AppState {
    _state: Entity<Annuncia>,
}
impl Global for AppState {}

pub enum Phase {
    Idle,
    Countdown {
        until: Instant,
        target: Target,
    },
    Starting,
    Recording {
        /// Time recorded before the current stretch.
        recorded: Duration,
        /// When the current stretch started; `None` while paused.
        resumed: Option<Instant>,
    },
    Stopping {
        end: End,
    },
    Saved {
        path: PathBuf,
        until: Instant,
    },
}

impl Phase {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Phase::Countdown { .. }
                | Phase::Starting
                | Phase::Recording { .. }
                | Phase::Stopping { .. }
        )
    }

    /// Recorded time so far, not counting pauses.
    pub fn recorded(&self) -> Duration {
        match self {
            Phase::Recording { recorded, resumed } => {
                *recorded + resumed.map_or(Duration::ZERO, |t| t.elapsed())
            }
            _ => Duration::ZERO,
        }
    }

    pub fn is_paused(&self) -> bool {
        matches!(self, Phase::Recording { resumed: None, .. })
    }
}

#[derive(Clone)]
pub enum TranscriptState {
    Working,
    Ready(Arc<Transcript>),
    Failed(SharedString),
}

/// How a recording ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Save,
    /// Moves the recording to the Trash.
    Discard,
    /// Discards, then starts a new recording with the same settings.
    Restart,
}

#[derive(Clone)]
pub struct Notice {
    pub message: SharedString,
    pub action: Option<(SharedString, &'static str)>,
    pub until: Instant,
}

pub struct Annuncia {
    pub settings: Settings,
    pub phase: Phase,
    pub notice: Option<Notice>,
    pub windows: Vec<WindowChoice>,
    pub windows_loading: bool,
    pub selected_window: Option<u32>,
    pub cameras: Vec<CameraDevice>,
    pub microphones: Vec<(String, String)>,
    pub frames: Arc<FrameSlot>,
    /// Composited frames for the pop-out bubble.
    pub popout_frames: Arc<PopoutSlot>,
    pub hotkey_problems: Vec<String>,
    /// Transcripts of recordings made (or edited) this session, by file.
    pub transcripts: HashMap<PathBuf, TranscriptState>,
    /// The speech model being downloaded, if any.
    pub download: Option<Download>,
    pub download_error: Option<SharedString>,
    /// Download progress in thousandths last shown, to redraw only when it moves.
    download_shown: u32,
    transcriber: Transcriber,
    camera: Option<Camera>,
    /// Taken out while a pause or resume runs on a worker thread.
    session: Option<Session>,
    /// Stop, discard or restart asked for while a pause or resume was still running.
    pending_finish: Option<End>,
    events_tx: Sender<RecorderEvent>,
    events_rx: Receiver<RecorderEvent>,
    tray: Tray,
    hotkeys: Option<Hotkeys>,
    launcher: Option<WindowHandle<Launcher>>,
    launcher_closed_at: Option<Instant>,
    hud: Option<WindowHandle<Hud>>,
    bubble: Option<WindowHandle<Bubble>>,
    bubble_hidden: bool,
    /// Hidden from the bubble's own control; lasts until the panel and recording are done.
    bubble_dismissed: bool,
    /// Where the bubble should open (the target window's corner in window recordings).
    bubble_anchor: Option<TopLeftRect>,
    /// Set after a camera start failure; cleared when the camera settings change.
    camera_failed: bool,
    launcher_opening: bool,
    quit_when_stopped: bool,
    editor_window: Option<WindowHandle<Editor>>,
    /// The recording open in the editor, which mustn't be renamed or trashed under it.
    pub editing: Option<PathBuf>,
    pub library: Library,
    /// Bumped whenever a recording is added or replaced, so the library window refreshes.
    pub library_revision: u64,
    library_window: Option<WindowHandle<LibraryView>>,
    library_opening: bool,
    last_tick: u64,
}

gpui::actions!(annuncia, [Quit]);

pub fn run() {
    Application::new()
        .with_assets(crate::ui::icons::Assets)
        .run(|cx: &mut App| {
            native::set_accessory_activation_policy();
            native::use_dark_appearance();
            let state = cx.new(Annuncia::new);
            // The menu bar only shows while the library makes Annuncia a regular app.
            cx.on_action({
                let state = state.clone();
                move |_: &Quit, cx| state.update(cx, |s, cx| s.quit(cx))
            });
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus(vec![Menu {
                name: "Annuncia".into(),
                items: vec![MenuItem::action("Quit Annuncia", Quit)],
            }]);
            cx.set_global(AppState { _state: state });
        });
}

impl Annuncia {
    fn new(cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        let _ = std::fs::create_dir_all(&settings.save_dir);
        let mut hotkeys = Hotkeys::new()
            .inspect_err(|err| eprintln!("annuncia: global shortcuts unavailable: {err}"))
            .ok();
        let hotkey_problems = hotkeys
            .as_mut()
            .map(|h| h.apply(&settings.hotkeys))
            .unwrap_or_default();
        let (events_tx, events_rx) = channel();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                if this.update(cx, |state, cx| state.pump(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // ggml aborts at exit if a loaded model still holds GPU buffers; release it first.
        cx.on_app_quit(|state: &mut Self, _| {
            state.transcriber.shutdown();
            async {}
        })
        .detach();

        Self {
            settings,
            phase: Phase::Idle,
            notice: None,
            windows: Vec::new(),
            windows_loading: false,
            selected_window: None,
            cameras: camera::devices(),
            microphones: recorder::microphones(),
            frames: Arc::new(FrameSlot::default()),
            popout_frames: Arc::new(PopoutSlot::default()),
            hotkey_problems,
            transcripts: HashMap::new(),
            download: None,
            download_error: None,
            download_shown: 0,
            transcriber: Transcriber::new(),
            camera: None,
            session: None,
            pending_finish: None,
            events_tx,
            events_rx,
            tray: Tray::new().expect("menu bar icon"),
            hotkeys,
            launcher: None,
            launcher_closed_at: None,
            hud: None,
            bubble: None,
            bubble_hidden: false,
            bubble_dismissed: false,
            bubble_anchor: None,
            camera_failed: false,
            launcher_opening: false,
            quit_when_stopped: false,
            editor_window: None,
            editing: None,
            library: Library::load(),
            library_revision: 0,
            library_window: None,
            library_opening: false,
            last_tick: u64::MAX,
        }
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        for action in self.tray.poll() {
            match action {
                TrayAction::Clicked | TrayAction::OpenLauncher => self.toggle_launcher(cx),
                TrayAction::ToggleRecording => self.toggle_recording(cx),
                TrayAction::TogglePause => self.toggle_pause(cx),
                TrayAction::Restart => self.restart_recording(cx),
                TrayAction::OpenFolder => {
                    let _ = std::fs::create_dir_all(&self.settings.save_dir);
                    native::open(&self.settings.save_dir);
                }
                TrayAction::OpenSettings => self.open_settings(cx),
                TrayAction::OpenLibrary => self.open_library(cx),
                TrayAction::Quit => self.quit(cx),
            }
        }
        let presses = self.hotkeys.as_ref().map(Hotkeys::poll).unwrap_or_default();
        for action in presses {
            match action {
                HotkeyAction::ToggleRecording => self.toggle_recording(cx),
                HotkeyAction::PauseRecording => self.toggle_pause(cx),
                HotkeyAction::RestartRecording => self.restart_recording(cx),
                HotkeyAction::OpenLauncher => self.toggle_launcher(cx),
                HotkeyAction::ToggleCamera => self.toggle_camera(cx),
                HotkeyAction::CancelRecording => self.cancel_recording(cx),
            }
        }
        while let Ok(event) = self.events_rx.try_recv() {
            self.on_recorder_event(event, cx);
        }
        if let Some(download) = &self.download {
            let shown = (download.fraction() * 1000.) as u32;
            if shown != self.download_shown {
                self.download_shown = shown;
                cx.notify();
            }
        }

        let now = Instant::now();
        match &self.phase {
            Phase::Countdown { until, target } => {
                if now >= *until {
                    let target = target.clone();
                    self.begin_capture(target, cx);
                } else {
                    let left = until.duration_since(now).as_secs() + 1;
                    if left != self.last_tick {
                        self.last_tick = left;
                        cx.notify();
                    }
                }
            }
            Phase::Recording { .. } => {
                let secs = self.phase.recorded().as_secs();
                if secs != self.last_tick {
                    self.last_tick = secs;
                    cx.notify();
                }
            }
            Phase::Saved { until, .. } if now >= *until => {
                self.phase = Phase::Idle;
                self.sync_windows(cx);
            }
            _ => {}
        }
        if self.notice.as_ref().is_some_and(|n| now >= n.until) {
            self.notice = None;
            self.sync_windows(cx);
        }
    }

    // MARK: recording

    pub fn toggle_recording(&mut self, cx: &mut Context<Self>) {
        match self.phase {
            Phase::Idle | Phase::Saved { .. } => self.start_recording(cx),
            Phase::Countdown { .. } => self.cancel_recording(cx),
            Phase::Recording { .. } => self.stop_recording(cx),
            Phase::Starting | Phase::Stopping { .. } => {}
        }
    }

    pub fn start_recording(&mut self, cx: &mut Context<Self>) {
        if self.phase.is_active() {
            return;
        }
        let target = match self.settings.capture_mode {
            CaptureMode::Screen => Target::Screen { display_id: None },
            CaptureMode::Window => match self.selected_window {
                Some(window_id) => Target::Window { window_id },
                None => {
                    // Nothing picked yet: ask in the panel instead of guessing.
                    self.show_notice("Pick a window to record", None, cx);
                    if self.launcher.is_none() {
                        self.toggle_launcher(cx);
                    }
                    return;
                }
            },
        };
        self.notice = None;
        self.close_launcher(cx);
        self.last_tick = u64::MAX;
        self.phase = if self.settings.countdown_secs == 0 {
            Phase::Starting
        } else {
            Phase::Countdown {
                until: Instant::now()
                    + Duration::from_secs(u64::from(self.settings.countdown_secs)),
                target: target.clone(),
            }
        };
        self.bubble_anchor = None;
        if let Target::Window { window_id } = &target {
            self.place_bubble_on_window(*window_id, cx);
        }
        self.sync_windows(cx);
        if matches!(self.phase, Phase::Starting) {
            // Let the HUD and bubble windows exist before we snapshot window ids.
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                let _ = this.update(cx, |state, cx| state.begin_capture(target, cx));
            })
            .detach();
        }
        cx.notify();
    }

    fn begin_capture(&mut self, target: Target, cx: &mut Context<Self>) {
        self.phase = Phase::Starting;
        let window_number = |handle: Option<gpui::AnyWindowHandle>, cx: &mut App| {
            handle.and_then(|h| {
                h.update(cx, |_, window, _| native::window_number(window))
                    .ok()
                    .flatten()
            })
        };
        let exclude_window_ids = [
            self.hud.map(|h| h.into()),
            self.launcher.map(|h| h.into()),
            self.editor_window.map(|h| h.into()),
            self.library_window.map(|h| h.into()),
        ]
        .into_iter()
        .filter_map(|h| window_number(h, cx))
        .collect();
        let bubble_window_id = window_number(self.bubble.map(|h| h.into()), cx);
        let s = &self.settings;
        let stamp = chrono::Local::now().format("%Y-%m-%d at %H.%M.%S");
        let request = StartRequest {
            target,
            path: s.save_dir.join(format!("Annuncia {stamp}.mp4")),
            exclude_window_ids,
            bubble_window_id,
            fps: s.fps,
            codec: s.codec,
            show_cursor: s.show_cursor,
            system_audio: s.system_audio,
            microphone: s.mic_enabled.then(|| s.mic_device.clone()),
        };
        let events = self.events_tx.clone();
        let task = cx.background_spawn(async move { recorder::start(request, events) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| match result {
                // Cancelled (or failed) while starting: stop the stray stream.
                Ok(session) if !matches!(state.phase, Phase::Starting) => {
                    cx.background_spawn(async move {
                        let _ = session.finish(false);
                    })
                    .detach();
                }
                Ok(session) => {
                    state.session = Some(session);
                    state.phase = Phase::Recording {
                        recorded: Duration::ZERO,
                        resumed: Some(Instant::now()),
                    };
                    state.last_tick = u64::MAX;
                    state.sync_tray();
                    state.sync_windows(cx);
                    cx.notify();
                }
                Err(err) => {
                    state.phase = Phase::Idle;
                    let action = format!("{err:#}").contains("Screen Recording").then_some((
                        "Open Settings".into(),
                        native::SCREEN_RECORDING_SETTINGS_URL,
                    ));
                    state.show_notice(format!("{err:#}"), action, cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.finish(End::Save, cx);
    }

    pub fn restart_recording(&mut self, cx: &mut Context<Self>) {
        self.finish(End::Restart, cx);
    }

    pub fn cancel_recording(&mut self, cx: &mut Context<Self>) {
        match self.phase {
            Phase::Countdown { .. } => {
                self.phase = Phase::Idle;
                self.sync_windows(cx);
            }
            Phase::Recording { .. } => self.finish(End::Discard, cx),
            _ => {}
        }
    }

    /// Pauses a running recording or resumes a paused one. The stream keeps running while
    /// paused, so the camera bubble stays live and resuming is instant.
    pub fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        let Phase::Recording { recorded, resumed } = &mut self.phase else {
            return;
        };
        // A pause or resume is still running.
        let Some(mut session) = self.session.take() else {
            return;
        };
        let pausing = resumed.is_some();
        // The timer stops at once; on resume it restarts when the new part has started.
        if let Some(since) = resumed.take() {
            *recorded += since.elapsed();
        }
        self.sync_tray();
        let task = cx.background_spawn(async move {
            let result = if pausing {
                session.pause()
            } else {
                session.resume()
            };
            (session, result)
        });
        cx.spawn(async move |this, cx| {
            let (session, result) = task.await;
            let _ = this.update(cx, |state, cx| {
                state.session = Some(session);
                if let Phase::Recording { resumed, .. } = &mut state.phase {
                    // Resuming worked, or pausing failed and the part is still recording.
                    if pausing == result.is_err() {
                        *resumed = Some(Instant::now());
                    }
                }
                if let Err(err) = result {
                    state.set_notice(format!("{err:#}"), None);
                }
                state.sync_tray();
                if let Some(end) = state.pending_finish.take() {
                    state.finish(end, cx);
                }
                state.sync_windows(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn finish(&mut self, end: End, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Recording { .. }) {
            return;
        }
        let Some(session) = self.session.take() else {
            // A pause or resume is running; finish when it's done.
            self.pending_finish = Some(end);
            return;
        };
        self.phase = Phase::Stopping { end };
        self.sync_tray();
        let task = cx.background_spawn(async move { session.finish(end == End::Save) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                if end == End::Restart && !state.quit_when_stopped {
                    // The bubble stays up through `Stopping`, so going straight into the
                    // countdown doesn't flicker it.
                    state.phase = Phase::Idle;
                    if let Err(err) = result {
                        state.set_notice(format!("{err:#}"), None);
                    }
                    state.start_recording(cx);
                    return;
                }
                if let Ok(Some(path)) = &result {
                    state.library.add(path.clone());
                    state.library_revision += 1;
                }
                if let Ok(Some(path)) = &result
                    && !state.quit_when_stopped
                {
                    state.transcribe(path.clone(), cx);
                    if state.settings.open_editor {
                        state.open_editor(path.clone(), cx);
                    }
                }
                state.phase = match result {
                    Ok(Some(path)) => Phase::Saved {
                        path,
                        until: Instant::now() + Duration::from_secs(8),
                    },
                    Ok(None) => Phase::Idle,
                    Err(err) => {
                        state.set_notice(format!("{err:#}"), None);
                        Phase::Idle
                    }
                };
                state.sync_windows(cx);
                if state.quit_when_stopped {
                    cx.quit();
                }
                cx.notify();
            });
        })
        .detach();
        self.sync_windows(cx);
        cx.notify();
    }

    fn on_recorder_event(&mut self, event: RecorderEvent, cx: &mut Context<Self>) {
        match event {
            RecorderEvent::Failed(err) => {
                self.set_notice(format!("Recording failed: {err}"), None);
                // Keep whatever was recorded before the failure.
                if matches!(self.phase, Phase::Recording { .. }) {
                    self.finish(End::Save, cx);
                } else {
                    self.sync_windows(cx);
                }
            }
        }
        cx.notify();
    }

    fn sync_tray(&mut self) {
        self.tray.set_state(match self.phase {
            Phase::Recording { resumed: None, .. } => TrayState::Paused,
            Phase::Recording { .. } => TrayState::Recording,
            _ => TrayState::Idle,
        });
    }

    pub fn dismiss_saved(&mut self, cx: &mut Context<Self>) {
        if matches!(self.phase, Phase::Saved { .. }) {
            self.phase = Phase::Idle;
        }
        self.notice = None;
        self.sync_windows(cx);
    }

    /// Keeps the "saved" HUD up while the pointer is on it.
    pub fn hold_saved(&mut self) {
        if let Phase::Saved { until, .. } = &mut self.phase {
            *until = (*until).max(Instant::now() + Duration::from_secs(3));
        }
    }

    pub fn show_notice(
        &mut self,
        message: impl Into<SharedString>,
        action: Option<(SharedString, &'static str)>,
        cx: &mut Context<Self>,
    ) {
        self.set_notice(message, action);
        self.sync_windows(cx);
        cx.notify();
    }

    /// Sets the notice without reconciling windows; for use inside `sync_windows`.
    fn set_notice(
        &mut self,
        message: impl Into<SharedString>,
        action: Option<(SharedString, &'static str)>,
    ) {
        self.notice = Some(Notice {
            message: message.into(),
            action,
            until: Instant::now() + Duration::from_secs(10),
        });
    }

    // MARK: settings & devices

    pub fn update_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        let before = self.settings.clone();
        f(&mut self.settings);
        if self.settings == before {
            return;
        }
        if self.settings.hotkeys != before.hotkeys
            && let Some(hotkeys) = self.hotkeys.as_mut()
        {
            self.hotkey_problems = hotkeys.apply(&self.settings.hotkeys);
        }
        if let Err(err) = self.settings.save() {
            self.show_notice(format!("Couldn't save settings: {err:#}"), None, cx);
        }
        let camera_changed = self.settings.camera_device != before.camera_device;
        if camera_changed || self.settings.camera_enabled != before.camera_enabled {
            self.camera_failed = false;
        }
        if camera_changed && self.camera.is_some() {
            self.camera = None;
        }
        // The pop-out bubble's shape and compositor depend on these, so start both afresh.
        let reshaped = self.settings.bubble_popout != before.bubble_popout
            || (self.settings.bubble_popout
                && (self.settings.bubble_size != before.bubble_size
                    || self.settings.camera_enabled != before.camera_enabled));
        if reshaped {
            self.camera = None;
            self.reopen_bubble(cx);
        }
        self.sync_windows(cx);
        cx.notify();
    }

    pub fn toggle_camera(&mut self, cx: &mut Context<Self>) {
        // After the bubble was hidden, the shortcut brings it back first.
        if self.bubble_dismissed {
            self.bubble_dismissed = false;
            self.sync_windows(cx);
            cx.notify();
            return;
        }
        self.update_settings(cx, |s| s.camera_enabled = !s.camera_enabled);
    }

    pub fn hide_bubble(&mut self, cx: &mut Context<Self>) {
        self.bubble_dismissed = true;
        self.sync_windows(cx);
        cx.notify();
    }

    pub fn refresh_devices(&mut self, cx: &mut Context<Self>) {
        self.cameras = camera::devices();
        self.microphones = recorder::microphones();
        self.windows_loading = true;
        let task = cx.background_spawn(async { recorder::list_windows() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                state.windows_loading = false;
                match result {
                    Ok(windows) => {
                        if state
                            .selected_window
                            .is_some_and(|id| !windows.iter().any(|w| w.id == id))
                        {
                            state.selected_window = None;
                        }
                        state.windows = windows;
                    }
                    Err(err) => state.show_notice(
                        format!("{err:#}"),
                        Some((
                            "Open Settings".into(),
                            native::SCREEN_RECORDING_SETTINGS_URL,
                        )),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // MARK: windows

    pub fn toggle_launcher(&mut self, cx: &mut Context<Self>) {
        if self.phase.is_active() {
            return;
        }
        if self.launcher.is_some() {
            self.close_launcher(cx);
            return;
        }
        // Clicking the menu bar icon first blurs the panel (closing it); don't reopen it.
        if self
            .launcher_closed_at
            .is_some_and(|t| t.elapsed() < Duration::from_millis(400))
        {
            return;
        }
        if self.launcher_opening {
            return;
        }
        self.launcher_opening = true;
        self.refresh_devices(cx);
        let anchor = self.tray.frame();
        let state = cx.entity();
        // A non-activating panel can take key focus without activating the app, so the app
        // being recorded stays frontmost.
        cx.defer(move |cx| {
            let handle = launcher::open(state.clone(), anchor, cx);
            state.update(cx, |s, cx| {
                s.launcher = handle;
                s.launcher_opening = false;
                s.sync_windows(cx);
            });
        });
    }

    pub fn close_launcher(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.launcher.take() {
            self.launcher_closed_at = Some(Instant::now());
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
        self.sync_windows(cx);
    }

    /// Settings lives in the library window.
    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.show_library(true, cx);
    }

    pub fn open_editor(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // Any recording opened in the editor gets a transcript, not just new ones.
        self.transcribe(path.clone(), cx);
        // One editor at a time: a new recording replaces whatever was open.
        if let Some(handle) = self.editor_window.take() {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
        self.editing = Some(path.clone());
        let state = cx.entity();
        when_active(cx, move |active, cx| {
            let handle = editor::open(state.clone(), path, active, cx);
            state.update(cx, |s, _| s.editor_window = handle);
        });
    }

    pub fn open_library(&mut self, cx: &mut Context<Self>) {
        self.show_library(false, cx);
    }

    /// The library window, on its recordings or on Settings. It makes Annuncia a regular app
    /// while it's open: in the Dock and in ⌘-Tab.
    fn show_library(&mut self, settings: bool, cx: &mut Context<Self>) {
        self.close_launcher(cx);
        self.library.prune();
        native::set_regular_activation_policy();
        let show = move |handle: WindowHandle<LibraryView>, cx: &mut App| {
            let _ = handle.update(cx, |view, window, cx| {
                window.activate_window();
                if settings {
                    view.show_settings(None, window, cx);
                } else {
                    view.close_settings(window, cx);
                }
            });
        };
        if let Some(handle) = self.library_window {
            when_active(cx, move |active, cx| {
                if active {
                    show(handle, cx);
                }
            });
            return;
        }
        if self.library_opening {
            return;
        }
        self.library_opening = true;
        let state = cx.entity();
        when_active(cx, move |active, cx| {
            let handle = crate::ui::library::open(state.clone(), active, cx);
            state.update(cx, |s, _| {
                s.library_window = handle;
                s.library_opening = false;
            });
            if let Some(handle) = handle
                && settings
            {
                show(handle, cx);
            }
        });
    }

    pub fn library_closed(&mut self) {
        self.library_window = None;
        native::set_accessory_activation_policy();
    }

    /// Renames a recording's file in place, carrying its transcript and thumbnail over.
    pub fn rename_recording(
        &mut self,
        path: &Path,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<PathBuf, SharedString> {
        if self.editing.as_deref() == Some(path) {
            return Err("Close the editor before renaming this recording".into());
        }
        if name.is_empty() || name.starts_with('.') || name.contains(['/', ':']) {
            return Err("Names can't be empty, start with a dot, or contain / or :".into());
        }
        let extension = path
            .extension()
            .map_or("mp4".into(), |e| e.to_string_lossy());
        let to = path.with_file_name(format!("{name}.{extension}"));
        if to == path {
            return Ok(to);
        }
        if to.exists() {
            return Err(format!("There's already a file named “{name}”").into());
        }
        // The caches are keyed by path, so look them up before the file moves.
        let transcript = crate::transcript::load_cached(path);
        let thumbnail = crate::library::thumbnail_path(path);
        std::fs::rename(path, &to).map_err(|err| SharedString::from(format!("{err}")))?;
        if let Some(transcript) = &transcript {
            crate::transcript::save_cached(&to, transcript);
        }
        if let (Some(from), Some(dest)) = (thumbnail, crate::library::thumbnail_path(&to)) {
            let _ = std::fs::rename(from, dest);
        }
        if let Some(state) = self.transcripts.remove(path) {
            self.transcripts.insert(to.clone(), state);
        }
        if let Phase::Saved { path: saved, .. } = &mut self.phase
            && saved == path
        {
            *saved = to.clone();
        }
        self.library.rename(path, to.clone());
        self.library_revision += 1;
        cx.notify();
        Ok(to)
    }

    pub fn trash_recording(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        if self.editing.as_deref() == Some(path) {
            return Err("Close the editor before moving this recording to the Trash".into());
        }
        if !native::trash(path) {
            return Err("Couldn't move the recording to the Trash".into());
        }
        self.transcripts.remove(path);
        if matches!(&self.phase, Phase::Saved { path: saved, .. } if saved == path) {
            self.dismiss_saved(cx);
        }
        self.library.remove(path);
        self.library_revision += 1;
        cx.notify();
        Ok(())
    }

    pub fn editor_closed(&mut self) {
        self.editor_window = None;
        self.editing = None;
    }

    /// The editor replaced the recording with its edit: offer it in the notch like a new one.
    pub fn edit_saved(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.editor_window = None;
        self.editing = None;
        // The edit replaced the file, so its old transcript no longer lines up.
        self.transcripts.remove(&path);
        self.library_revision += 1;
        self.transcribe(path.clone(), cx);
        if !self.phase.is_active() {
            self.phase = Phase::Saved {
                path,
                until: Instant::now() + Duration::from_secs(8),
            };
            self.sync_windows(cx);
        }
        cx.notify();
    }

    // MARK: transcripts

    /// Transcribes `path` in the background when transcription is on and its model is ready.
    pub fn transcribe(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let model = self.settings.transcription;
        if model == Transcription::Off || !model.is_installed() {
            return;
        }
        self.transcribe_with(path, model, cx);
    }

    /// The model for transcribing by hand: the one chosen in Settings, or else any that's
    /// downloaded, so it works even with automatic transcription off.
    pub fn manual_model(&self) -> Option<Transcription> {
        let chosen = self.settings.transcription;
        if chosen != Transcription::Off && chosen.is_installed() {
            return Some(chosen);
        }
        [Transcription::English, Transcription::Multilingual]
            .into_iter()
            .find(|m| m.is_installed())
    }

    pub fn transcribe_now(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(model) = self.manual_model() {
            self.transcribe_with(path, model, cx);
        }
    }

    /// Picks up a transcript saved in an earlier session.
    pub fn load_transcript(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.transcripts.contains_key(path) {
            return;
        }
        if let Some(transcript) = crate::transcript::load_cached(path) {
            self.transcripts.insert(
                path.to_path_buf(),
                TranscriptState::Ready(Arc::new(transcript)),
            );
            cx.notify();
        }
    }

    fn transcribe_with(&mut self, path: PathBuf, model: Transcription, cx: &mut Context<Self>) {
        if matches!(
            self.transcripts.get(&path),
            Some(TranscriptState::Working | TranscriptState::Ready(_))
        ) {
            return;
        }
        self.transcripts
            .insert(path.clone(), TranscriptState::Working);
        let result = self.transcriber.transcribe(path.clone(), model);
        cx.spawn(async move |this, cx| {
            let result = result.recv().await;
            let _ = this.update(cx, |state, cx| {
                let entry = match result {
                    Ok(Ok(transcript)) => TranscriptState::Ready(Arc::new(transcript)),
                    Ok(Err(err)) => TranscriptState::Failed(format!("{err:#}").into()),
                    Err(_) => TranscriptState::Failed("The transcriber stopped".into()),
                };
                state.transcripts.insert(path, entry);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Copies `path`'s transcript as Markdown; returns whether there was one to copy.
    pub fn copy_transcript(&self, path: &Path) -> bool {
        let transcript = match self.transcripts.get(path) {
            Some(TranscriptState::Ready(transcript)) => transcript.clone(),
            // Recordings from earlier sessions only have the cached copy.
            _ => match crate::transcript::load_cached(path) {
                Some(transcript) if !transcript.is_empty() => Arc::new(transcript),
                _ => return false,
            },
        };
        let title = path
            .file_stem()
            .map_or("Recording".into(), |s| s.to_string_lossy().into_owned());
        native::copy_text_to_pasteboard(&transcript.markdown(&title));
        true
    }

    /// Picks the transcription model; one that isn't downloaded yet starts downloading.
    pub fn set_transcription(&mut self, model: Transcription, cx: &mut Context<Self>) {
        if self.download.as_ref().is_some_and(|d| d.model != model) {
            self.cancel_download();
        }
        self.download_error = None;
        self.update_settings(cx, |s| s.transcription = model);
        if model != Transcription::Off && !model.is_installed() && self.download.is_none() {
            self.start_download(model, cx);
        }
    }

    fn start_download(&mut self, model: Transcription, cx: &mut Context<Self>) {
        let Some(download) = Download::new(model) else {
            return;
        };
        self.download = Some(download.clone());
        self.download_shown = 0;
        let task = cx.background_spawn(async move { download.run() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |state, cx| {
                state.download = None;
                match result {
                    Ok(()) => {
                        // Transcribe the recording waiting in the notch, if there is one.
                        if let Phase::Saved { path, .. } = &state.phase {
                            let path = path.clone();
                            state.transcribe(path, cx);
                        }
                    }
                    Err(err) if !format!("{err}").contains("cancelled") => {
                        state.download_error = Some(format!("{err:#}").into());
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn retry_download(&mut self, cx: &mut Context<Self>) {
        self.download_error = None;
        let model = self.settings.transcription;
        if model != Transcription::Off && self.download.is_none() {
            self.start_download(model, cx);
        }
    }

    pub fn cancel_download(&mut self) {
        if let Some(download) = self.download.take() {
            download.cancel();
        }
    }

    /// Deletes the selected model's file and turns transcription off.
    pub fn remove_model(&mut self, cx: &mut Context<Self>) {
        let model = self.settings.transcription;
        self.cancel_download();
        // The worker may still hold the file open; releasing it first frees the memory too.
        self.transcriber.shutdown();
        self.transcriber = Transcriber::new();
        model.remove();
        self.update_settings(cx, |s| s.transcription = Transcription::Off);
    }

    /// While the settings window records a shortcut, ours must not swallow the keypress.
    pub fn suspend_hotkeys(&mut self) {
        if let Some(hotkeys) = self.hotkeys.as_mut() {
            hotkeys.apply(&crate::settings::HotkeySettings::none());
        }
    }

    pub fn resume_hotkeys(&mut self, cx: &mut Context<Self>) {
        if let Some(hotkeys) = self.hotkeys.as_mut() {
            self.hotkey_problems = hotkeys.apply(&self.settings.hotkeys);
        }
        cx.notify();
    }

    fn wants_bubble(&self) -> bool {
        let showing = self.launcher.is_some()
            || matches!(
                self.phase,
                Phase::Countdown { .. }
                    | Phase::Starting
                    | Phase::Recording { .. }
                    | Phase::Stopping { end: End::Restart }
            );
        showing
            && !self.bubble_dismissed
            && (self.settings.camera_enabled || self.settings.avatar_image.is_some())
    }

    /// Reconciles the HUD, bubble window and camera session with the current state.
    fn sync_windows(&mut self, cx: &mut Context<Self>) {
        let state = cx.entity();
        if self.launcher.is_none() && !self.phase.is_active() {
            self.bubble_dismissed = false;
        }

        let wants_camera =
            self.wants_bubble() && self.settings.camera_enabled && !self.camera_failed;
        if wants_camera && self.camera.is_none() {
            self.start_camera(cx);
        } else if !wants_camera && self.camera.is_some() {
            self.camera = None;
        }

        let wants_hud = !matches!(self.phase, Phase::Idle) || self.notice.is_some();
        match (wants_hud, self.hud) {
            (true, None) => {
                let state = state.clone();
                cx.defer(move |cx| {
                    if state.read(cx).hud.is_some() {
                        return;
                    }
                    let handle = hud::open(state.clone(), cx);
                    state.update(cx, |s, _| s.hud = handle);
                });
            }
            (false, Some(handle)) => {
                self.hud = None;
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                });
            }
            _ => {}
        }

        let wants_bubble = self.wants_bubble();
        let recording = self.phase.is_active();
        match (wants_bubble, self.bubble) {
            (true, None) => {
                let state = state.clone();
                let diameter = self.settings.bubble_size.diameter();
                let anchor = self.bubble_anchor;
                let popout = self.popout_active();
                cx.defer(move |cx| {
                    if state.read(cx).bubble.is_some() {
                        return;
                    }
                    let handle =
                        crate::ui::bubble::open(state.clone(), diameter, anchor, popout, cx);
                    state.update(cx, |s, _| {
                        s.bubble = handle;
                        s.bubble_hidden = false;
                    });
                });
            }
            (true, Some(handle)) if self.bubble_hidden => {
                self.bubble_hidden = false;
                cx.defer(move |cx| {
                    let _ =
                        handle.update(cx, |_, window, cx| native::set_visible(window, cx, true));
                });
            }
            // While recording, hide rather than close so a window recording keeps the bubble's id.
            (false, Some(handle)) if recording => {
                if !self.bubble_hidden {
                    self.bubble_hidden = true;
                    cx.defer(move |cx| {
                        let _ = handle
                            .update(cx, |_, window, cx| native::set_visible(window, cx, false));
                    });
                }
            }
            (false, Some(handle)) => {
                self.bubble = None;
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                });
            }
            _ => {}
        }
    }

    fn start_camera(&mut self, cx: &mut Context<Self>) {
        match camera::access() {
            camera::Access::Granted => {
                let popout = self.popout_active().then(|| {
                    (
                        f64::from(self.settings.bubble_size.diameter()),
                        self.popout_frames.clone(),
                    )
                });
                match Camera::start(
                    self.settings.camera_device.as_deref(),
                    self.frames.clone(),
                    popout,
                ) {
                    Ok(camera) => self.camera = Some(camera),
                    // Don't retry on every sync (e.g. no camera on a Mac mini); the bubble
                    // falls back to the avatar or "Camera off".
                    Err(err) => {
                        self.camera_failed = true;
                        self.set_notice(format!("{err:#}"), None);
                    }
                }
            }
            camera::Access::Undetermined => {
                let (tx, rx) = smol::channel::bounded(1);
                camera::request_access(move |granted| {
                    let _ = tx.try_send(granted);
                });
                cx.spawn(async move |this, cx| {
                    let granted = rx.recv().await.unwrap_or(false);
                    let _ = this.update(cx, |state, cx| {
                        if granted {
                            state.sync_windows(cx);
                        } else {
                            state.update_settings(cx, |s| s.camera_enabled = false);
                        }
                    });
                })
                .detach();
            }
            camera::Access::Denied => {
                self.camera_failed = true;
                self.set_notice(
                    "Camera access is off for Annuncia",
                    Some(("Open Settings".into(), native::CAMERA_SETTINGS_URL)),
                );
            }
        }
    }

    /// The pop-out bubble: on, with the camera running (the avatar keeps the plain circle).
    pub fn popout_active(&self) -> bool {
        self.settings.bubble_popout && self.settings.camera_enabled && !self.camera_failed
    }

    /// Closes the bubble so `sync_windows` reopens it in its current shape. Mid-recording it
    /// stays as it is: a window recording keeps the bubble by its window id.
    fn reopen_bubble(&mut self, cx: &mut Context<Self>) {
        if self.phase.is_active() {
            return;
        }
        if let Some(handle) = self.bubble.take() {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
    }

    pub fn resize_bubble(&mut self, cx: &mut Context<Self>) {
        // A size change reopens the pop-out bubble (see `update_settings`).
        if self.settings.bubble_popout {
            return;
        }
        let Some(handle) = self.bubble else { return };
        let diameter = f64::from(self.settings.bubble_size.diameter());
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                if let Some(frame) = native::frame(window) {
                    // Keep the bottom-left corner anchored.
                    let rect = TopLeftRect {
                        x: frame.x,
                        y: frame.y + frame.h - diameter,
                        w: diameter,
                        h: diameter,
                    };
                    native::set_frame(window, cx, rect);
                    native::setup_window(
                        window,
                        cx,
                        WindowSetup {
                            shadow: true,
                            corner_radius: Some(diameter / 2.),
                            ..Default::default()
                        },
                    );
                }
            });
        });
    }

    fn place_bubble_on_window(&mut self, window_id: u32, cx: &mut Context<Self>) {
        let Some(target) = self.windows.iter().find(|w| w.id == window_id) else {
            return;
        };
        let d = f64::from(self.settings.bubble_size.diameter());
        let inset = 20.;
        let rect = TopLeftRect {
            x: target.frame.x + inset,
            y: target.frame.y + target.frame.h - d - inset,
            w: d,
            h: d,
        };
        // Opened later (e.g. started from a shortcut) → `bubble::open` uses the anchor.
        self.bubble_anchor = Some(rect);
        if let Some(handle) = self.bubble {
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| native::set_frame(window, cx, rect));
            });
        }
    }

    /// Quitting mid-recording saves the file first instead of leaving a broken MP4.
    fn quit(&mut self, cx: &mut Context<Self>) {
        match self.phase {
            Phase::Recording { .. } => {
                self.quit_when_stopped = true;
                self.stop_recording(cx);
            }
            Phase::Stopping { .. } => self.quit_when_stopped = true,
            _ => cx.quit(),
        }
    }
}

/// Shared options for our borderless overlay windows; the native frame is applied afterwards.
pub fn overlay_options(rect: TopLeftRect, focus: bool) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(rect.x as f32), px(rect.y as f32)),
            size: size(px(rect.w as f32), px(rect.h as f32)),
        })),
        titlebar: None,
        focus,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    }
}

pub fn overlay_setup(level: OverlayLevel, rect: TopLeftRect) -> WindowSetup {
    WindowSetup {
        overlay: Some(level),
        frame: Some(rect),
        show: true,
        ..Default::default()
    }
}

/// Re-render `window`'s root view whenever the shared state changes.
pub fn observe_state<V: 'static>(
    state: &Entity<Annuncia>,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    cx.observe(state, |_, _, cx| cx.notify()).detach();
    cx.observe_window_appearance(window, |_, _, cx| cx.notify())
        .detach();
}

/// Activates the app, then runs `f` once macOS reports it active (or after a short timeout,
/// with `false`). GPUI deadlocks if one of its normal windows becomes key while the app is
/// inactive: its key-status handler calls `resignKeyWindow` while holding the window's lock,
/// and the resulting notification re-enters the handler. A menu bar app is usually inactive.
fn when_active(cx: &mut Context<Annuncia>, f: impl FnOnce(bool, &mut App) + 'static) {
    cx.activate(true);
    cx.spawn(async move |_, cx| {
        for _ in 0..50 {
            if native::app_is_active() {
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(10))
                .await;
        }
        let _ = cx.update(|cx| f(native::app_is_active(), cx));
    })
    .detach();
}
