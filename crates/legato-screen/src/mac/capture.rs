//! Capturing a display with ScreenCaptureKit, as NV12 pixel buffers ready to encode.
//!
//! ScreenCaptureKit only delivers a frame when something on the display changed.

use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFArray, CGRect};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_core_graphics::{
    CGPreflightScreenCaptureAccess, CGRectMakeWithDictionaryRepresentation,
    CGRequestScreenCaptureAccess, kCGDisplayStreamYCbCrMatrix_ITU_R_709_2,
};
use objc2_core_media::{CMClock, CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSObject, NSObjectProtocol, NSValue};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamFrameInfoDirtyRects, SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A captured picture.
pub struct Frame {
    pub image: CFRetained<CVPixelBuffer>,
    /// When the picture appeared on the display.
    pub shown_at: Instant,
    /// How much of it changed since the last picture, from 0 to 1 (1 if unknown).
    pub changed: f64,
}

// SAFETY: pixel buffers are reference counted and safe to hand between threads.
unsafe impl Send for Frame {}

type Callback = Mutex<Box<dyn FnMut(Frame) + Send>>;

struct Ivars {
    on_frame: Callback,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and `Output` has no Drop.
    #[unsafe(super(NSObject))]
    #[name = "LegatoStreamOutput"]
    #[ivars = Ivars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // SAFETY: a valid sample buffer for the call's duration.
            let Some(info) = (unsafe { frame_info(sample) }) else {
                return;
            };
            if !is_complete(info) {
                return;
            }
            // SAFETY: as above.
            let Some(image) = (unsafe { sample.image_buffer() }) else {
                return;
            };
            let size = (
                CVPixelBufferGetWidth(&image),
                CVPixelBufferGetHeight(&image),
            );
            let changed = changed_share(info, size).unwrap_or(1.0);
            let shown_at = shown_at(sample);
            if let Ok(mut on_frame) = self.ivars().on_frame.lock() {
                on_frame(Frame {
                    image,
                    shown_at,
                    changed,
                });
            }
        }
    }
);

impl Output {
    fn new(on_frame: Callback) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { on_frame });
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

/// When a captured picture appeared on the display: its timestamp is on the host clock.
fn shown_at(sample: &CMSampleBuffer) -> Instant {
    let now = Instant::now();
    // SAFETY: reading a valid sample buffer's timestamp and the host clock.
    let age = unsafe {
        let shown = sample.presentation_time_stamp().seconds();
        let host = CMClock::host_time_clock().time().seconds();
        host - shown
    };
    if age.is_finite() && (0.0..1.0).contains(&age) {
        now.checked_sub(Duration::from_secs_f64(age)).unwrap_or(now)
    } else {
        now
    }
}

/// What ScreenCaptureKit says about a captured frame. Lives as long as `sample`.
///
/// # Safety
///
/// `sample` must be a valid sample buffer from ScreenCaptureKit.
unsafe fn frame_info(sample: &CMSampleBuffer) -> Option<&CFDictionary> {
    // SAFETY: the first attachment dictionary of a valid sample buffer.
    unsafe {
        let attachments = sample.sample_attachments_array(false)?;
        if attachments.count() == 0 {
            return None;
        }
        (attachments.value_at_index(0) as *const CFDictionary).as_ref()
    }
}

/// Whether ScreenCaptureKit marked the frame as a new picture (not "nothing changed").
fn is_complete(info: &CFDictionary) -> bool {
    // SAFETY: a lookup in a valid dictionary, whose status value is a number.
    unsafe {
        let key: *const CFString = &**SCStreamFrameInfoStatus as *const _ as *const CFString;
        let Some(status) = (info.value(key.cast()) as *const CFNumber).as_ref() else {
            return false;
        };
        status.as_i64() == Some(SCFrameStatus::Complete.0 as i64)
    }
}

