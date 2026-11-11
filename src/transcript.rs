//! A recording's transcript: timed words (or phrases, for models without word timings) and the
//! pauses between them, with a Markdown rendering for the clipboard. Transcripts are cached
//! under Application Support, keyed by the recording's path, size and modification time.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A paragraph ends at a pause at least this long, or after a sentence once it's this long.
const PARAGRAPH_PAUSE: f64 = 1.5;
const PARAGRAPH_WORDS: usize = 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Piece {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Transcript {
    /// Words, or phrases when the model only times phrases.
    pub pieces: Vec<Piece>,
    /// Stretches without speech, from the audio itself (word timings absorb pauses).
    pub pauses: Vec<Range<f64>>,
    pub duration: f64,
}

impl Transcript {
    pub fn is_empty(&self) -> bool {
        self.pieces.iter().all(|p| p.text.trim().is_empty())
    }

    /// The pieces of each paragraph, by index into `pieces` (empty pieces left out).
    pub fn paragraph_pieces(&self) -> Vec<Vec<usize>> {
        let mut paragraphs: Vec<Vec<usize>> = Vec::new();
        let mut words = 0;
        // Word timings stretch across the pause after a word, so a pause counts as a break
        // when it starts after the previous word started and ends by this one.
        let mut previous: Option<&Piece> = None;
        for (i, piece) in self.pieces.iter().enumerate() {
            let text = piece.text.trim();
            if text.is_empty() {
                continue;
            }
            let paused = previous.is_some_and(|prev| {
                self.pauses.iter().any(|p| {
                    p.end - p.start >= PARAGRAPH_PAUSE
                        && p.start > prev.start
                        && p.end <= piece.start + 0.5
                })
            });
            let ends_sentence =
                previous.is_some_and(|prev| prev.text.trim_end().ends_with(['.', '?', '!']));
            let breaks = paused || (words >= PARAGRAPH_WORDS && ends_sentence);
            match paragraphs.last_mut() {
                Some(paragraph) if !breaks => paragraph.push(i),
                _ => {
                    paragraphs.push(vec![i]);
                    words = 0;
                }
            }
            words += text.split_whitespace().count();
            previous = Some(piece);
        }
        paragraphs
    }

    /// Paragraphs of text with their start times.
    pub fn paragraphs(&self) -> Vec<(f64, String)> {
        self.paragraph_pieces()
            .into_iter()
            .map(|indices| {
                let start = self.pieces[indices[0]].start;
                let text = indices
                    .iter()
                    .map(|&i| self.pieces[i].text.trim())
                    .collect::<Vec<_>>()
                    .join(" ");
                (start, text)
            })
            .collect()
    }

    /// The piece being spoken at `t`: the last one that has started.
    pub fn piece_at(&self, t: f64) -> Option<usize> {
        let i = self.pieces.partition_point(|p| p.start <= t);
        i.checked_sub(1)
    }

    /// Where `query` appears, as ranges of piece indices, ignoring case and spacing. A match
    /// can run across several words.
    pub fn search(&self, query: &str) -> Vec<Range<usize>> {
        let query = query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        // The words joined by single spaces, and which piece each byte of that text came from.
        let mut text = String::new();
        let mut owners = Vec::new();
        for (i, piece) in self.pieces.iter().enumerate() {
            let words = piece.text.split_whitespace().collect::<Vec<_>>().join(" ");
            if words.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push(' ');
                owners.push(i);
            }
            let words = words.to_lowercase();
            owners.extend(std::iter::repeat_n(i, words.len()));
            text.push_str(&words);
        }
        text.match_indices(&query)
            .map(|(at, found)| owners[at]..owners[at + found.len() - 1] + 1)
            .collect()
    }

    /// The transcript as Markdown: a title, then timestamped paragraphs.
    pub fn markdown(&self, title: &str) -> String {
        let mut out = format!("# {title}\n");
        for (start, text) in self.paragraphs() {
            out.push_str(&format!("\n**{}** {text}\n", timestamp(start)));
        }
        out
    }
}

/// `m:ss`, or `h:mm:ss` past an hour.
pub fn timestamp(seconds: f64) -> String {
    let s = seconds.max(0.) as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Identifies one version of a file: its path, size and modification time. Editing (or
/// replacing) the file changes the key, so nothing cached for the old version is reused.
pub fn file_key(video: &Path) -> Option<String> {
    let meta = std::fs::metadata(video).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let mut hasher = Sha256::new();
    hasher.update(video.to_string_lossy().as_bytes());
    hasher.update(meta.len().to_le_bytes());
    hasher.update(modified.to_le_bytes());
    Some(
        hasher.finalize()[..12]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// Where a recording's transcript is cached.
fn cache_path(video: &Path) -> Option<PathBuf> {
    Some(
        dirs::data_dir()?
            .join("Annuncia")
            .join("transcripts")
            .join(format!("{}.json", file_key(video)?)),
    )
}

pub fn load_cached(video: &Path) -> Option<Transcript> {
    let text = std::fs::read_to_string(cache_path(video)?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Drops `video`'s cached transcript, so the next transcription runs the model again.
pub fn forget_cached(video: &Path) {
    if let Some(path) = cache_path(video) {
        let _ = std::fs::remove_file(path);
    }
}

pub fn save_cached(video: &Path, transcript: &Transcript) {
    let Some(path) = cache_path(video) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(transcript) {
        let _ = std::fs::write(path, json);
    }
}
