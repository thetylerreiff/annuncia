//! Finds the pauses in a recording's 16 kHz mono audio: where the transcript breaks into
//! paragraphs, where long audio is cut into chunks for the model, and later where silences can
//! be trimmed.

use std::ops::Range;

pub const SAMPLE_RATE: usize = 16_000;
/// 20 ms frames.
const FRAME_SAMPLES: usize = SAMPLE_RATE / 50;
const SPEECH_MARGIN_DB: f32 = 12.0;
const MINIMUM_SPEECH_DB: f32 = -55.0;
const LOWEST_FLOOR_DB: f32 = -90.0;
/// About 3 dB per second at 50 frames per second.
const FLOOR_RISE_DB_PER_FRAME: f32 = 0.06;

/// Classifies consecutive 20 ms frames as speech or not. Speech is any frame at least 12 dB
/// above an adaptive noise floor and never quieter than -55 dBFS. The floor starts at the first
/// frame, falls immediately, and rises slowly, so speech between word gaps can't drag it up to
/// the speech level.
#[derive(Default)]
struct SpeechFrames {
    floor_db: Option<f32>,
}

impl SpeechFrames {
    fn is_speech(&mut self, frame: &[f32]) -> bool {
        let energy = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
        let level_db = if energy.is_finite() {
            10.0 * energy.max(1e-18).log10()
        } else {
            0.0
        };
        let floor_db = match self.floor_db {
            Some(floor_db) if level_db >= floor_db => floor_db + FLOOR_RISE_DB_PER_FRAME,
            _ => level_db.max(LOWEST_FLOOR_DB),
        };
        self.floor_db = Some(floor_db);
        level_db > (floor_db + SPEECH_MARGIN_DB).max(MINIMUM_SPEECH_DB)
    }
}

/// Stretches without speech at least `min_seconds` long, in seconds.
pub fn silences(pcm: &[f32], min_seconds: f64) -> Vec<Range<f64>> {
    let frame_secs = FRAME_SAMPLES as f64 / SAMPLE_RATE as f64;
    let mut frames = SpeechFrames::default();
    let mut out = Vec::new();
    let mut quiet_from: Option<usize> = None;
    let chunks = pcm.chunks_exact(FRAME_SAMPLES);
    let count = chunks.len();
    for (i, frame) in chunks.enumerate() {
        if frames.is_speech(frame) {
            if let Some(from) = quiet_from.take()
                && (i - from) as f64 * frame_secs >= min_seconds
            {
                out.push(from as f64 * frame_secs..i as f64 * frame_secs);
            }
        } else if quiet_from.is_none() {
            quiet_from = Some(i);
        }
    }
    if let Some(from) = quiet_from
        && (count - from) as f64 * frame_secs >= min_seconds
    {
        out.push(from as f64 * frame_secs..count as f64 * frame_secs);
    }
    out
}
