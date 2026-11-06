//! AVFoundation work on finished recordings: joining the parts of a paused recording, exporting
//! an edit, reading the audio waveform for the editor's timeline, and library thumbnails. All of it blocks, so call
//! it off the main thread.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use block2::RcBlock;
use objc2::AnyThread as _;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetExportPresetHEVCHighestQuality, AVAssetExportPresetHighestQuality,
    AVAssetExportPresetPassthrough, AVAssetExportSession, AVAssetExportSessionStatus,
    AVAssetImageGenerator, AVAssetReader, AVAssetReaderAudioMixOutput, AVFileTypeMPEG4,
    AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo, AVMutableComposition,
    AVMutableCompositionTrack, AVURLAsset, AVVideoComposition,
};
use objc2_core_foundation::{CFString, CFURL, CGSize};
use objc2_core_media::{CMTime, CMTimeRange, kCMPersistentTrackID_Invalid, kCMTimeZero};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};
use objc2_image_io::CGImageDestination;

use crate::settings::Codec;

/// Time scale for edit points: divisible by 24, 25, 30 and 60 fps.
pub(crate) const TIMESCALE: i32 = 600;

/// A file, or the given ranges of it in seconds, to place one after another.
pub struct Source {
    pub path: PathBuf,
    /// `None` takes the whole file.
    pub ranges: Option<Vec<Range<f64>>>,
}

pub enum Encode {
    /// Copy the compressed tracks: instant, but cuts can only land cleanly on keyframes.
    Passthrough,
    /// Re-encode so every cut is frame-exact and the file plays everywhere.
    Reencode(Codec),
}

pub fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

pub(crate) fn asset(path: &Path) -> Retained<AVURLAsset> {
    // SAFETY: plain constructor; the asset loads lazily.
    unsafe { AVURLAsset::URLAssetWithURL_options(&file_url(path), None) }
}

pub(crate) fn time(seconds: f64) -> CMTime {
    // Rounded: `CMTime::with_seconds` truncates, which can land a tick before a frame.
    let ticks = (seconds * f64::from(TIMESCALE)).round() as i64;
    // SAFETY: pure value conversion.
    unsafe { CMTime::new(ticks, TIMESCALE) }
}

/// Length of a media file in seconds.
#[allow(deprecated)] // Synchronous loading is fine off the main thread.
pub fn duration(path: &Path) -> f64 {
    // SAFETY: reads a property of a local asset.
    unsafe { asset(path).duration().seconds() }
}

/// Joins the parts of a paused recording without re-encoding.
pub fn join(parts: &[PathBuf], out: &Path) -> Result<()> {
    let sources: Vec<Source> = parts
        .iter()
        .map(|path| Source {
            path: path.clone(),
            ranges: None,
        })
        .collect();
    export(&sources, out, Encode::Passthrough, |_| {})
}

/// Writes `sources` back to back into the MP4 at `out`, which must not exist yet. `progress`
/// receives values from 0 to 1 while the export runs.
pub fn export(
    sources: &[Source],
    out: &Path,
    encode: Encode,
    progress: impl Fn(f32) + Sync,
) -> Result<()> {
    // Cutting one recording: copy what's untouched and re-encode only around the cuts, which is
    // many times faster. If the recording doesn't allow that, re-encode all of it.
    if let (Encode::Reencode(codec), [source]) = (&encode, sources)
        && let Some(ranges) = &source.ranges
    {
        match crate::smart_cut::export(&source.path, ranges, *codec, out, &progress) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("annuncia: re-encoding the whole edit: {err:#}");
                let _ = std::fs::remove_file(out);
            }
        }
    }
    render(sources, out, encode, None, progress)
}

