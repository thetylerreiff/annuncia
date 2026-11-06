//! Saving an edit without re-encoding all of it.
//!
//! A recording's video is a run of closed GOPs: a keyframe about every second, and frames that
//! only refer back to their own keyframe. A GOP that an edit keeps whole can be copied byte for
//! byte, so only the partial GOPs at each cut are re-encoded. The audio comes out exactly as a
//! full export writes it.
//!
//! The copied and re-encoded frames share one track, which only plays everywhere if the encoder
//! made the re-encoded pieces with exactly the recording's codec settings. VideoToolbox does,
//! for recordings made by Annuncia, but this checks every piece, and returns an error to fall
//! back to re-encoding everything when anything doesn't line up.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderStatus, AVAssetReaderTrackOutput, AVAssetTrack, AVAssetWriter,
    AVAssetWriterInput, AVAssetWriterStatus, AVFileTypeMPEG4, AVMediaType, AVMediaTypeAudio,
    AVMediaTypeVideo, AVURLAsset,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags, CMTimeRange,
    kCMTimeInvalid, kCMTimeZero,
};

use crate::media::{self, Encode, Source, asset, file_url, time};
use crate::settings::Codec;

/// Closer than this to a keyframe counts as on it (frames are at least 1/120 s apart).
const SNAP: f64 = 1e-3;
/// How many pieces to re-encode at once.
const WORKERS: usize = 4;
/// Below this many seconds of whole GOPs to copy, re-encoding everything is as quick.
const MIN_COPY: f64 = 2.;

/// Writes `ranges` of the recording at `path` to `out`, as [`media::export`] would with
/// `Encode::Reencode(codec)`. Errors when the recording can't be cut this way; `out` may then
/// be left half-written.
#[allow(deprecated)] // The synchronous accessors are fine on a worker thread.
pub fn export(
    path: &Path,
    ranges: &[Range<f64>],
    codec: Codec,
    out: &Path,
    progress: &(impl Fn(f32) + Sync),
) -> Result<()> {
    // SAFETY: every AVFoundation object here stays on the thread that made it.
    let (format, plan, has_audio) = unsafe {
        let asset = asset(path);
        let track = only_track(&asset, AVMediaTypeVideo)?.context("no video")?;
        let format = only_format(&track)?;
        let subtype = format.media_sub_type().to_be_bytes();
        let expected: &[[u8; 4]] = match codec {
            Codec::H264 => &[*b"avc1"],
            Codec::Hevc => &[*b"hvc1", *b"hev1"],
        };
        ensure!(
            expected.contains(&subtype),
            "the recording isn't in the chosen codec"
        );
        let span = track.timeRange();
        let end = span.start.seconds() + span.duration.seconds();
        let offset = media_offset(&track)?.seconds();
        let k: Vec<f64> = keyframes(&track)?.into_iter().map(|k| k - offset).collect();
        let has_audio = asset.tracksWithMediaType(AVMediaTypeAudio.unwrap()).count() > 0;
        (format, plan(ranges, &k, end), has_audio)
    };
    let copied: f64 = plan
        .iter()
        .filter(|p| p.copy)
        .map(|p| p.source.end - p.source.start)
        .sum();
    ensure!(copied >= MIN_COPY, "too little to copy");

    // Re-encode the pieces around the cuts, and all the audio, side by side.
    let temp = TempFiles::new();
    // (source, whether it's the audio). The audio takes longest, so it goes first.
    let audio = has_audio.then(|| {
        let source = Source {
            path: path.to_path_buf(),
            ranges: Some(ranges.to_vec()),
        };
        (source, true)
    });
    let video = plan.iter().filter(|p| !p.copy).map(|p| {
        let source = Source {
            path: path.to_path_buf(),
            ranges: Some(vec![p.source.clone()]),
        };
        (source, false)
    });
    let jobs: Vec<(Source, bool)> = audio.into_iter().chain(video).collect();
    let outs: Vec<PathBuf> = (0..jobs.len()).map(|i| temp.path(i)).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let first_error = std::sync::Mutex::new(None);
    let format_ref: &CMFormatDescription = &format;
    thread::scope(|scope| {
        for _ in 0..WORKERS.min(jobs.len()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((source, is_audio)) = jobs.get(i) else {
                        break;
                    };
                    // SAFETY: reading framework constants.
                    let media = unsafe {
                        if *is_audio {
                            AVMediaTypeAudio
                        } else {
                            AVMediaTypeVideo
                        }
                    };
                    let result = media::render(
                        std::slice::from_ref(source),
                        &outs[i],
                        Encode::Reencode(codec),
                        media,
                        |_| {},
                    );
                    // Stop at the first piece that won't fit the recording's track, rather
                    // than re-encode the rest for nothing.
                    // SAFETY: the asset stays on this thread.
                    let result = result.and_then(|()| unsafe {
                        if !*is_audio {
                            let (_, track, _) = Samples::video(&outs[i], None)?;
                            ensure!(
                                CMFormatDescription::equal(
                                    Some(&*only_format(&track)?),
                                    Some(format_ref)
                                ),
                                "a re-encoded piece doesn't match the recording's codec settings"
                            );
                        }
                        Ok(())
                    });
                    if let Err(err) = result {
                        first_error.lock().unwrap().get_or_insert(err);
                        next.store(jobs.len(), Ordering::Relaxed);
                        break;
                    }
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    progress(0.8 * n as f32 / jobs.len() as f32);
                }
            });
        }
    });
    if let Some(err) = first_error.into_inner().unwrap() {
        return Err(err.context("re-encoding around the cuts"));
    }

    let (audio, encoded) = outs.split_at(usize::from(has_audio));
    let total: f64 = ranges.iter().map(|r| r.end - r.start).sum();
    let audio = audio.first().map(PathBuf::as_path);
    // SAFETY: as above.
    unsafe { stitch(path, &format, &plan, encoded, audio, out, total, progress) }?;
    progress(1.);
    Ok(())
}