/// How much of a `size` picture its dirty rectangles (redrawn or moved areas, in pixels)
/// cover, from 0 to 1. `None` if ScreenCaptureKit didn't say.
fn changed_share(info: &CFDictionary, size: (usize, usize)) -> Option<f64> {
    let area = size.0 as f64 * size.1 as f64;
    if area <= 0.0 {
        return None;
    }
    // SAFETY: a lookup in a valid dictionary; each element is checked before use.
    let rects: Vec<CGRect> = unsafe {
        let key: *const CFString = &**SCStreamFrameInfoDirtyRects as *const _ as *const CFString;
        let rects = (info.value(key.cast()) as *const CFArray).as_ref()?;
        (0..rects.count())
            .filter_map(|i| {
                let item = (rects.value_at_index(i) as *const AnyObject).as_ref()?;
                // Documented as NSValues; CGRect dictionaries in practice.
                if let Some(value) = item.downcast_ref::<NSValue>() {
                    return Some(value.rectValue());
                }
                item.downcast_ref::<NSDictionary>()?;
                let mut rect = CGRect::default();
                let dict = item as *const AnyObject as *const CFDictionary;
                CGRectMakeWithDictionaryRepresentation(dict.as_ref(), &mut rect).then_some(rect)
            })
            .collect()
    };
    Some(covered(&rects, size) / area)
}

/// The area `rects` cover within `size`, counting overlaps once.
fn covered(rects: &[CGRect], size: (usize, usize)) -> f64 {
    let (w, h) = (size.0 as f64, size.1 as f64);
    let clip = |r: &CGRect| {
        let (x0, y0) = (r.origin.x.max(0.0), r.origin.y.max(0.0));
        let (x1, y1) = (
            (r.origin.x + r.size.width).min(w),
            (r.origin.y + r.size.height).min(h),
        );
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
    };
    let rects: Vec<_> = rects.iter().filter_map(clip).collect();
    // Sweep across x: in each band between rectangle edges, add up the covered height.
    let mut xs: Vec<f64> = rects.iter().flat_map(|r| [r.0, r.2]).collect();
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    let mut total = 0.0;
    for band in xs.windows(2) {
        let (left, right) = (band[0], band[1]);
        let mut spans: Vec<(f64, f64)> = rects
            .iter()
            .filter(|r| r.0 <= left && r.2 >= right)
            .map(|r| (r.1, r.3))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut height, mut reach) = (0.0, f64::MIN);
        for (top, bottom) in spans {
            if bottom > reach {
                height += bottom - top.max(reach);
                reach = bottom;
            }
        }
        total += height * (right - left);
    }
    total
}

/// Whether this process may record the screen. Asks (once) if it hasn't been decided.
pub fn screen_recording_allowed() -> bool {
    CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess()
}

pub struct ScreenCapture {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
}

// SAFETY: SCStream may be stopped from any thread, and `&ScreenCapture` offers nothing.
unsafe impl Send for ScreenCapture {}
unsafe impl Sync for ScreenCapture {}

struct SendDisplay(Retained<SCDisplay>);
// SAFETY: SCDisplay is an immutable description.
unsafe impl Send for SendDisplay {}

/// Finds display `id` among what ScreenCaptureKit can capture. A display that was just
/// added can take a moment to be listed, so this retries briefly.
fn find_display(id: u32) -> Result<Retained<SCDisplay>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match find_display_once(id)? {
            Some(display) => return Ok(display),
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            None => bail!("the display isn't available to capture"),
        }
    }
}

fn find_display_once(id: u32) -> Result<Option<Retained<SCDisplay>>> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: ScreenCaptureKit passes valid (or null) objects for the call.
            let result = unsafe {
                match content.as_ref() {
                    Some(content) => Ok(content
                        .displays()
                        .iter()
                        .find(|d| d.displayID() == id)
                        .map(SendDisplay)),
                    None => Err(error
                        .as_ref()
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "no shareable content".into())),
                }
            };
            let _ = tx.send(result);
        },
    );
    // SAFETY: the handler is copied by the callee.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    match rx.recv_timeout(TIMEOUT) {
        Ok(Ok(display)) => Ok(display.map(|d| d.0)),
        Ok(Err(e)) => bail!("can't capture the screen: {e}"),
        Err(_) => bail!("timed out finding the display to capture"),
    }
}

