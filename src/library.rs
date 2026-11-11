//! The recordings Annuncia has saved, kept in `library.json` next to the settings file, plus
//! the thumbnails the library window shows. A recording moved or deleted outside Annuncia drops
//! out the next time the library is opened.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    /// When it was recorded, in Unix seconds. Kept here because editing replaces the file.
    pub created: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Library {
    /// Newest first.
    pub recordings: Vec<Entry>,
}

fn library_path() -> PathBuf {
    settings::config_dir().join("library.json")
}

impl Library {
    pub fn load() -> Self {
        let path = library_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let mut library: Self = serde_json::from_str(&text).unwrap_or_else(|err| {
            eprintln!("annuncia: ignoring invalid {}: {err}", path.display());
            Self::default()
        });
        library
            .recordings
            .sort_by_key(|e| std::cmp::Reverse(e.created));
        library
    }

    fn save(&self) {
        let result = serde_json::to_vec_pretty(self)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| settings::write_atomic(&library_path(), &bytes));
        if let Err(err) = result {
            eprintln!("annuncia: could not save the library: {err:#}");
        }
    }

    /// Records a newly saved recording.
    pub fn add(&mut self, path: PathBuf) {
        self.recordings.retain(|e| e.path != path);
        self.recordings.insert(
            0,
            Entry {
                path,
                created: chrono::Local::now().timestamp(),
            },
        );
        self.save();
    }

    pub fn rename(&mut self, from: &Path, to: PathBuf) {
        if let Some(entry) = self.recordings.iter_mut().find(|e| e.path == from) {
            entry.path = to;
            self.save();
        }
    }

    /// Puts back a recording that was removed, in its place by date.
    pub fn restore(&mut self, entry: Entry) {
        self.recordings.retain(|e| e.path != entry.path);
        let at = self
            .recordings
            .iter()
            .position(|e| e.created < entry.created)
            .unwrap_or(self.recordings.len());
        self.recordings.insert(at, entry);
        self.save();
    }

    pub fn remove(&mut self, path: &Path) {
        self.recordings.retain(|e| e.path != path);
        self.save();
    }

    /// Drops recordings whose files are gone.
    pub fn prune(&mut self) {
        let before = self.recordings.len();
        self.recordings.retain(|e| e.path.exists());
        if self.recordings.len() != before {
            self.save();
        }
    }
}

/// Where `video`'s thumbnail is cached (whether or not it has been made yet).
pub fn thumbnail_path(video: &Path) -> Option<PathBuf> {
    let key = crate::transcript::file_key(video)?;
    Some(
        dirs::cache_dir()?
            .join("Annuncia")
            .join("thumbnails")
            .join(format!("{key}.jpg")),
    )
}

/// A cached thumbnail for `video`, made on first use. Blocks, so call it off the main thread.
pub fn thumbnail(video: &Path) -> Option<PathBuf> {
    let path = thumbnail_path(video)?;
    if path.exists() {
        return Some(path);
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    crate::media::thumbnail(video, &path, 640.)
        .inspect_err(|err| eprintln!("annuncia: thumbnail for {}: {err:#}", video.display()))
        .ok()?;
    Some(path)
}
