use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    Screen,
    Window,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    H264,
    Hevc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    /// Tagged releases.
    Stable,
    /// Built from main every night it changes.
    Nightly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BubbleSize {
    Small,
    Medium,
    Large,
}

impl BubbleSize {
    pub fn diameter(self) -> f32 {
        match self {
            BubbleSize::Small => 160.,
            BubbleSize::Medium => 220.,
            BubbleSize::Large => 300.,
        }
    }
}

/// Where the camera bubble was last dropped: how far across the room the screen leaves it
/// (0 at the left or top edge, 1 at the right or bottom), so it lands in the same place on any
/// display and at any size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BubblePosition {
    pub x: f64,
    pub y: f64,
}

/// Global shortcuts, stored in the `global-hotkey` string format (e.g. `alt+shift+KeyR`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeySettings {
    pub toggle_recording: String,
    pub pause_recording: String,
    pub restart_recording: String,
    pub open_launcher: String,
    pub toggle_camera: String,
    pub cancel_recording: String,
}

impl Default for HotkeySettings {
    fn default() -> Self {
        Self {
            toggle_recording: "alt+shift+KeyR".into(),
            pause_recording: "alt+shift+KeyP".into(),
            // Restart discards the recording, so it's left unbound unless you choose one.
            restart_recording: String::new(),
            open_launcher: "alt+shift+KeyL".into(),
            toggle_camera: "alt+shift+KeyC".into(),
            cancel_recording: "alt+shift+Escape".into(),
        }
    }
}

impl HotkeySettings {
    pub fn none() -> Self {
        Self {
            toggle_recording: String::new(),
            pause_recording: String::new(),
            restart_recording: String::new(),
            open_launcher: String::new(),
            toggle_camera: String::new(),
            cancel_recording: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub save_dir: PathBuf,
    pub capture_mode: CaptureMode,
    pub camera_enabled: bool,
    pub camera_device: Option<String>,
    pub mic_enabled: bool,
    pub mic_device: Option<String>,
    pub system_audio: bool,
    pub show_cursor: bool,
    pub fps: u32,
    pub codec: Codec,
    pub countdown_secs: u32,
    pub bubble_size: BubbleSize,
    /// Unset until the bubble is first moved; it then reopens there.
    pub bubble_position: Option<BubblePosition>,
    /// Shown in the bubble when the camera is off.
    pub avatar_image: Option<PathBuf>,
    /// With the camera off, show the avatar (that photo, or smoke that moves with your voice)
    /// rather than no bubble at all.
    pub avatar_enabled: bool,
    /// Let your head rise out of the top of the camera bubble.
    pub bubble_popout: bool,
    /// Open the trim-and-slice editor when a recording is saved.
    pub open_editor: bool,
    /// Transcribe recordings with a local model once they're saved.
    pub transcription: crate::models::Transcription,
    /// What the editor trims on its own when it opens.
    pub cleanup: crate::cleanup::Cleanup,
    /// Which builds updates come from; unset means the channel this build came from.
    pub update_channel: Option<UpdateChannel>,
    pub hotkeys: HotkeySettings,
    /// First-run setup is done. Settings files from before it existed count as done, so only
    /// new installs (and anyone without Screen Recording access) see it unasked.
    #[serde(default = "yes")]
    pub onboarded: bool,
    /// Screen Recording was asked for once. macOS only shows that prompt the first time, so
    /// from then on setup points at System Settings instead.
    pub asked_for_screen: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            save_dir: default_save_dir(),
            capture_mode: CaptureMode::Screen,
            camera_enabled: true,
            camera_device: None,
            mic_enabled: true,
            mic_device: None,
            system_audio: false,
            show_cursor: true,
            fps: 30,
            codec: Codec::H264,
            countdown_secs: 3,
            bubble_size: BubbleSize::Medium,
            bubble_position: None,
            avatar_image: None,
            avatar_enabled: true,
            bubble_popout: false,
            open_editor: true,
            transcription: crate::models::Transcription::Off,
            cleanup: crate::cleanup::Cleanup::Off,
            update_channel: None,
            hotkeys: HotkeySettings::default(),
            onboarded: false,
            asked_for_screen: false,
        }
    }
}

fn default_save_dir() -> PathBuf {
    dirs::document_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Documents"))
        .join("Annuncia")
}

/// `$XDG_CONFIG_HOME/annuncia`, falling back to `~/.config/annuncia`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("annuncia")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                eprintln!("annuncia: ignoring invalid {}: {err}", path.display());
                Self::default()
            }),
            Err(_) => {
                let settings = Self::default();
                if let Err(err) = settings.save() {
                    eprintln!("annuncia: could not write default settings: {err:#}");
                }
                settings
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path();
        write_atomic(&path, &serde_json::to_vec_pretty(self)?)
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// `~/Documents/Annuncia` style display path.
pub fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_before_setup_count_as_onboarded() {
        let old: Settings = serde_json::from_str(r#"{ "fps": 60 }"#).unwrap();
        assert!(old.onboarded);
        assert_eq!(old.fps, 60);
        assert!(!Settings::default().onboarded);
    }
}
