//! The pop-out camera bubble. Each camera frame is split into you and your background with
//! Vision's person segmentation, then Core Image composites it on the GPU into a frame with
//! transparency: the background inside the circle, the ring, and you on top, free to rise
//! above the circle but never out of its sides. Runs on the camera's capture queue.

use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::AnyThread as _;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFDictionary, CFRetained, CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColorSpace, kCGColorSpaceSRGB};
use objc2_core_image::{CIColor, CIContext, CIFilter, CIImage, CIVector};
use objc2_core_video::{CVPixelBuffer, CVPixelBufferPool, kCVReturnSuccess};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
use objc2_vision::{
    VNGeneratePersonSegmentationRequest, VNGeneratePersonSegmentationRequestQualityLevel,
    VNImageRequestHandler, VNRequest,
};

/// Room above the circle for your head, as a fraction of the bubble's diameter.
pub const HEADROOM: f64 = 0.35;
/// Frames are drawn at Retina resolution.
pub const SCALE: f64 = 2.;
/// The camera is zoomed this much past filling the circle, so the top of your head sits above
/// the circle's rim (webcams frame the head close to the top).
const ZOOM: f64 = 1.3;
const RING_POINTS: f64 = 2.5;
/// Softens the person mask's edge, in points.
const FEATHER_POINTS: f64 = 0.6;

const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
const ONE_COMPONENT_8: u32 = u32::from_be_bytes(*b"L008");

/// A composited frame, handed from the capture queue to the bubble on the main thread.
struct Frame(CFRetained<CVPixelBuffer>);
// SAFETY: CVPixelBuffers are reference counted and thread safe; the capture queue writes a
// frame once and the bubble only reads it.
unsafe impl Send for Frame {}

/// The latest composited frame.
#[derive(Default)]
pub struct PopoutSlot {
    frame: Mutex<Option<Frame>>,
    seq: AtomicU64,
}

impl PopoutSlot {
    pub fn latest(&self) -> Option<CFRetained<CVPixelBuffer>> {
        self.frame.lock().ok()?.as_ref().map(|f| f.0.clone())
    }

    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    pub fn clear(&self) {
        if let Ok(mut frame) = self.frame.lock() {
            *frame = None;
        }
    }

    fn store(&self, buffer: CFRetained<CVPixelBuffer>) {
        if let Ok(mut frame) = self.frame.lock() {
            *frame = Some(Frame(buffer));
        }
        self.seq.fetch_add(1, Ordering::AcqRel);
    }
}

pub struct Compositor {
    request: Retained<VNGeneratePersonSegmentationRequest>,
    context: Retained<CIContext>,
    pool: CFRetained<CVPixelBufferPool>,
    /// Output size in pixels; the circle sits at the bottom, the headroom above it.
    size: CGSize,
    diameter: f64,
    circle: Retained<CIImage>,
    ring: Retained<CIImage>,
    /// Where the person may appear: the circle, and straight up from it.
    allowed: Retained<CIImage>,
    clear: Retained<CIImage>,
    /// Core Image works in linear light; frames are written out as sRGB.
    srgb: CFRetained<CGColorSpace>,
}

// SAFETY: the compositor lives behind a mutex and is only used from the camera's serial
// capture queue.
unsafe impl Send for Compositor {}

fn object<T: objc2::Message>(value: &T) -> Retained<AnyObject> {
    // SAFETY: every Objective-C object is an `AnyObject`.
    unsafe { Retained::cast_unchecked(value.retain()) }
}

/// Runs a Core Image filter by name and returns its output.
fn filter(name: &str, params: &[(&str, Retained<AnyObject>)]) -> Option<Retained<CIImage>> {
    let keys: Vec<Retained<NSString>> = params.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<Retained<AnyObject>> = params.iter().map(|(_, v)| v.clone()).collect();
    let dict = NSDictionary::<NSString, AnyObject>::from_retained_objects(&key_refs, &values);
    // SAFETY: filter names and keys are Core Image's documented ones; values match their types.
    unsafe {
        CIFilter::filterWithName_withInputParameters(&NSString::from_str(name), Some(&dict))?
            .outputImage()
    }
}

