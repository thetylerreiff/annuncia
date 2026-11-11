//! The microphone's level, for the avatar that moves while you talk and the panel's meter. The
//! recording sends the microphone straight into the file, so the app never sees that audio; this
//! opens the same microphone in a small capture session of its own and reads AVFoundation's power
//! meter.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use dispatch2::{DispatchQueue, DispatchQueueAttr};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureAudioDataOutput, AVCaptureAudioDataOutputSampleBufferDelegate,
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession,
    AVMediaTypeAudio,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::NSString;

/// Quieter than this reads as silence, and louder than `LOUD_DB` as full voice. Ordinary
/// speech into a laptop mic sits around -45 to -35 dB.
const QUIET_DB: f32 = -52.;
const LOUD_DB: f32 = -28.;
/// Below this the microphone is delivering nothing at all (muted, unplugged or wrong device),
/// rather than a quiet room, which still reads around -60 dB.
const SILENT_DB: f32 = -80.;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "AnnunciaLevelDelegate"]
    #[ivars = Arc<AtomicU32>]
    struct LevelDelegate;

    unsafe impl NSObjectProtocol for LevelDelegate {}

    unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for LevelDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn did_output(
            &self,
            _output: &AVCaptureOutput,
            _sample_buffer: &CMSampleBuffer,
            connection: &AVCaptureConnection,
        ) {
            // The connection meters every buffer it delivers; keep the loudest channel's.
            let db = unsafe { connection.audioChannels() }
                .iter()
                .map(|channel| unsafe { channel.averagePowerLevel() })
                .fold(f32::NEG_INFINITY, f32::max);
            self.ivars().store(db.to_bits(), Ordering::Relaxed);
        }
    }
);

impl LevelDelegate {
    fn new(level: Arc<AtomicU32>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(level);
        unsafe { msg_send![super(this), init] }
    }
}

/// Moves AVFoundation objects onto the capture queue.
struct OnQueue<T>(T);
// SAFETY: AVCaptureSession is documented as safe to start/stop from a background queue, and the
// delegate is only released there, after the session has stopped delivering buffers.
unsafe impl<T> Send for OnQueue<T> {}

pub struct MicLevel {
    session: Retained<AVCaptureSession>,
    delegate: Option<Retained<LevelDelegate>>,
    queue: dispatch2::DispatchRetained<DispatchQueue>,
    db: Arc<AtomicU32>,
    device: Option<String>,
}

impl MicLevel {
    /// Starts metering the microphone with `device_id` (or the default one). Returns `None`
    /// rather than ask for access: the recording has already been granted it by then.
    pub fn start(device_id: Option<&str>) -> Option<Self> {
        unsafe {
            let audio = AVMediaTypeAudio?;
            if AVCaptureDevice::authorizationStatusForMediaType(audio)
                != AVAuthorizationStatus::Authorized
            {
                return None;
            }
            let device = device_id
                .and_then(|id| AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(id)))
                .or_else(|| AVCaptureDevice::defaultDeviceWithMediaType(audio))?;
            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device).ok()?;
            let session = AVCaptureSession::new();
            session.beginConfiguration();
            if !session.canAddInput(&input) {
                return None;
            }
            session.addInput(&input);
            let output = AVCaptureAudioDataOutput::new();
            let db = Arc::new(AtomicU32::new(f32::NEG_INFINITY.to_bits()));
            let delegate = LevelDelegate::new(db.clone());
            let queue = DispatchQueue::new(
                "co.thetylerreiff.annuncia.mic-level",
                DispatchQueueAttr::SERIAL,
            );
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );
            if !session.canAddOutput(&output) {
                return None;
            }
            session.addOutput(&output);
            session.commitConfiguration();
            // startRunning blocks; keep it off the main thread.
            let runner = OnQueue(session.clone());
            queue.exec_async(move || {
                let runner = runner;
                runner.0.startRunning();
            });
            Some(Self {
                session,
                delegate: Some(delegate),
                queue,
                db,
                device: device_id.map(str::to_owned),
            })
        }
    }

    /// From 0 (silence) to 1 (full voice).
    pub fn level(&self) -> f32 {
        let db = f32::from_bits(self.db.load(Ordering::Relaxed));
        ((db - QUIET_DB) / (LOUD_DB - QUIET_DB)).clamp(0., 1.)
    }

    /// Whether the microphone has delivered any sound, even room noise, in its latest buffer.
    pub fn has_signal(&self) -> bool {
        f32::from_bits(self.db.load(Ordering::Relaxed)) > SILENT_DB
    }

    /// The device asked for, as passed to `start`.
    pub fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }
}

impl Drop for MicLevel {
    fn drop(&mut self) {
        let job = OnQueue((self.session.clone(), self.delegate.take()));
        self.queue.exec_async(move || {
            let job = job;
            let (session, delegate) = job.0;
            unsafe { session.stopRunning() };
            drop(delegate);
        });
    }
}
