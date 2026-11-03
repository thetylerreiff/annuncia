//! Webcam capture with AVFoundation. Frames arrive as NV12 (`420f`) pixel buffers, which GPUI's
//! `surface()` element draws directly on the GPU.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow};
use core_foundation::base::TCFType as _;
use core_video::pixel_buffer::CVPixelBuffer;
use dispatch2::{DispatchQueue, DispatchQueueAttr};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceDiscoverySession,
    AVCaptureDeviceInput, AVCaptureDevicePosition, AVCaptureDeviceTypeBuiltInWideAngleCamera,
    AVCaptureDeviceTypeContinuityCamera, AVCaptureDeviceTypeExternal, AVCaptureOutput,
    AVCaptureSession, AVCaptureSessionPreset1280x720, AVCaptureVideoDataOutput,
    AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use crate::popout::{Compositor, PopoutSlot};

/// `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`, the only format GPUI surfaces accept.
const NV12_FULL_RANGE: u32 = u32::from_be_bytes(*b"420f");

/// Latest camera frame, shared between the capture queue and the bubble view.
#[derive(Default)]
pub struct FrameSlot {
    frame: Mutex<Option<CVPixelBuffer>>,
    seq: AtomicU64,
}

// SAFETY: CVPixelBuffers are reference counted and safe to hand between threads once written;
// access is serialized by the mutex.
unsafe impl Send for FrameSlot {}
unsafe impl Sync for FrameSlot {}

impl FrameSlot {
    pub fn latest(&self) -> Option<CVPixelBuffer> {
        self.frame.lock().ok()?.clone()
    }

    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    pub fn clear(&self) {
        if let Ok(mut frame) = self.frame.lock() {
            *frame = None;
        }
        self.seq.fetch_add(1, Ordering::AcqRel);
    }

    fn store(&self, buffer: CVPixelBuffer) {
        if let Ok(mut frame) = self.frame.lock() {
            *frame = Some(buffer);
        }
        self.seq.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug)]
pub struct CameraDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    Undetermined,
}

pub fn access() -> Access {
    let Some(video) = (unsafe { AVMediaTypeVideo }) else {
        return Access::Denied;
    };
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(video) } {
        AVAuthorizationStatus::Authorized => Access::Granted,
        AVAuthorizationStatus::NotDetermined => Access::Undetermined,
        _ => Access::Denied,
    }
}

/// Shows the system prompt; `done` runs on an arbitrary queue.
pub fn request_access(done: impl Fn(bool) + Send + Sync + 'static) {
    let Some(video) = (unsafe { AVMediaTypeVideo }) else {
        done(false);
        return;
    };
    let block = block2::RcBlock::new(move |granted: Bool| done(granted.as_bool()));
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(video, &block) };
}

pub fn devices() -> Vec<CameraDevice> {
    unsafe {
        let types = NSArray::from_slice(&[
            AVCaptureDeviceTypeBuiltInWideAngleCamera,
            AVCaptureDeviceTypeContinuityCamera,
            AVCaptureDeviceTypeExternal,
        ]);
        let session =
            AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                &types,
                AVMediaTypeVideo,
                AVCaptureDevicePosition::Unspecified,
            );
        session
            .devices()
            .iter()
            .map(|d| CameraDevice {
                id: d.uniqueID().to_string(),
                name: d.localizedName().to_string(),
            })
            .collect()
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "AnnunciaCameraDelegate"]
    #[ivars = Frames]
    struct FrameDelegate;

    unsafe impl NSObjectProtocol for FrameDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for FrameDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            let Some(image) = (unsafe { sample_buffer.image_buffer() }) else {
                return;
            };
            let raw = objc2_core_foundation::CFRetained::as_ptr(&image).as_ptr();
            // SAFETY: `raw` is a live CVPixelBuffer; the get rule adds our own retain.
            let buffer = unsafe { CVPixelBuffer::wrap_under_get_rule(raw.cast()) };
            let frames = self.ivars();
            frames.slot.store(buffer);
            if let Some((compositor, popout)) = &frames.popout
                && let Ok(mut compositor) = compositor.lock()
            {
                compositor.process(&image, popout);
            }
        }
    }
);

/// Where each camera frame goes: the plain bubble, and the pop-out compositor when it's on.
struct Frames {
    slot: Arc<FrameSlot>,
    popout: Option<(Mutex<Compositor>, Arc<PopoutSlot>)>,
}