/// A stretch of the edited video: copied from the recording, or re-encoded.
#[derive(Clone, Debug, PartialEq)]
struct Piece {
    source: Range<f64>,
    /// Where it starts in the edited video.
    at: f64,
    copy: bool,
}

/// Splits each kept range into whole GOPs to copy, and the partial ones at its ends to
/// re-encode. `keyframes` is in order, and `end` is where the video ends (the audio can run
/// a little longer).
fn plan(ranges: &[Range<f64>], keyframes: &[f64], end: f64) -> Vec<Piece> {
    // Placed exactly where `media::render` would put the same frames, which rounds each
    // range's start and length to its time scale.
    let ticks = |seconds: f64| (seconds * f64::from(media::TIMESCALE)).round();
    let mut pieces = Vec::new();
    let mut range_at = 0.;
    for range in ranges {
        let mut push = |source: Range<f64>, copy: bool| {
            if source.end - source.start > SNAP {
                let at = (range_at + ticks(source.start) - ticks(range.start))
                    / f64::from(media::TIMESCALE);
                pieces.push(Piece { source, at, copy });
            }
        };
        let range_end = range.end.min(end);
        let first = keyframes.iter().find(|&&k| k >= range.start - SNAP);
        let last = keyframes
            .iter()
            .chain([&end])
            .rev()
            .find(|&&k| k <= range_end + SNAP);
        match (first, last) {
            (Some(&first), Some(&last)) if last - first > SNAP => {
                push(range.start..first, false);
                push(first..last, true);
                push(last..range_end, false);
            }
            _ => push(range.start..range_end, false),
        }
        range_at += ticks(range.end - range.start);
    }
    pieces
}

/// Presentation times of the keyframes that start a closed GOP, in order. Errors if any GOP
/// isn't closed, since then it can't be copied on its own.
unsafe fn keyframes(track: &AVAssetTrack) -> Result<Vec<f64>> {
    unsafe {
        ensure!(
            track.canProvideSampleCursors(),
            "can't step through the frames"
        );
        let cursor = track
            .makeSampleCursorAtFirstSampleInDecodeOrder()
            .context("no frames")?;
        let mut keyframes: Vec<f64> = Vec::new();
        // Latest presentation time in the GOP so far.
        let mut latest = f64::NEG_INFINITY;
        loop {
            let pts = cursor.presentationTimeStamp().seconds();
            if cursor.currentSampleSyncInfo().sampleIsFullSync.as_bool() {
                ensure!(pts > latest, "a GOP shows frames after the next keyframe");
                keyframes.push(pts);
            } else {
                let start = keyframes.last().context("doesn't start on a keyframe")?;
                ensure!(pts > *start, "a GOP shows frames before its keyframe");
            }
            latest = latest.max(pts);
            if cursor.stepInDecodeOrderByCount(1) != 1 {
                break;
            }
        }
        Ok(keyframes)
    }
}

#[allow(deprecated)]
unsafe fn only_track(
    asset: &AVURLAsset,
    media: Option<&AVMediaType>,
) -> Result<Option<Retained<AVAssetTrack>>> {
    unsafe {
        let tracks = asset.tracksWithMediaType(media.context("no media type")?);
        ensure!(tracks.count() <= 1, "more than one track");
        Ok(tracks.firstObject())
    }
}