/// [`export`] without the fast path, and with only the tracks of media type `only` if given.
#[allow(deprecated)] // The synchronous `tracks`/`duration` accessors are fine on a worker thread.
pub(crate) fn render(
    sources: &[Source],
    out: &Path,
    encode: Encode,
    only: Option<&AVMediaType>,
    progress: impl Fn(f32),
) -> Result<()> {
    // SAFETY: the composition and export session live on this thread; the export's completion
    // handler only signals a channel.
    unsafe {
        let composition = AVMutableComposition::composition();
        // Recordings share one track layout (video, then the audio tracks), so the n-th track
        // of a media type in every source continues the same composition track.
        let mut targets: HashMap<(String, usize), Retained<AVMutableCompositionTrack>> =
            HashMap::new();
        let mut cursor = kCMTimeZero;
        for source in sources {
            let asset = asset(&source.path);
            let whole = 0.0..asset.duration().seconds();
            let ranges = source.ranges.clone().unwrap_or_else(|| vec![whole]);
            let tracks = asset.tracks();
            for range in &ranges {
                let mut seen: HashMap<String, usize> = HashMap::new();
                for track in tracks.iter() {
                    let media = track.mediaType();
                    if only.is_some_and(|only| *only != *media) {
                        continue;
                    }
                    let kind = media.to_string();
                    let n = seen.entry(kind.clone()).or_default();
                    let key = (kind, *n);
                    *n += 1;
                    // Tracks needn't span the whole file (audio often starts a few ms late).
                    let span = track.timeRange();
                    let track_start = span.start.seconds();
                    let track_end = track_start + span.duration.seconds();
                    let start = range.start.max(track_start);
                    let end = range.end.min(track_end);
                    if end <= start {
                        continue;
                    }
                    let target = match targets.get(&key) {
                        Some(target) => target.clone(),
                        None => {
                            let target = composition
                                .addMutableTrackWithMediaType_preferredTrackID(
                                    &media,
                                    kCMPersistentTrackID_Invalid,
                                )
                                .ok_or_else(|| anyhow!("couldn't add a {} track", key.0))?;
                            target.setPreferredTransform(track.preferredTransform());
                            targets.insert(key, target.clone());
                            target
                        }
                    };
                    target
                        .insertTimeRange_ofTrack_atTime_error(
                            CMTimeRange::new(time(start), time(end - start)),
                            &track,
                            cursor.add(time(start - range.start)),
                        )
                        .map_err(|e| anyhow!("{}", e.localizedDescription()))
                        .with_context(|| format!("reading {}", source.path.display()))?;
                }
                cursor = cursor.add(time(range.end - range.start));
            }
        }

        let preset = match encode {
            Encode::Passthrough => AVAssetExportPresetPassthrough,
            Encode::Reencode(Codec::H264) => AVAssetExportPresetHighestQuality,
            Encode::Reencode(Codec::Hevc) => AVAssetExportPresetHEVCHighestQuality,
        };
        let export = AVAssetExportSession::initWithAsset_presetName(
            AVAssetExportSession::alloc(),
            &composition,
            preset,
        )
        .context("couldn't create an export session")?;
        let has_video = composition
            .tracks()
            .iter()
            .any(|t| AVMediaTypeVideo.is_some_and(|video| *t.mediaType() == *video));
        if matches!(encode, Encode::Reencode(_)) && has_video {
            // Without a video composition the quality presets quietly copy matching tracks,
            // cuts and all. Rendering through one forces every frame to be encoded.
            export.setVideoComposition(Some(
                &AVVideoComposition::videoCompositionWithPropertiesOfAsset(&composition),
            ));
        }
        export.setOutputURL(Some(&file_url(out)));
        export.setOutputFileType(AVFileTypeMPEG4);
        let (done_tx, done_rx) = channel();
        let handler = RcBlock::new(move || {
            let _ = done_tx.send(());
        });
        export.exportAsynchronouslyWithCompletionHandler(&handler);
        loop {
            match done_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => progress(export.progress()),
            }
        }
        if export.status() != AVAssetExportSessionStatus::Completed {
            let reason = export.error().map_or("unknown error".to_string(), |e| {
                e.localizedDescription().to_string()
            });
            bail!("Couldn't save the video: {reason}");
        }
        progress(1.);
    }
    Ok(())
}

