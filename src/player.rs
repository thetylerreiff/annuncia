//! Video playback for the editor: `AVPlayer` handles decoding, timing and audio, and an
//! `AVPlayerItemVideoOutput` hands us each frame as an NV12 pixel buffer for GPUI's `surface()`,
//! the same path the camera bubble uses.

use std::path::Path;

use core_foundation::base::TCFType as _;
use core_video::pixel_buffer::CVPixelBuffer;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread as _, MainThreadMarker};
use objc2_av_foundation::{AVPlayer, AVPlayerItem, AVPlayerItemVideoOutput};
use objc2_core_media::{CMTime, kCMTimeZero};
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use objc2_quartz_core::CACurrentMediaTime;

use crate::media::file_url;

/// `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`, the format GPUI surfaces accept.
const NV12_FULL_RANGE: u32 = u32::from_be_bytes(*b"420f");

pub struct Player {
    player: Retained<AVPlayer>,
    output: Retained<AVPlayerItemVideoOutput>,
    frame: Option<CVPixelBuffer>,
}

impl Player {
    pub fn new(path: &Path, mtm: MainThreadMarker) -> Self {
        // SAFETY: AVFoundation objects created and used on the main thread (`mtm`).
        unsafe {
            let item = AVPlayerItem::playerItemWithURL(&file_url(path), mtm);
            let keys = [
                NSString::from_str("PixelFormatType"),
                // Surfaces need IOSurface-backed buffers.
                NSString::from_str("IOSurfaceProperties"),
            ];
            let format: Retained<AnyObject> =
                Retained::into_super(Retained::into_super(NSNumber::new_u32(NV12_FULL_RANGE)))
                    .into();
            let iosurface: Retained<AnyObject> = Retained::into_super(Retained::into_super(
                NSDictionary::<NSString, AnyObject>::new(),
            ));
            let attributes = NSDictionary::<NSString, AnyObject>::from_retained_objects(
                &[&*keys[0], &*keys[1]],
                &[format, iosurface],
            );
            let output = AVPlayerItemVideoOutput::initWithPixelBufferAttributes(
                AVPlayerItemVideoOutput::alloc(),
                Some(&attributes),
            );
            item.addOutput(&output);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            Self {
                player,
                output,
                frame: None,
            }
        }
    }

    /// Pulls the frame due now, if it changed. Call on every redraw tick.
    pub fn poll(&mut self) -> bool {
        // SAFETY: main-thread calls on objects we own.
        unsafe {
            let now = self.output.itemTimeForHostTime(CACurrentMediaTime());
            if !self.output.hasNewPixelBufferForItemTime(now) {
                return false;
            }
            let Some(buffer) = self
                .output
                .copyPixelBufferForItemTime_itemTimeForDisplay(now, std::ptr::null_mut())
            else {
                return false;
            };
            // SAFETY: `buffer` is a live CVPixelBuffer; the get rule adds our own retain and
            // `buffer` releases its copy when dropped.
            let raw = Retained::as_ptr(&buffer) as *mut _;
            self.frame = Some(CVPixelBuffer::wrap_under_get_rule(raw));
            true
        }
    }

    pub fn frame(&self) -> Option<CVPixelBuffer> {
        self.frame.clone()
    }

    pub fn is_playing(&self) -> bool {
        // SAFETY: property read on the main thread.
        unsafe { self.player.rate() != 0. }
    }

    pub fn play(&self) {
        // SAFETY: main-thread call.
        unsafe { self.player.play() }
    }

    pub fn pause(&self) {
        // SAFETY: main-thread call.
        unsafe { self.player.pause() }
    }

    /// Current position in seconds.
    pub fn time(&self) -> f64 {
        // SAFETY: property read on the main thread.
        let t = unsafe { self.player.currentTime().seconds() };
        if t.is_finite() { t } else { 0. }
    }

    /// `exact` seeks are frame-accurate but slower; use loose seeks while scrubbing.
    pub fn seek(&self, seconds: f64, exact: bool) {
        // SAFETY: main-thread call with plain time values.
        unsafe {
            let time = CMTime::with_seconds(seconds.max(0.), 600);
            let tolerance = if exact {
                kCMTimeZero
            } else {
                CMTime::with_seconds(0.15, 600)
            };
            self.player
                .seekToTime_toleranceBefore_toleranceAfter(time, tolerance, tolerance);
        }
    }
}