#[allow(deprecated)]
unsafe fn only_format(track: &AVAssetTrack) -> Result<CFRetained<CMFormatDescription>> {
    unsafe {
        let formats = track.formatDescriptions();
        ensure!(formats.count() == 1, "the track changes format part way");
        let format = formats.objectAtIndex(0);
        let ptr = NonNull::new(Retained::as_ptr(&format) as *mut CMFormatDescription)
            .context("no format")?;
        Ok(CFRetained::retain(ptr))
    }
}

/// How far a track's samples' timestamps are ahead of its timeline: tracks with B-frames start
/// with an edit that skips the first few hundredths of a second of media time.
#[allow(deprecated)]
unsafe fn media_offset(track: &AVAssetTrack) -> Result<CMTime> {
    unsafe {
        let mut offset = None;
        for segment in track.segments().iter() {
            if segment.isEmpty() {
                continue;
            }
            let map = segment.timeMapping();
            ensure!(offset.is_none(), "the track has edits");
            ensure!(
                map.source.duration.compare(map.target.duration) == 0,
                "the track changes speed"
            );
            offset = Some(map.source.start.subtract(map.target.start));
        }
        offset.context("the track is empty")
    }
}

/// Reads one track's compressed samples from a file.
struct Samples {
    reader: Retained<AVAssetReader>,
    output: Retained<AVAssetReaderTrackOutput>,
}

impl Samples {
    /// Reads `track` of `asset`, or the part of it in `range`.
    unsafe fn new(
        asset: &AVURLAsset,
        track: &AVAssetTrack,
        range: Option<CMTimeRange>,
    ) -> Result<Self> {
        unsafe {
            let reader = AVAssetReader::assetReaderWithAsset_error(asset)
                .map_err(|e| anyhow!("{}", e.localizedDescription()))?;
            let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                track, None,
            );
            output.setAlwaysCopiesSampleData(false);
            reader.addOutput(&output);
            if let Some(range) = range {
                reader.setTimeRange(range);
            }
            ensure!(reader.startReading(), "couldn't read a track");
            Ok(Self { reader, output })
        }
    }

    /// Reads the video of the file at `path`. Also returns its track, and the media time at
    /// the start of the track's timeline, which is what the samples' timestamps count from.
    unsafe fn video(
        path: &Path,
        range: Option<CMTimeRange>,
    ) -> Result<(Self, Retained<AVAssetTrack>, CMTime)> {
        unsafe {
            let asset = asset(path);
            let track = only_track(&asset, AVMediaTypeVideo)?.context("no video")?;
            let samples = Self::new(&asset, &track, range)?;
            let offset = media_offset(&track)?;
            Ok((samples, track, offset))
        }
    }

    unsafe fn next(&self) -> Result<Option<Retained<CMSampleBuffer>>> {
        unsafe {
            match self.output.copyNextSampleBuffer() {
                Some(sample) => Ok(Some(sample)),
                None if self.reader.status() == AVAssetReaderStatus::Failed => {
                    bail!("reading failed")
                }
                None => Ok(None),
            }
        }
    }
}

/// The edited video's frames in decode order, moved to their place in the edit.
struct VideoFeed<'a> {
    path: &'a Path,
    format: &'a CMFormatDescription,
    plan: &'a [Piece],
    encoded: &'a [PathBuf],
    piece: usize,
    encoded_seen: usize,
    current: Option<(Samples, Piece, CMTime)>,
    last_dts: Option<CMTime>,
}

impl VideoFeed<'_> {
    unsafe fn next(&mut self) -> Result<Option<Retained<CMSampleBuffer>>> {
        unsafe {
            loop {
                if self.current.is_none() {
                    let Some(piece) = self.plan.get(self.piece).cloned() else {
                        return Ok(None);
                    };
                    self.piece += 1;
                    // `origin`: the media time the piece starts at.
                    let (samples, origin) = if piece.copy {
                        let range = CMTimeRange::new(
                            time(piece.source.start),
                            time(piece.source.end - piece.source.start),
                        );
                        let (samples, _, offset) = Samples::video(self.path, Some(range))?;
                        (samples, time(piece.source.start).add(offset))
                    } else {
                        let file = &self.encoded[self.encoded_seen];
                        self.encoded_seen += 1;
                        let (samples, track, offset) = Samples::video(file, None)?;
                        // The piece has to share the recording's codec settings exactly.
                        let format = only_format(&track)?;
                        ensure!(
                            CMFormatDescription::equal(Some(&format), Some(self.format)),
                            "a re-encoded piece doesn't match the recording's codec settings"
                        );
                        (samples, offset)
                    };
                    self.current = Some((samples, piece, origin));
                }
                let (samples, piece, origin) = self.current.as_ref().unwrap();
                let Some(sample) = samples.next()? else {
                    self.current = None;
                    continue;
                };
                if sample.num_samples() == 0 {
                    continue;
                }
                // The reader may hand back frames around the range it was asked for.
                let t = sample.presentation_time_stamp().subtract(*origin).seconds();
                if t < -SNAP || t >= piece.source.end - piece.source.start - SNAP {
                    continue;
                }
                let sample = retime(&sample, time(piece.at).subtract(*origin), self.last_dts)?;
                self.last_dts = Some(sample.decode_time_stamp());
                return Ok(Some(sample));
            }
        }
    }
}

