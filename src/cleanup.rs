//! Automatic cuts: dead air at both ends, long pauses shortened to a natural beat, and filler
//! words. Pauses come from the audio itself (word timings stretch across them); fillers need a
//! word-timed transcript. Each cut is applied the way you'd make it by hand: a split in its middle
//! with both clips trimmed back to its edges, so a handle drag restores any of it.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::edit::Edit;
use crate::transcript::Piece;

/// How much the editor cleans up on its own when it opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cleanup {
    #[default]
    Off,
    Pauses,
    PausesAndFillers,
}

/// Pauses at least this long are shortened...
const LONG_PAUSE: f64 = 1.0;
/// ...to this much silence, split evenly around the cut.
const KEPT_PAUSE: f64 = 0.35;
/// Silence kept before the first word and after the last one.
const LEAD_IN: f64 = 0.25;
const TAIL: f64 = 0.4;
/// Leading or trailing silence shorter than this isn't worth trimming.
const MIN_DEAD_AIR: f64 = 0.5;
/// A filler is never cut longer than this (its timing may run into the next pause).
const MAX_FILLER: f64 = 0.8;
/// Cuts shorter than this aren't worth a clip.
const MIN_CUT: f64 = 0.15;

const FILLERS: &[&str] = &[
    "um", "umm", "uh", "uhh", "uhm", "erm", "er", "ah", "hmm", "hm", "mm", "mhm",
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// The kept part of the recording.
    pub start: f64,
    pub end: f64,
    /// Ranges to remove inside it, sorted and not overlapping.
    pub cuts: Vec<Range<f64>>,
}

pub fn is_filler(text: &str) -> bool {
    let word: String = text
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect::<String>()
        .to_lowercase();
    FILLERS.contains(&word.as_str())
}

/// Plans the cuts for a recording `duration` seconds long with the given silences (from
/// `speech::silences`) and, for filler words, its transcript's pieces.
pub fn plan(duration: f64, silences: &[Range<f64>], words: Option<&[Piece]>) -> Plan {
    let mut start = 0.;
    let mut end = duration;
    // Dead air: silence that runs from the very start, or to the very end.
    if let Some(first) = silences.first()
        && first.start <= 0.05
        && first.end - first.start >= MIN_DEAD_AIR
    {
        start = (first.end - LEAD_IN).max(0.);
    }
    if let Some(last) = silences.last()
        && last.end >= duration - 0.05
        && last.end - last.start >= MIN_DEAD_AIR
        && last.start > start
    {
        end = (last.start + TAIL).min(duration);
    }

    let mut cuts: Vec<Range<f64>> = Vec::new();
    for pause in silences {
        if pause.end - pause.start < LONG_PAUSE || pause.start <= start || pause.end >= end {
            continue;
        }
        let keep = KEPT_PAUSE / 2.;
        cuts.push(pause.start + keep..pause.end - keep);
    }

    if let Some(words) = words {
        for (i, word) in words.iter().enumerate() {
            if !is_filler(&word.text) || word.start < start || word.start >= end {
                continue;
            }
            let next = words.get(i + 1).map_or(end, |w| w.start);
            let cut_end = word.end.min(next).min(word.start + MAX_FILLER);
            cuts.push(word.start..cut_end);
        }
    }

    cuts.retain(|c| c.end - c.start >= MIN_CUT);
    cuts.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged: Vec<Range<f64>> = Vec::new();
    for cut in cuts {
        match merged.last_mut() {
            Some(last) if cut.start <= last.end + 0.1 => last.end = last.end.max(cut.end),
            _ => merged.push(cut),
        }
    }
    Plan {
        start,
        end,
        cuts: merged,
    }
}

/// Adds the plan to `edit`: dead air at both ends and each cut, made with `Edit::cut` and only
/// where something is still kept, so cuts and trims made by hand stay as they are. Returns how
/// many of the plan's spots changed the edit.
pub fn apply(plan: &Plan, edit: &mut Edit) -> usize {
    let ends = [0.0..plan.start, plan.end..edit.duration];
    let mut spots = 0;
    for cut in ends.into_iter().chain(plan.cuts.iter().cloned()) {
        if edit.cut(cut) {
            spots += 1;
        }
    }
    spots
}
