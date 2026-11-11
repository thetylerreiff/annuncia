//! ScreenCaptureKit capture written straight to disk with `SCRecordingOutput` (macOS 15+),
//! which muxes video, system audio and the microphone without us touching sample buffers.
//!
//! A recording is a series of parts: pausing detaches the recording output (finishing the
//! current part) while the stream keeps running, and resuming attaches a fresh one. On stop the
//! parts are joined into the final file.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use screencapturekit::prelude::*;
use screencapturekit::recording_output::{
    RecordingCallbacks, SCRecordingOutput, SCRecordingOutputCodec, SCRecordingOutputConfiguration,
    SCRecordingOutputFileType,
};

use crate::native::TopLeftRect;
use crate::settings::Codec;

#[derive(Clone, Debug)]
pub struct WindowChoice {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub frame: TopLeftRect,
    /// The owning app's process.
    pub pid: i32,
    /// The owning app's icon as a PNG, once it has been looked up.
    pub icon: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum Target {
    /// `None` = the display under the menu bar.
    Screen {
        display_id: Option<u32>,
    },
    Window {
        window_id: u32,
    },
}

#[derive(Clone, Debug)]
pub struct StartRequest {
    pub target: Target,
    pub path: PathBuf,
    /// Our own control windows, hidden from the recording.
    pub exclude_window_ids: Vec<u32>,
    /// The camera bubble, kept in window recordings even though it isn't the target.
    pub bubble_window_id: Option<u32>,
    pub fps: u32,
    pub codec: Codec,
    pub show_cursor: bool,
    pub system_audio: bool,
    /// `Some(None)` = default microphone.
    pub microphone: Option<Option<String>>,
}

/// Reported from ScreenCaptureKit's delegate, outside the session calls.
#[derive(Debug)]
pub enum RecorderEvent {
    Failed(String),
}

/// The part being written: its output must stay alive until ScreenCaptureKit finishes it.
struct Part {
    output: SCRecordingOutput,
    done: Receiver<()>,
}

impl Part {
    fn wait(self) {
        if self.done.recv_timeout(Duration::from_secs(10)).is_err() {
            eprintln!("annuncia: a recording part didn't report finishing");
        }
    }
}

pub struct Session {
    stream: SCStream,
    part: Option<Part>,
    /// Every part started so far, in order.
    parts: Vec<PathBuf>,
    parts_dir: PathBuf,
    codec: Codec,
    events: Sender<RecorderEvent>,
    /// Where the joined recording is saved.
    path: PathBuf,
}

struct DropFrames;

impl SCStreamOutputTrait for DropFrames {
    fn did_output_sample_buffer(&self, _: CMSampleBuffer, _: SCStreamOutputType) {}
}

/// Lists windows worth recording (titled, on screen, normal layer, not ours).
pub fn list_windows() -> Result<Vec<WindowChoice>> {
    let content = SCShareableContent::create()
        .with_on_screen_windows_only(true)
        .with_exclude_desktop_windows(true)
        .get()
        .map_err(permission_hint)?;
    let own_pid = std::process::id() as i32;
    let mut choices = Vec::new();
    for window in content.windows() {
        let title = window.title().unwrap_or_default();
        let frame = window.frame();
        let Some(app) = window.owning_application() else {
            continue;
        };
        if window.window_layer() != 0
            || title.is_empty()
            || app.process_id() == own_pid
            || frame.size.width < 80.
            || frame.size.height < 60.
        {
            continue;
        }
        choices.push(WindowChoice {
            id: window.window_id(),
            app: app.application_name(),
            title,
            frame: TopLeftRect {
                x: frame.origin.x,
                y: frame.origin.y,
                w: frame.size.width,
                h: frame.size.height,
            },
            pid: app.process_id(),
            icon: None,
        });
    }
    Ok(choices)
}

pub fn microphones() -> Vec<(String, String)> {
    AudioInputDevice::list()
        .into_iter()
        .map(|d| (d.id, d.name))
        .collect()
}

fn permission_hint(err: SCError) -> anyhow::Error {
    // Kept short to fit the notch drawer; the underlying error goes to the log.
    eprintln!("annuncia: shareable content: {err}");
    anyhow!("Annuncia needs Screen Recording access. Allow it in System Settings, then try again.")
}

/// Blocking: call off the main thread.
pub fn start(req: StartRequest, events: Sender<RecorderEvent>) -> Result<Session> {
    let content = SCShareableContent::get().map_err(permission_hint)?;
    let windows = content.windows();
    let find_window = |id: u32| windows.iter().find(|w| w.window_id() == id);
    let displays = content.displays();
    if displays.is_empty() {
        bail!("There's no display to record.");
    }

    let (filter, source_rect) = match &req.target {
        Target::Screen { display_id } => {
            let display = display_id
                .and_then(|id| displays.iter().find(|d| d.display_id() == id))
                .or_else(|| {
                    displays
                        .iter()
                        .find(|d| d.frame().origin.x == 0. && d.frame().origin.y == 0.)
                })
                .unwrap_or(&displays[0]);
            let excluded: Vec<&SCWindow> = req
                .exclude_window_ids
                .iter()
                .filter_map(|id| find_window(*id))
                .collect();
            let filter = SCContentFilter::create()
                .with_display(display)
                .with_excluding_windows(&excluded)
                .build()?;
            (filter, None)
        }
        Target::Window { window_id } => {
            let window =
                find_window(*window_id).context("That window has closed. Pick another one.")?;
            let frame = window.frame();
            let center = (
                frame.origin.x + frame.size.width / 2.,
                frame.origin.y + frame.size.height / 2.,
            );
            let display = displays
                .iter()
                .find(|d| {
                    let f = d.frame();
                    center.0 >= f.origin.x
                        && center.0 < f.origin.x + f.size.width
                        && center.1 >= f.origin.y
                        && center.1 < f.origin.y + f.size.height
                })
                .unwrap_or(&displays[0]);
            // Record the display but keep only the target window and the bubble, cropped to the
            // window: unlike a single-window filter, this lets the camera bubble appear on top.
            let mut included = vec![window];
            if let Some(bubble) = req.bubble_window_id.and_then(find_window) {
                included.push(bubble);
            }
            let filter = SCContentFilter::create()
                .with_display(display)
                .with_including_windows(&included)
                .build()?;
            let d = display.frame();
            let rect = CGRect {
                origin: CGPoint {
                    x: frame.origin.x - d.origin.x,
                    y: frame.origin.y - d.origin.y,
                },
                size: frame.size,
            };
            (filter, Some(rect))
        }
    };

    let scale = f64::from(filter.point_pixel_scale().max(1.));
    let content_rect = filter.content_rect();
    let size = source_rect.map_or(content_rect.size, |r| r.size);
    // VideoToolbox's H.264 encoder tops out at 4096 px; scale 5K/6K captures down to fit.
    let max_edge = match req.codec {
        Codec::H264 => 4096.,
        Codec::Hevc => 8192.,
    };
    let scale = scale.min(max_edge / size.width.max(size.height));
    // H.264 needs even dimensions.
    let even = |v: f64| ((v * scale).round() as u32) & !1;

    let mut config = SCStreamConfiguration::new()
        .with_width(even(size.width))
        .with_height(even(size.height))
        .with_scales_to_fit(true)
        .with_fps(req.fps)
        .with_shows_cursor(req.show_cursor)
        .with_captures_audio(req.system_audio)
        .with_excludes_current_process_audio(true)
        .with_captures_microphone(req.microphone.is_some())?;
    if let Some(Some(device)) = &req.microphone {
        config = config.with_microphone_capture_device_id(device)?;
    }
    if let Some(rect) = source_rect {
        config = config.with_source_rect(rect);
    }

    if let Some(dir) = req.path.parent() {
        std::fs::create_dir_all(dir).with_context(|| {
            format!(
                "Couldn't create the recordings folder at {}.",
                dir.display()
            )
        })?;
    }
    // Parts go to the cache, not the recordings folder, which is often synced to iCloud.
    let stem = req
        .path
        .file_stem()
        .map_or("recording".into(), |s| s.to_string_lossy().into_owned());
    let parts_dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Annuncia")
        .join("parts")
        .join(stem);
    std::fs::create_dir_all(&parts_dir).with_context(|| {
        format!(
            "Couldn't create a working folder at {}.",
            parts_dir.display()
        )
    })?;

    let mut stream = SCStream::new(&filter, &config)?;
    // Keeps frames flowing (and the stream alive) while no recording output is attached, and
    // stops ScreenCaptureKit logging a warning for every dropped frame.
    stream.add_output_handler(DropFrames, SCStreamOutputType::Screen)?;
    let mut session = Session {
        stream,
        part: None,
        parts: Vec::new(),
        parts_dir,
        codec: req.codec,
        events,
        path: req.path,
    };
    let part = session.next_part()?;
    session.stream.add_recording_output(&part.output)?;
    session.part = Some(part);
    session
        .stream
        .start_capture()
        .map_err(|e| anyhow!("The screen capture didn't start ({e})."))?;
    Ok(session)
}

/// Moves `from` to `to`, copying when they're on different volumes.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_err() {
        std::fs::copy(from, to)
            .with_context(|| format!("Couldn't save the recording to {}.", to.display()))?;
        let _ = std::fs::remove_file(from);
    }
    Ok(())
}