/// `sample`, `shift` later. Its decode time is nudged to just after `after` if it would
/// otherwise come before it: the pieces' encoders can disagree by a tick about how far decode
/// times run ahead of presentation times.
unsafe fn retime(
    sample: &CMSampleBuffer,
    shift: CMTime,
    after: Option<CMTime>,
) -> Result<Retained<CMSampleBuffer>> {
    unsafe {
        let mut count = 0;
        sample.sample_timing_info_array(0, std::ptr::null_mut(), &mut count);
        let mut timing = vec![
            CMSampleTimingInfo {
                duration: kCMTimeInvalid,
                presentationTimeStamp: kCMTimeInvalid,
                decodeTimeStamp: kCMTimeInvalid,
            };
            count as usize
        ];
        let status =
            sample.sample_timing_info_array(count, timing.as_mut_ptr(), std::ptr::null_mut());
        ensure!(status == 0, "couldn't read frame timing ({status})");
        for t in &mut timing {
            t.presentationTimeStamp = t.presentationTimeStamp.add(shift);
            if t.decodeTimeStamp.flags.contains(CMTimeFlags::Valid) {
                t.decodeTimeStamp = t.decodeTimeStamp.add(shift);
                if let Some(after) = after
                    && t.decodeTimeStamp.compare(after) <= 0
                {
                    t.decodeTimeStamp = after.add(CMTime::new(1, 90_000));
                    ensure!(
                        t.decodeTimeStamp.compare(t.presentationTimeStamp) <= 0,
                        "the pieces' frame order doesn't line up"
                    );
                }
            }
        }
        let mut copy: *mut CMSampleBuffer = std::ptr::null_mut();
        let status = CMSampleBuffer::create_copy_with_new_timing(
            None,
            sample,
            count,
            timing.as_ptr(),
            NonNull::from(&mut copy),
        );
        ensure!(
            status == 0 && !copy.is_null(),
            "couldn't move a frame ({status})"
        );
        Ok(Retained::from_raw(copy).unwrap())
    }
}