/// Peak loudness (0 to 1) of `samples` in `buckets` equal slices, for a waveform.
pub fn peaks(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let per_bucket = samples.len().div_ceil(buckets);
    samples
        .chunks(per_bucket)
        .map(|chunk| chunk.iter().fold(0f32, |peak, s| peak.max(s.abs())))
        .collect()
}

/// All audio tracks mixed to mono float samples at `rate` Hz. Empty when the file has no audio.
#[allow(deprecated)]
pub fn audio(path: &Path, rate: f64) -> Result<Vec<f32>> {
    // SAFETY: the reader and its output live on this thread; sample data is copied out of each
    // block buffer into a local vector before it's read.
    unsafe {
        let asset = asset(path);
        let tracks = asset.tracksWithMediaType(AVMediaTypeAudio.context("no audio media type")?);
        if tracks.is_empty() {
            return Ok(Vec::new());
        }
        let keys = [
            "AVFormatIDKey",
            "AVSampleRateKey",
            "AVNumberOfChannelsKey",
            "AVLinearPCMBitDepthKey",
            "AVLinearPCMIsFloatKey",
            "AVLinearPCMIsNonInterleaved",
            "AVLinearPCMIsBigEndianKey",
        ]
        .map(NSString::from_str);
        let values: [Retained<NSNumber>; 7] = [
            NSNumber::new_u32(u32::from_be_bytes(*b"lpcm")),
            NSNumber::new_f64(rate),
            NSNumber::new_u32(1),
            NSNumber::new_u32(32),
            NSNumber::new_bool(true),
            NSNumber::new_bool(false),
            NSNumber::new_bool(false),
        ];
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let value_objs: Vec<Retained<AnyObject>> = values
            .into_iter()
            .map(|v| Retained::into_super(Retained::into_super(v)).into())
            .collect();
        let settings =
            NSDictionary::<NSString, AnyObject>::from_retained_objects(&key_refs, &value_objs);

        let reader = AVAssetReader::assetReaderWithAsset_error(&asset)
            .map_err(|e| anyhow!("{}", e.localizedDescription()))?;
        let output =
            AVAssetReaderAudioMixOutput::assetReaderAudioMixOutputWithAudioTracks_audioSettings(
                &tracks,
                Some(&settings),
            );
        reader.addOutput(&output);
        if !reader.startReading() {
            bail!("couldn't read the audio");
        }

        let mut samples = Vec::new();
        let mut bytes = Vec::new();
        while let Some(sample) = output.copyNextSampleBuffer() {
            let Some(block) = sample.data_buffer() else {
                continue;
            };
            let len = block.data_length();
            bytes.resize(len, 0u8);
            let Some(dest) = NonNull::new(bytes.as_mut_ptr().cast()) else {
                continue;
            };
            if block.copy_data_bytes(0, len, dest) != 0 {
                continue;
            }
            samples.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&c| f32::from_ne_bytes(c)),
            );
        }
        Ok(samples)
    }
}

/// Writes a JPEG of the frame a second in (or halfway, for shorter clips), at most `width`
/// pixels wide.
#[allow(deprecated)] // The synchronous accessors are fine on a worker thread.
pub fn thumbnail(video: &Path, out: &Path, width: f64) -> Result<()> {
    // SAFETY: the generator and image stay on this thread; the URL is toll-free bridged to the
    // CFURL ImageIO expects.
    unsafe {
        let asset = asset(video);
        let at = (asset.duration().seconds() / 2.).min(1.);
        let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset);
        generator.setAppliesPreferredTrackTransform(true);
        generator.setMaximumSize(CGSize::new(width, 0.));
        let image = generator
            .copyCGImageAtTime_actualTime_error(time(at), std::ptr::null_mut())
            .map_err(|e| anyhow!("{}", e.localizedDescription()))?;
        let url = file_url(out);
        let url: &CFURL = &*(Retained::as_ptr(&url).cast::<CFURL>());
        let destination =
            CGImageDestination::with_url(url, &CFString::from_static_str("public.jpeg"), 1, None)
                .context("couldn't create the thumbnail file")?;
        destination.add_image(&image, None);
        if !destination.finalize() {
            bail!("couldn't write the thumbnail");
        }
        Ok(())
    }
}