/// The calls below block: run them off the main thread.
impl Session {
    /// Creates the output for the next part; the caller attaches it to the stream.
    fn next_part(&mut self) -> Result<Part> {
        let path = self
            .parts_dir
            .join(format!("part-{}.mp4", self.parts.len() + 1));
        let config = SCRecordingOutputConfiguration::new()?
            .with_output_url(&path)
            .map_err(|e| anyhow!("Couldn't record to {} ({e:?}).", path.display()))?
            .with_video_codec(match self.codec {
                Codec::H264 => SCRecordingOutputCodec::H264,
                Codec::Hevc => SCRecordingOutputCodec::HEVC,
            })
            .with_output_file_type(SCRecordingOutputFileType::MP4);
        let (done_tx, done) = channel();
        let failed_done = done_tx.clone();
        let failed = self.events.clone();
        let callbacks = RecordingCallbacks::new()
            .on_fail(move |err| {
                let _ = failed.send(RecorderEvent::Failed(err));
                let _ = failed_done.send(());
            })
            .on_finish(move || {
                let _ = done_tx.send(());
            });
        let output = SCRecordingOutput::new_with_delegate(&config, callbacks)
            .context("Recording to a file needs macOS 15 or later")?;
        self.parts.push(path);
        Ok(Part { output, done })
    }

