//! Transcribes recordings with a local model through transcribe.cpp (Metal on Apple silicon).
//! One worker thread owns the loaded model, so jobs run one at a time; the model is loaded on
//! first use and released after a couple of idle minutes to give its memory back.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use transcribe_cpp::{Model, RunOptions, Session, TimestampKind};

use crate::media;
use crate::models::Transcription;
use crate::speech::{self, SAMPLE_RATE};
use crate::transcript::{Piece, Transcript};

const IDLE_UNLOAD: Duration = Duration::from_secs(120);
/// Chunks never run longer than this (or the model's own limit) and end at a pause.
const MAX_CHUNK_SECONDS: f64 = 30.;
const MIN_CHUNK_SECONDS: f64 = 10.;
/// Silences this long are where chunks may end.
const CHUNK_PAUSE_SECONDS: f64 = 0.3;
/// Silences this long are recorded in the transcript (paragraph breaks, later trimming).
const TRANSCRIPT_PAUSE_SECONDS: f64 = 0.6;
/// Parakeet needs a little audio to work with; shorter chunks are padded with silence.
const MIN_RUN_SECONDS: f64 = 1.5;

type Reply = smol::channel::Sender<Result<Transcript>>;

enum Message {
    Job {
        video: PathBuf,
        model: Transcription,
        reply: Reply,
    },
    /// Release the model and stop. ggml aborts if its Metal device is torn down at process
    /// exit while a model still holds GPU buffers, so this must run before the app quits.
    Shutdown(Sender<()>),
}

pub struct Transcriber {
    messages: Sender<Message>,
}

impl Transcriber {
    pub fn new() -> Self {
        let (messages, rx) = channel();
        std::thread::Builder::new()
            .name("transcriber".into())
            .spawn(move || worker(rx))
            .expect("transcriber thread");
        Self { messages }
    }

    /// Blocks until the worker has released the model (or briefly, if a job is running).
    pub fn shutdown(&self) {
        let (done, wait) = channel();
        if self.messages.send(Message::Shutdown(done)).is_ok() {
            let _ = wait.recv_timeout(Duration::from_secs(5));
        }
    }

    /// Queues `video` for transcription; the result arrives on the returned channel.
    pub fn transcribe(
        &self,
        video: PathBuf,
        model: Transcription,
    ) -> smol::channel::Receiver<Result<Transcript>> {
        let (reply, result) = smol::channel::bounded(1);
        if self
            .messages
            .send(Message::Job {
                video,
                model,
                reply: reply.clone(),
            })
            .is_err()
        {
            let _ = reply.try_send(Err(anyhow::anyhow!("The transcriber stopped")));
        }
        result
    }
}

struct Loaded {
    kind: Transcription,
    session: Session,
    max_seconds: f64,
    timestamps: TimestampKind,
}

fn worker(messages: Receiver<Message>) {
    let mut loaded: Option<Loaded> = None;
    loop {
        match messages.recv_timeout(IDLE_UNLOAD) {
            Ok(Message::Job {
                video,
                model,
                reply,
            }) => {
                let result = run(&video, model, &mut loaded);
                if let Ok(transcript) = &result {
                    crate::transcript::save_cached(&video, transcript);
                }
                let _ = reply.try_send(result);
            }
            Ok(Message::Shutdown(done)) => {
                drop(loaded.take());
                let _ = done.send(());
                return;
            }
            Err(RecvTimeoutError::Timeout) => loaded = None,
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn load(kind: Transcription) -> Result<Loaded> {
    let path = kind.path().context("no model selected")?;
    if !kind.is_installed() {
        bail!(
            "The {} speech model isn't downloaded yet. Download it in Settings.",
            kind.model_name()
        );
    }
    let model = Model::load(&path).context("couldn't load the speech model")?;
    let caps = model.capabilities()?;
    let max_seconds = if caps.max_audio_ms > 0 {
        (caps.max_audio_ms as f64 / 1000.).min(MAX_CHUNK_SECONDS)
    } else {
        MAX_CHUNK_SECONDS
    };
    // Word timings where the model has them (Parakeet), phrase timings otherwise (Whisper).
    let timestamps = match caps.max_timestamp_kind {
        TimestampKind::Word | TimestampKind::Token => TimestampKind::Word,
        other => other,
    };
    Ok(Loaded {
        kind,
        session: model.session()?,
        max_seconds,
        timestamps,
    })
}

fn run(video: &Path, model: Transcription, loaded: &mut Option<Loaded>) -> Result<Transcript> {
    if let Some(cached) = crate::transcript::load_cached(video) {
        return Ok(cached);
    }
    if loaded.as_ref().is_none_or(|l| l.kind != model) {
        *loaded = None;
        *loaded = Some(load(model)?);
    }
    let engine = loaded.as_mut().context("no model loaded")?;
    transcribe(video, engine)
}

fn transcribe(video: &Path, engine: &mut Loaded) -> Result<Transcript> {
    let pcm = media::audio(video, SAMPLE_RATE as f64)?;
    let duration = pcm.len() as f64 / SAMPLE_RATE as f64;
    let pauses = speech::silences(&pcm, CHUNK_PAUSE_SECONDS);
    let options = RunOptions {
        timestamps: engine.timestamps,
        ..Default::default()
    };

    let mut pieces = Vec::new();
    for chunk in chunks(duration, &pauses, engine.max_seconds) {
        // Skip chunks that are entirely silence.
        if pauses
            .iter()
            .any(|p| p.start <= chunk.start && p.end >= chunk.end)
        {
            continue;
        }
        let from = (chunk.start * SAMPLE_RATE as f64) as usize;
        let to = ((chunk.end * SAMPLE_RATE as f64) as usize).min(pcm.len());
        let mut audio = pcm[from..to].to_vec();
        let min = (MIN_RUN_SECONDS * SAMPLE_RATE as f64) as usize;
        if audio.len() < min {
            audio.resize(min, 0.);
        }
        let result = engine
            .session
            .run(&audio, &options)
            .context("transcription failed")?;
        let at = |ms: i64| (chunk.start + ms as f64 / 1000.).min(chunk.end);
        if result.words.is_empty() {
            pieces.extend(result.segments.iter().map(|s| Piece {
                start: at(s.t0_ms),
                end: at(s.t1_ms),
                text: s.text.trim().to_string(),
            }));
        } else {
            pieces.extend(result.words.iter().map(|w| Piece {
                start: at(w.t0_ms),
                end: at(w.t1_ms),
                text: w.text.trim().to_string(),
            }));
        }
    }
    pieces.retain(|p| !p.text.is_empty());

    Ok(Transcript {
        pieces,
        pauses: pauses
            .into_iter()
            .filter(|p| p.end - p.start >= TRANSCRIPT_PAUSE_SECONDS)
            .collect(),
        duration,
    })
}

/// Splits `0..duration` into chunks no longer than `max`, each ending in the middle of the last
/// pause that fits (so no word is cut in half), or at `max` when there's no pause to use.
fn chunks(duration: f64, pauses: &[std::ops::Range<f64>], max: f64) -> Vec<std::ops::Range<f64>> {
    let mut out = Vec::new();
    let mut start = 0.;
    while duration - start > max {
        let limit = start + max;
        let cut = pauses
            .iter()
            .map(|p| (p.start + p.end) / 2.)
            .rfind(|mid| *mid > start + MIN_CHUNK_SECONDS.min(max / 2.) && *mid < limit)
            .unwrap_or(limit);
        out.push(start..cut);
        start = cut;
    }
    if duration > start {
        out.push(start..duration);
    }
    out
}
