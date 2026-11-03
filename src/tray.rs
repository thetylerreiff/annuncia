use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::native::{self, TopLeftRect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    /// Left click on the icon.
    Clicked,
    ToggleRecording,
    TogglePause,
    Restart,
    OpenLauncher,
    OpenLibrary,
    OpenFolder,
    OpenSettings,
    Quit,
}

/// What the menu offers depends on whether a recording is running or paused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayState {
    Idle,
    Recording,
    Paused,
}

pub struct Tray {
    icon: TrayIcon,
    toggle_item: MenuItem,
    pause_item: MenuItem,
    restart_item: MenuItem,
    state: TrayState,
}

const TOGGLE: &str = "toggle";
const PAUSE: &str = "pause";
const RESTART: &str = "restart";
const LAUNCHER: &str = "launcher";
const LIBRARY: &str = "library";
const FOLDER: &str = "folder";
const SETTINGS: &str = "settings";
const QUIT: &str = "quit";

impl Tray {
    pub fn new() -> anyhow::Result<Self> {
        let toggle_item = MenuItem::with_id(TOGGLE, "Start Recording", true, None);
        let pause_item = MenuItem::with_id(PAUSE, "Pause Recording", false, None);
        let restart_item = MenuItem::with_id(RESTART, "Restart Recording", false, None);
        let menu = Menu::new();
        menu.append_items(&[
            &toggle_item,
            &pause_item,
            &restart_item,
            &MenuItem::with_id(LAUNCHER, "Open Recorder…", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(LIBRARY, "Library…", true, None),
            &MenuItem::with_id(FOLDER, "Open Recordings Folder", true, None),
            &MenuItem::with_id(SETTINGS, "Settings…", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(QUIT, "Quit Annuncia", true, None),
        ])?;
        let icon = TrayIconBuilder::new()
            .with_icon_templated(idle_icon())
            .with_tooltip("Annuncia")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;
        Ok(Self {
            icon,
            toggle_item,
            pause_item,
            restart_item,
            state: TrayState::Idle,
        })
    }

    pub fn poll(&self) -> Vec<TrayAction> {
        let mut actions = Vec::new();
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                actions.push(TrayAction::Clicked);
            }
        }
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            actions.push(match event.id().as_ref() {
                TOGGLE => TrayAction::ToggleRecording,
                PAUSE => TrayAction::TogglePause,
                RESTART => TrayAction::Restart,
                LAUNCHER => TrayAction::OpenLauncher,
                LIBRARY => TrayAction::OpenLibrary,
                FOLDER => TrayAction::OpenFolder,
                SETTINGS => TrayAction::OpenSettings,
                QUIT => TrayAction::Quit,
                _ => continue,
            });
        }
        actions
    }

    pub fn set_state(&mut self, state: TrayState) {
        if self.state == state {
            return;
        }
        self.state = state;
        // The icon itself stays put: the notch HUD shows that a recording is running.
        self.toggle_item.set_text(if state == TrayState::Idle {
            "Start Recording"
        } else {
            "Stop Recording"
        });
        self.pause_item.set_enabled(state != TrayState::Idle);
        self.restart_item.set_enabled(state != TrayState::Idle);
        self.pause_item.set_text(if state == TrayState::Paused {
            "Resume Recording"
        } else {
            "Pause Recording"
        });
    }

    pub fn frame(&self) -> Option<TopLeftRect> {
        self.icon
            .ns_status_item()
            .and_then(|item| native::status_item_frame(&item))
    }
}

const ICON_PX: u32 = 36;

/// Ring with a centre dot, drawn as an alpha mask so macOS can tint it (template image).
fn idle_icon() -> Icon {
    draw_icon(|d| {
        let ring = (d - 12.5).abs() < 1.6;
        let dot = d < 6.5;
        if ring || dot { Some([0, 0, 0]) } else { None }
    })
}

fn draw_icon(shade: impl Fn(f32) -> Option<[u8; 3]>) -> Icon {
    let mut rgba = Vec::with_capacity((ICON_PX * ICON_PX * 4) as usize);
    let c = ICON_PX as f32 / 2.;
    for y in 0..ICON_PX {
        for x in 0..ICON_PX {
            // 4x supersampling for smooth edges.
            let mut hits = 0u32;
            let mut color = [0u8; 3];
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f32 + (sx as f32 + 0.5) / 4.;
                    let py = y as f32 + (sy as f32 + 0.5) / 4.;
                    if let Some(rgb) = shade(((px - c).powi(2) + (py - c).powi(2)).sqrt()) {
                        hits += 1;
                        color = rgb;
                    }
                }
            }
            rgba.extend_from_slice(&color);
            rgba.push((hits * 255 / 16) as u8);
        }
    }
    Icon::from_rgba(rgba, ICON_PX, ICON_PX).expect("icon dimensions are valid")
}