    /// Finishes the current part; the stream (and the camera bubble) keep running.
    pub fn pause(&mut self) -> Result<()> {
        if let Some(part) = self.part.take() {
            self.stream
                .remove_recording_output(&part.output)
                .map_err(|e| anyhow!("Pausing the capture didn't work ({e})."))?;
            part.wait();
        }
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        if self.part.is_some() {
            return Ok(());
        }
        let part = self.next_part()?;
        if let Err(e) = self.stream.add_recording_output(&part.output) {
            self.parts.pop();
            return Err(anyhow!("Resuming the capture didn't work ({e})."));
        }
        self.part = Some(part);
        Ok(())
    }

    /// Stops capture and saves the joined recording, or moves the parts to the Trash when
    /// `keep` is false. Returns the saved file.
    pub fn finish(mut self, keep: bool) -> Result<Option<PathBuf>> {
        let stopped = self
            .stream
            .stop_capture()
            .map_err(|e| anyhow!("The screen capture didn't stop cleanly ({e})."));
        if let Some(part) = self.part.take() {
            part.wait();
        }
        if !keep {
            let _ = crate::native::trash(&self.parts_dir);
            return stopped.map(|()| None);
        }
        // A part paused right after it started may have no file.
        let parts: Vec<PathBuf> = self
            .parts
            .iter()
            .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0))
            .cloned()
            .collect();
        match parts.as_slice() {
            [] => bail!("Nothing was recorded."),
            [only] => move_file(only, &self.path)?,
            _ => {
                if let Err(err) = crate::media::join(&parts, &self.path) {
                    // Don't lose the recording: save the parts side by side instead.
                    // Names contain dots ("at 12.50.40"), so build them rather than swap extensions.
                    let stem = self
                        .path
                        .file_stem()
                        .map_or("Annuncia".into(), |s| s.to_string_lossy().into_owned());
                    for (i, part) in parts.iter().enumerate() {
                        let name = format!("{stem} part {}.mp4", i + 1);
                        let _ = move_file(part, &self.path.with_file_name(name));
                    }
                    let _ = std::fs::remove_dir_all(&self.parts_dir);
                    return Err(
                        err.context("Couldn't join the parts, so they were saved separately")
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.parts_dir);
        Ok(Some(self.path))
    }
}