fn number(value: f64) -> Retained<AnyObject> {
    object(&*NSNumber::new_f64(value))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

/// White inside a circle of `radius`, black outside, with a one-pixel antialiased edge.
fn disk(
    center: (f64, f64),
    radius: f64,
    inverted: bool,
    canvas: CGRect,
) -> Option<Retained<CIImage>> {
    // SAFETY: plain value constructors.
    let (inside, outside) = unsafe { (CIColor::whiteColor(), CIColor::blackColor()) };
    let (c0, c1) = if inverted {
        (outside, inside)
    } else {
        (inside, outside)
    };
    let image = filter(
        "CIRadialGradient",
        &[
            (
                "inputCenter",
                object(&*unsafe { CIVector::vectorWithX_Y(center.0, center.1) }),
            ),
            ("inputRadius0", number(radius - 1.)),
            ("inputRadius1", number(radius)),
            ("inputColor0", object(&*c0)),
            ("inputColor1", object(&*c1)),
        ],
    )?;
    // SAFETY: cropping an image we made.
    Some(unsafe { image.imageByCroppingToRect(canvas) })
}

fn solid(color: &CIColor, area: CGRect) -> Option<Retained<CIImage>> {
    let image = filter("CIConstantColorGenerator", &[("inputColor", object(color))])?;
    // SAFETY: cropping an image we made.
    Some(unsafe { image.imageByCroppingToRect(area) })
}

fn masked(image: &CIImage, mask: &CIImage, background: &CIImage) -> Option<Retained<CIImage>> {
    filter(
        "CIBlendWithMask",
        &[
            ("inputImage", object(image)),
            ("inputBackgroundImage", object(background)),
            ("inputMaskImage", object(mask)),
        ],
    )
}

fn multiply(a: &CIImage, b: &CIImage) -> Option<Retained<CIImage>> {
    filter(
        "CIMultiplyCompositing",
        &[
            ("inputImage", object(a)),
            ("inputBackgroundImage", object(b)),
        ],
    )
}

impl Compositor {
    /// A compositor for a bubble `diameter` points across, or `None` if Core Image or Vision
    /// can't be set up (the bubble then falls back to the plain circle).
    pub fn new(diameter: f64) -> Option<Self> {
        let d = (diameter * SCALE).round();
        let size = CGSize::new(d, (d * (1. + HEADROOM)).round());
        let canvas = rect(0., 0., size.width, size.height);
        let r = d / 2.;
        // Core Image's y runs upward, so the circle at the bottom is centered at (r, r).
        let center = (r, r);
        let circle = disk(center, r, false, canvas)?;
        let inner = disk(center, r - RING_POINTS * SCALE, true, canvas)?;
        let band = multiply(&circle, &inner)?;
        // SAFETY: plain value constructors and compositing images we made.
        unsafe {
            let white = CIColor::colorWithRed_green_blue_alpha(1., 1., 1., 0.92);
            let clear = solid(&CIColor::clearColor(), canvas)?;
            let paint = solid(&white, canvas)?;
            let ring = masked(&paint, &band, &clear)?;
            // The middle of the circle's width, straight up: room for a head, but not for a hand
            // raised beside it. Soft edges, so hair near the sides fades rather than being cut.
            let above = solid(
                &CIColor::whiteColor(),
                rect(d * 0.1, r, d * 0.8, size.height - r),
            )?;
            // Only the band is softened: the circle keeps its crisp edge below.
            let allowed = above
                .imageByApplyingGaussianBlurWithSigma(d * 0.03)
                .imageByCroppingToRect(canvas)
                .imageByCompositingOverImage(&circle);

            let request = VNGeneratePersonSegmentationRequest::init(
                VNGeneratePersonSegmentationRequest::alloc(),
            );
            request.setQualityLevel(VNGeneratePersonSegmentationRequestQualityLevel::Balanced);
            request.setOutputPixelFormat(ONE_COMPONENT_8);

            let keys = [
                NSString::from_str("PixelFormatType"),
                NSString::from_str("Width"),
                NSString::from_str("Height"),
                NSString::from_str("IOSurfaceProperties"),
            ];
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let attributes = NSDictionary::<NSString, AnyObject>::from_retained_objects(
                &key_refs,
                &[
                    object(&*NSNumber::new_u32(BGRA)),
                    number(size.width),
                    number(size.height),
                    object(&*NSDictionary::<NSString, AnyObject>::new()),
                ],
            );
            let attributes: &CFDictionary =
                &*(Retained::as_ptr(&attributes) as *const CFDictionary);
            let mut pool: *mut CVPixelBufferPool = std::ptr::null_mut();
            if CVPixelBufferPool::create(None, None, Some(attributes), NonNull::from(&mut pool))
                != kCVReturnSuccess
            {
                return None;
            }
            let pool = CFRetained::from_raw(NonNull::new(pool)?);

            Some(Self {
                request,
                context: CIContext::contextWithOptions(None),
                pool,
                size,
                diameter: d,
                circle,
                ring,
                allowed,
                clear,
                srgb: CGColorSpace::with_name(Some(kCGColorSpaceSRGB))?,
            })
        }
    }

    /// Composites one camera frame; the result appears in `slot`.
    pub fn process(&mut self, frame: &CVPixelBuffer, slot: &PopoutSlot) {
        if let Some(output) = self.composite(frame) {
            slot.store(output);
        }
    }

    fn composite(&mut self, frame: &CVPixelBuffer) -> Option<CFRetained<CVPixelBuffer>> {
        // SAFETY: Vision and Core Image calls on objects this compositor owns, on its queue.
        unsafe {
            let handler = VNImageRequestHandler::initWithCVPixelBuffer_options(
                VNImageRequestHandler::alloc(),
                frame,
                &NSDictionary::new(),
            );
            let request: &VNRequest = &self.request;
            handler
                .performRequests_error(&NSArray::from_slice(&[request]))
                .ok()?;
            let mask_buffer = self.request.results()?.firstObject()?.pixelBuffer();

            let camera = CIImage::imageWithCVPixelBuffer(frame);
            let fw = camera.extent().size.width;
            let fh = camera.extent().size.height;
            if fw <= 0. || fh <= 0. {
                return None;
            }
            // Zoom past filling the circle and anchor near its bottom, so the head rises above.
            let d = self.diameter;
            let s = (d / fw).max(d / fh) * ZOOM;
            let place = CGAffineTransform {
                a: s,
                b: 0.,
                c: 0.,
                d: s,
                tx: (d - fw * s) / 2.,
                ty: -0.04 * d,
            };
            let camera = camera.imageByApplyingTransform(place);

            let mask = CIImage::imageWithCVPixelBuffer(&mask_buffer);
            let to_frame = CGAffineTransform {
                a: fw / mask.extent().size.width,
                b: 0.,
                c: 0.,
                d: fh / mask.extent().size.height,
                tx: 0.,
                ty: 0.,
            };
            let canvas = rect(0., 0., self.size.width, self.size.height);
            let mask = mask
                .imageByApplyingTransform(to_frame)
                .imageByApplyingTransform(place)
                .imageByClampingToExtent()
                .imageByApplyingGaussianBlurWithSigma(FEATHER_POINTS * SCALE)
                .imageByCroppingToRect(canvas);
            let mask = multiply(&mask, &self.allowed)?;

            let background = masked(&camera, &self.circle, &self.clear)?;
            let person = masked(&camera, &mask, &self.clear)?;
            let image = person
                .imageByCompositingOverImage(&self.ring.imageByCompositingOverImage(&background));

            let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
            if CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut out))
                != kCVReturnSuccess
            {
                return None;
            }
            let out = CFRetained::from_raw(NonNull::new(out)?);
            self.context.render_toCVPixelBuffer_bounds_colorSpace(
                &image,
                &out,
                canvas,
                Some(&self.srgb),
            );
            Some(out)
        }
    }
}
