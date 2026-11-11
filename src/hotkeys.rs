use std::str::FromStr;

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui::Keystroke;

use crate::settings::HotkeySettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    ToggleRecording,
    PauseRecording,
    RestartRecording,
    OpenLauncher,
    ToggleCamera,
    CancelRecording,
}

impl HotkeyAction {
    pub const ALL: [HotkeyAction; 6] = [
        HotkeyAction::ToggleRecording,
        HotkeyAction::PauseRecording,
        HotkeyAction::RestartRecording,
        HotkeyAction::OpenLauncher,
        HotkeyAction::ToggleCamera,
        HotkeyAction::CancelRecording,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HotkeyAction::ToggleRecording => "Start / stop recording",
            HotkeyAction::PauseRecording => "Pause / resume recording",
            HotkeyAction::RestartRecording => "Restart recording",
            HotkeyAction::OpenLauncher => "Open recorder panel",
            HotkeyAction::ToggleCamera => "Toggle camera bubble",
            HotkeyAction::CancelRecording => "Cancel recording",
        }
    }

    pub fn binding(self, settings: &HotkeySettings) -> &str {
        match self {
            HotkeyAction::ToggleRecording => &settings.toggle_recording,
            HotkeyAction::PauseRecording => &settings.pause_recording,
            HotkeyAction::RestartRecording => &settings.restart_recording,
            HotkeyAction::OpenLauncher => &settings.open_launcher,
            HotkeyAction::ToggleCamera => &settings.toggle_camera,
            HotkeyAction::CancelRecording => &settings.cancel_recording,
        }
    }

    pub fn binding_mut(self, settings: &mut HotkeySettings) -> &mut String {
        match self {
            HotkeyAction::ToggleRecording => &mut settings.toggle_recording,
            HotkeyAction::PauseRecording => &mut settings.pause_recording,
            HotkeyAction::RestartRecording => &mut settings.restart_recording,
            HotkeyAction::OpenLauncher => &mut settings.open_launcher,
            HotkeyAction::ToggleCamera => &mut settings.toggle_camera,
            HotkeyAction::CancelRecording => &mut settings.cancel_recording,
        }
    }
}

pub struct Hotkeys {
    manager: GlobalHotKeyManager,
    registered: Vec<(HotKey, HotkeyAction)>,
}

impl Hotkeys {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            manager: GlobalHotKeyManager::new()?,
            registered: Vec::new(),
        })
    }

    /// Re-registers every shortcut; returns the problems (bad strings, conflicts) with the action
    /// each belongs to, so Settings can show them on that row.
    pub fn apply(&mut self, settings: &HotkeySettings) -> Vec<(HotkeyAction, String)> {
        for (hotkey, _) in self.registered.drain(..) {
            let _ = self.manager.unregister(hotkey);
        }
        let mut problems = Vec::new();
        // Parsed first so two actions on the same keys are caught before the OS sees either.
        let mut parsed: Vec<(HotKey, HotkeyAction)> = Vec::new();
        for action in HotkeyAction::ALL {
            let binding = action.binding(settings);
            if binding.is_empty() {
                continue;
            }
            match HotKey::from_str(binding) {
                Ok(hotkey) => match parsed.iter().find(|(h, _)| h.id() == hotkey.id()) {
                    Some((_, first)) => {
                        problems.push((*first, format!("Same as {}", action.label())));
                        problems.push((action, format!("Same as {}", first.label())));
                    }
                    None => parsed.push((hotkey, action)),
                },
                Err(err) => problems.push((action, format!("{binding}: {err}"))),
            }
        }
        for (hotkey, action) in parsed {
            match self.manager.register(hotkey) {
                Ok(()) => self.registered.push((hotkey, action)),
                Err(err) => problems.push((action, err.to_string())),
            }
        }
        problems
    }

    /// Drains pending presses.
    pub fn poll(&self) -> Vec<HotkeyAction> {
        let mut actions = Vec::new();
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state() != HotKeyState::Pressed {
                continue;
            }
            if let Some((_, action)) = self.registered.iter().find(|(h, _)| h.id() == event.id()) {
                actions.push(*action);
            }
        }
        actions
    }
}

/// Converts a GPUI keystroke captured in the settings window to a `global-hotkey` string.
/// Requires at least one of ⌘ ⌥ ⌃ so a bare letter can't hijack typing.
pub fn keystroke_to_binding(keystroke: &Keystroke) -> Option<String> {
    let m = &keystroke.modifiers;
    if !(m.platform || m.alt || m.control) {
        return None;
    }
    let key = keystroke.key.as_str();
    let code = match key {
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphabetic()) => {
            format!("Key{}", k.to_ascii_uppercase())
        }
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_digit()) => format!("Digit{k}"),
        "space" => "Space".into(),
        "escape" => "Escape".into(),
        "enter" => "Enter".into(),
        "tab" => "Tab".into(),
        "backspace" => "Backspace".into(),
        "up" => "ArrowUp".into(),
        "down" => "ArrowDown".into(),
        "left" => "ArrowLeft".into(),
        "right" => "ArrowRight".into(),
        "-" => "Minus".into(),
        "=" => "Equal".into(),
        "," => "Comma".into(),
        "." => "Period".into(),
        "/" => "Slash".into(),
        ";" => "Semicolon".into(),
        "'" => "Quote".into(),
        "[" => "BracketLeft".into(),
        "]" => "BracketRight".into(),
        "\\" => "Backslash".into(),
        "`" => "Backquote".into(),
        k if k.starts_with('f') && k[1..].parse::<u8>().is_ok_and(|n| (1..=20).contains(&n)) => {
            k.to_ascii_uppercase()
        }
        _ => return None,
    };
    let mut parts = Vec::new();
    if m.control {
        parts.push("control");
    }
    if m.alt {
        parts.push("alt");
    }
    if m.shift {
        parts.push("shift");
    }
    if m.platform {
        parts.push("super");
    }
    let binding = format!("{}+{code}", parts.join("+"));
    HotKey::from_str(&binding).ok().map(|_| binding)
}

/// `alt+shift+KeyR` → `⌥⇧R`.
pub fn display_binding(binding: &str) -> String {
    if binding.is_empty() {
        return "None".into();
    }
    let mut mods = String::new();
    let mut key = String::new();
    for part in binding.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => mods.push('⌃'),
            "alt" | "option" => mods.push('⌥'),
            "shift" => mods.push('⇧'),
            "super" | "cmd" | "command" | "meta" => mods.push('⌘'),
            _ => {
                key = match part {
                    "Escape" => "Esc".into(),
                    "Space" => "Space".into(),
                    "Enter" => "↩".into(),
                    "ArrowUp" => "↑".into(),
                    "ArrowDown" => "↓".into(),
                    "ArrowLeft" => "←".into(),
                    "ArrowRight" => "→".into(),
                    p => p
                        .strip_prefix("Key")
                        .or_else(|| p.strip_prefix("Digit"))
                        .unwrap_or(p)
                        .to_string(),
                }
            }
        }
    }
    // Apple's canonical order is ⌃⌥⇧⌘.
    let order = ['⌃', '⌥', '⇧', '⌘'];
    let mut sorted: Vec<char> = mods.chars().collect();
    sorted.sort_by_key(|c| order.iter().position(|o| o == c));
    format!("{}{key}", sorted.into_iter().collect::<String>())
}