fn wait(what: &str, start: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>)) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let handler = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: a valid (or null) error for the call.
        let error = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send(error);
    });
    start(&handler);
    match rx.recv_timeout(TIMEOUT) {
        Ok(None) => Ok(()),
        Ok(Some(e)) => bail!("{what} failed: {e}"),
        Err(_) => bail!("{what} timed out"),
    }
}

/// What to capture: NV12 at `width`×`height`, up to `fps` frames a second, no cursor.
fn configuration(width: u32, height: u32, fps: u32) -> Retained<SCStreamConfiguration> {
    // SAFETY: plain setters on a fresh configuration.
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setWidth(width as usize);
        config.setHeight(height as usize);
        config.setMinimumFrameInterval(CMTime::new(1, fps as i32));
        config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        config.setColorMatrix(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2);
        config.setShowsCursor(false);
        config.setQueueDepth(5);
        config
    }
}

impl ScreenCapture {
    /// Changes the captured size and frame rate while running.
    pub fn reconfigure(&self, width: u32, height: u32, fps: u32) -> Result<()> {
        let config = configuration(width, height, fps);
        let stream = &self.stream;
        // SAFETY: updating a started stream with a valid configuration.
        wait("changing the capture size", |h| unsafe {
            stream.updateConfiguration_completionHandler(&config, Some(h))
        })
    }

    /// Captures display `id` at `width`×`height` pixels, up to `fps` frames a second,
    /// without the cursor. `on_frame` runs on a capture thread.
    pub fn start(
        id: u32,
        width: u32,
        height: u32,
        fps: u32,
        on_frame: impl FnMut(Frame) + Send + 'static,
    ) -> Result<Self> {
        if !screen_recording_allowed() {
            bail!(
                "Legato needs Screen Recording permission to show this Mac's extra display. \
                 Allow it in System Settings → Privacy & Security → Screen & System Audio \
                 Recording, then try again."
            );
        }
        let display = find_display(id)?;
        let queue = DispatchQueue::new("io.legato.capture", None);
        // SAFETY: ScreenCaptureKit setup with objects that outlive the calls.
        unsafe {
            let filter = SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            );
            let config = configuration(width, height, fps);
            let stream = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            );
            let output = Output::new(Mutex::new(Box::new(on_frame)));
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))
                .context("can't capture the screen")?;
            wait("starting screen capture", |h| {
                stream.startCaptureWithCompletionHandler(Some(h))
            })?;
            Ok(Self {
                stream,
                _output: output,
                _queue: queue,
            })
        }
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        let stream = &self.stream;
        // SAFETY: stopping a started stream.
        let result = wait("stopping screen capture", |h| unsafe {
            stream.stopCaptureWithCompletionHandler(Some(h))
        });
        if let Err(e) = result {
            tracing::debug!("{e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn covered_area_counts_overlaps_once_and_stays_on_the_picture() {
        let size = (100, 100);
        assert_eq!(covered(&[], size), 0.0);
        assert_eq!(covered(&[rect(10.0, 10.0, 20.0, 10.0)], size), 200.0);
        // Two overlapping squares: 2 × 400 - 100.
        let overlapping = [rect(0.0, 0.0, 20.0, 20.0), rect(10.0, 10.0, 20.0, 20.0)];
        assert_eq!(covered(&overlapping, size), 700.0);
        // One inside another.
        let nested = [rect(0.0, 0.0, 50.0, 50.0), rect(10.0, 10.0, 5.0, 5.0)];
        assert_eq!(covered(&nested, size), 2500.0);
        // Hanging off the edge, or off the picture entirely.
        assert_eq!(covered(&[rect(90.0, 90.0, 20.0, 20.0)], size), 100.0);
        assert_eq!(covered(&[rect(200.0, 0.0, 20.0, 20.0)], size), 0.0);
        assert_eq!(covered(&[rect(-50.0, -50.0, 500.0, 500.0)], size), 10_000.0);
    }
}
