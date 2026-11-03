//! The local speech models Annuncia can use, and downloading them. Each file is pinned to a
//! Hugging Face revision, size and SHA-256, downloaded with `curl` so
//! an interrupted download resumes, and verified before it's moved into place.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Which model transcribes recordings, if any.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transcription {
    #[default]
    Off,
    /// Parakeet Unified English: fast, with per-word timings.
    English,
    /// Whisper large-v3-turbo: about 100 languages, timed by phrase rather than word.
    Multilingual,
}

pub struct Artifact {
    pub filename: &'static str,
    revision: &'static str,
    repository: &'static str,
    pub bytes: u64,
    sha256: &'static str,
}

impl Artifact {
    fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repository, self.revision, self.filename
        )
    }
}

const PARAKEET_UNIFIED_ENGLISH: Artifact = Artifact {
    filename: "parakeet-unified-en-0.6b-Q8_0.gguf",
    revision: "7e948f21b7bdbac698d3318db9d350f1096f3b6c",
    repository: "handy-computer/parakeet-unified-en-0.6b-gguf",
    bytes: 731_357_568,
    sha256: "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795",
};

const WHISPER_LARGE_V3_TURBO: Artifact = Artifact {
    filename: "whisper-large-v3-turbo-Q8_0.gguf",
    revision: "d222c9f621c1128299248f2ded4d8a1820519780",
    repository: "handy-computer/whisper-large-v3-turbo-gguf",
    bytes: 886_381_824,
    sha256: "d5e65f2b0828802ae2c231673d31982cebe3a778c95d9494a9f3efee6bd17448",
};

impl Transcription {
    pub fn artifact(self) -> Option<&'static Artifact> {
        match self {
            Transcription::Off => None,
            Transcription::English => Some(&PARAKEET_UNIFIED_ENGLISH),
            Transcription::Multilingual => Some(&WHISPER_LARGE_V3_TURBO),
        }
    }

    pub fn model_name(self) -> &'static str {
        match self {
            Transcription::Off => "",
            Transcription::English => "Parakeet",
            Transcription::Multilingual => "Whisper",
        }
    }

    pub fn path(self) -> Option<PathBuf> {
        self.artifact().map(|a| models_dir().join(a.filename))
    }

    /// Downloaded and complete. Downloads are verified before they're moved into place, so
    /// the size check is enough here.
    pub fn is_installed(self) -> bool {
        match (self.artifact(), self.path()) {
            (Some(artifact), Some(path)) => {
                std::fs::metadata(path).is_ok_and(|m| m.len() == artifact.bytes)
            }
            _ => false,
        }
    }

    pub fn remove(self) {
        if let Some(path) = self.path() {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(partial(&path));
        }
    }
}

/// `~/Library/Application Support/Annuncia/models`.
pub fn models_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Annuncia")
        .join("models")
}

fn partial(path: &std::path::Path) -> PathBuf {
    path.with_extension("gguf.partial")
}

/// A download in progress, shared with the UI.
#[derive(Clone)]
pub struct Download {
    pub model: Transcription,
    pub received: Arc<AtomicU64>,
    pub total: u64,
    cancel: Arc<AtomicBool>,
}

impl Download {
    pub fn new(model: Transcription) -> Option<Self> {
        Some(Self {
            model,
            received: Arc::new(AtomicU64::new(0)),
            total: model.artifact()?.bytes,
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn fraction(&self) -> f32 {
        (self.received.load(Ordering::Relaxed) as f64 / self.total.max(1) as f64) as f32
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Blocking: downloads, verifies and installs the model. Resumes a partial download.
    pub fn run(&self) -> Result<()> {
        let artifact = self.model.artifact().context("no model to download")?;
        let path = self.model.path().context("no model path")?;
        let partial = partial(&path);
        std::fs::create_dir_all(models_dir()).context("creating the models folder")?;

        let mut curl = Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--retry",
                "3",
                "--connect-timeout",
                "15",
                "--speed-limit",
                "1024",
                "--speed-time",
                "60",
                "--continue-at",
                "-",
                "--output",
            ])
            .arg(&partial)
            .arg(artifact.url())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("couldn't start curl")?;
        let status = loop {
            if self.cancel.load(Ordering::Relaxed) {
                let _ = curl.kill();
                let _ = curl.wait();
                bail!("Download cancelled");
            }
            let size = std::fs::metadata(&partial).map_or(0, |m| m.len());
            self.received.store(size, Ordering::Relaxed);
            if let Some(status) = curl.try_wait()? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if !status.success() {
            let mut message = String::new();
            if let Some(mut stderr) = curl.stderr.take() {
                let _ = stderr.read_to_string(&mut message);
            }
            bail!("Download failed: {}", message.trim());
        }

        let size = std::fs::metadata(&partial).map_or(0, |m| m.len());
        if size != artifact.bytes {
            let _ = std::fs::remove_file(&partial);
            bail!("Download was incomplete; try again");
        }
        let mut file = std::fs::File::open(&partial)?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher)?;
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if digest != artifact.sha256 {
            let _ = std::fs::remove_file(&partial);
            bail!("The downloaded model didn't match its checksum; try again");
        }
        std::fs::rename(&partial, &path).context("installing the model")?;
        Ok(())
    }
}