/// Writes the planned video and the re-encoded audio into `out`, without re-encoding either.
#[allow(clippy::too_many_arguments, deprecated)] // Synchronous loading is fine on a worker thread.
unsafe fn stitch(
    path: &Path,
    format: &CMFormatDescription,
    plan: &[Piece],
    encoded: &[PathBuf],
    audio: Option<&Path>,
    out: &Path,
    total: f64,
    progress: &impl Fn(f32),
) -> Result<()> {
    unsafe {
        let writer = AVAssetWriter::assetWriterWithURL_fileType_error(
            &file_url(out),
            AVFileTypeMPEG4.context("no MPEG-4 file type")?,
        )
        .map_err(|e| anyhow!("{}", e.localizedDescription()))?;

        let source_track = only_track(&asset(path), AVMediaTypeVideo)?.context("no video")?;
        let video_in =
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings_sourceFormatHint(
                AVMediaTypeVideo.unwrap(),
                None,
                Some(format),
            );
        video_in.setExpectsMediaDataInRealTime(false);
        video_in.setTransform(source_track.preferredTransform());
        writer.addInput(&video_in);

        // Each audio track, with its next sample.
        let mut audio_feeds = Vec::new();
        if let Some(audio) = audio {
            let audio = asset(audio);
            for track in audio.tracksWithMediaType(AVMediaTypeAudio.unwrap()).iter() {
                let input =
                    AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings_sourceFormatHint(
                        AVMediaTypeAudio.unwrap(),
                        None,
                        Some(&*only_format(&track)?),
                    );
                input.setExpectsMediaDataInRealTime(false);
                writer.addInput(&input);
                let samples = Samples::new(&audio, &track, None)?;
                let next = samples.next()?;
                audio_feeds.push((samples, input, next));
            }
        }

        ensure!(writer.startWriting(), "couldn't start writing");
        writer.startSessionAtSourceTime(kCMTimeZero);

        let mut video = VideoFeed {
            path,
            format,
            plan,
            encoded,
            piece: 0,
            encoded_seen: 0,
            current: None,
            last_dts: None,
        };
        let mut next_video = video.next()?;
        let failed = |what: &str| {
            let reason = writer.error().map_or("unknown error".into(), |e| {
                e.localizedDescription().to_string()
            });
            anyhow!("couldn't write {what}: {reason}")
        };
        let mut stalled = Instant::now();
        // Feed whichever input is ready, the one furthest behind first: the writer holds an
        // input back until the others catch up with it.
        loop {
            let video_ready = next_video
                .as_ref()
                .filter(|_| video_in.isReadyForMoreMediaData())
                .map(|s| (s.decode_time_stamp().seconds(), None));
            let audio_ready = audio_feeds
                .iter()
                .enumerate()
                .filter_map(|(i, (_, input, next))| {
                    let next = next.as_ref().filter(|_| input.isReadyForMoreMediaData())?;
                    Some((next.presentation_time_stamp().seconds(), Some(i)))
                });
            let ready = video_ready.into_iter().chain(audio_ready);
            let Some((_, pick)) = ready.min_by(|a, b| a.0.total_cmp(&b.0)) else {
                if next_video.is_none() && audio_feeds.iter().all(|f| f.2.is_none()) {
                    break;
                }
                ensure!(
                    writer.status() == AVAssetWriterStatus::Writing,
                    failed("the file")
                );
                ensure!(
                    stalled.elapsed() < Duration::from_secs(10),
                    "the writer stalled"
                );
                thread::sleep(Duration::from_micros(200));
                continue;
            };
            stalled = Instant::now();
            match pick {
                None => {
                    let sample = next_video.take().unwrap();
                    if !video_in.appendSampleBuffer(&sample) {
                        return Err(failed("a frame"));
                    }
                    let at = sample.presentation_time_stamp().seconds();
                    progress(0.8 + 0.2 * (at / total).min(1.) as f32);
                    next_video = video.next()?;
                }
                Some(i) => {
                    let (samples, input, next) = &mut audio_feeds[i];
                    if !input.appendSampleBuffer(&next.take().unwrap()) {
                        return Err(failed("the audio"));
                    }
                    *next = samples.next()?;
                }
            }
        }
        video_in.markAsFinished();
        for (_, input, _) in &audio_feeds {
            input.markAsFinished();
        }
        let (done_tx, done_rx) = channel();
        let handler = RcBlock::new(move || {
            let _ = done_tx.send(());
        });
        writer.finishWritingWithCompletionHandler(&handler);
        let _ = done_rx.recv();
        ensure!(
            writer.status() == AVAssetWriterStatus::Completed,
            failed("the file")
        );
        Ok(())
    }
}

/// Scratch files for the re-encoded pieces, deleted when dropped.
struct TempFiles {
    dir: PathBuf,
}

impl TempFiles {
    fn new() -> Self {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "annuncia-cut-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    fn path(&self, i: usize) -> PathBuf {
        self.dir.join(format!("{i}.mp4"))
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(source: Range<f64>, at: f64, copy: bool) -> Piece {
        Piece { source, at, copy }
    }

    #[test]
    fn copies_whole_gops_and_reencodes_the_ends() {
        let keys = [0., 1., 2., 3., 4.];
        assert_eq!(
            plan(&[0.5..3.5], &keys, 5.),
            [
                piece(0.5..1., 0., false),
                piece(1.0..3., 0.5, true),
                piece(3.0..3.5, 2.5, false),
            ]
        );
    }

    #[test]
    fn cuts_on_keyframes_copy_everything() {
        let keys = [0., 1., 2., 3., 4.];
        assert_eq!(
            plan(&[0.0..2., 3.0..5.], &keys, 5.),
            [piece(0.0..2., 0., true), piece(3.0..5., 2., true)]
        );
    }

    #[test]
    fn stops_at_the_end_of_the_video() {
        let keys = [0., 1., 2.];
        assert_eq!(
            plan(&[0.5..3.04], &keys, 3.),
            [piece(0.5..1., 0., false), piece(1.0..3., 0.5, true)]
        );
    }

    #[test]
    fn a_range_inside_one_gop_is_reencoded() {
        let keys = [0., 1., 2.];
        assert_eq!(plan(&[1.2..1.8], &keys, 3.), [piece(1.2..1.8, 0., false)]);
    }
}