impl FrameDelegate {
    fn new(frames: Frames) -> Retained<Self> {
        let this = Self::alloc().set_ivars(frames);
        unsafe { msg_send![super(this), init] }
    }
}

/// Moves AVFoundation objects onto the capture queue.
struct OnQueue<T>(T);
// SAFETY: AVCaptureSession is documented as safe to start/stop from a background queue, and the
// delegate is only released there, after the session has stopped delivering frames.
unsafe impl<T> Send for OnQueue<T> {}

pub struct Camera {
    session: Retained<AVCaptureSession>,
    delegate: Option<Retained<FrameDelegate>>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
    slot: Arc<FrameSlot>,
    popout: Option<Arc<PopoutSlot>>,
}

impl Camera {
    /// `popout` (a bubble diameter in points, and where its frames go) composites every frame
    /// for the pop-out bubble as well.
    pub fn start(
        device_id: Option<&str>,
        slot: Arc<FrameSlot>,
        popout: Option<(f64, Arc<PopoutSlot>)>,
    ) -> Result<Self> {
        if access() == Access::Denied {
            return Err(anyhow!(
                "Camera access is off for Annuncia in System Settings"
            ));
        }
        unsafe {
            let video = AVMediaTypeVideo.context("AVFoundation unavailable")?;
            let device = device_id
                .and_then(|id| AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(id)))
                .or_else(|| AVCaptureDevice::defaultDeviceWithMediaType(video))
                .context("No camera found")?;

            let session = AVCaptureSession::new();
            session.beginConfiguration();
            if session.canSetSessionPreset(AVCaptureSessionPreset1280x720) {
                session.setSessionPreset(AVCaptureSessionPreset1280x720);
            }

            let input =
                AVCaptureDeviceInput::deviceInputWithDevice_error(&device).map_err(|e| {
                    anyhow!(
                        "Could not open {}: {}",
                        device.localizedName(),
                        e.localizedDescription()
                    )
                })?;
            if !session.canAddInput(&input) {
                return Err(anyhow!("Camera is busy"));
            }
            session.addInput(&input);

            let output = AVCaptureVideoDataOutput::new();
            let key = NSString::from_str("PixelFormatType");
            let format = NSNumber::new_u32(NV12_FULL_RANGE);
            let settings = NSDictionary::<NSString, AnyObject>::from_retained_objects(
                &[&*key],
                &[Retained::into_super(Retained::into_super(format)).into()],
            );
            output.setVideoSettings(Some(&settings));
            output.setAlwaysDiscardsLateVideoFrames(true);

            let popout_slot = popout.as_ref().map(|(_, slot)| slot.clone());
            let popout = popout.and_then(|(diameter, slot)| {
                let compositor = Compositor::new(diameter);
                if compositor.is_none() {
                    eprintln!("annuncia: the pop-out bubble couldn't start; showing the plain one");
                }
                Some((Mutex::new(compositor?), slot))
            });
            let delegate = FrameDelegate::new(Frames {
                slot: slot.clone(),
                popout,
            });
            let queue = DispatchQueue::new("dev.annuncia.camera", DispatchQueueAttr::SERIAL);
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
            if !session.canAddOutput(&output) {
                return Err(anyhow!("Camera output unavailable"));
            }
            session.addOutput(&output);

            // Selfie view: mirror like FaceTime and Photo Booth do.
            if let Some(connection) = output.connectionWithMediaType(video)
                && connection.isVideoMirroringSupported()
            {
                connection.setAutomaticallyAdjustsVideoMirroring(false);
                connection.setVideoMirrored(true);
            }
            session.commitConfiguration();

            // startRunning blocks for a few hundred ms; keep it off the main thread.
            let runner = OnQueue(session.clone());
            queue.exec_async(move || {
                let runner = runner;
                runner.0.startRunning();
            });

            Ok(Self {
                session,
                delegate: Some(delegate),
                _queue: queue,
                slot,
                popout: popout_slot,
            })
        }
    }
}

impl Drop for Camera {
    fn drop(&mut self) {
        let job = OnQueue((
            self.session.clone(),
            self.delegate.take(),
            self.slot.clone(),
            self.popout.clone(),
        ));
        self._queue.exec_async(move || {
            let job = job;
            let (session, delegate, slot, popout) = job.0;
            unsafe { session.stopRunning() };
            // Frames already queued ran before this; now nothing references the delegate.
            drop(delegate);
            slot.clear();
            if let Some(popout) = popout {
                popout.clear();
            }
        });
    }
}
