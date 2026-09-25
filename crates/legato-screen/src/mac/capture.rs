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
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_core_graphics::{
    CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
    kCGDisplayStreamYCbCrMatrix_ITU_R_709_2,
};
use objc2_core_media::{CMClock, CMSampleBuffer, CMTime};
use objc2_core_video::{CVPixelBuffer, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A captured picture.
pub struct Frame {
    pub image: CFRetained<CVPixelBuffer>,
    /// When the picture appeared on the display.
    pub shown_at: Instant,
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
            if kind != SCStreamOutputType::Screen || !is_complete(sample) {
                return;
            }
            // SAFETY: a valid sample buffer for the call's duration.
            let Some(image) = (unsafe { sample.image_buffer() }) else {
                return;
            };
            let shown_at = shown_at(sample);
            if let Ok(mut on_frame) = self.ivars().on_frame.lock() {
                on_frame(Frame { image, shown_at });
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

/// Whether ScreenCaptureKit marked the frame as a new picture (not "nothing changed").
fn is_complete(sample: &CMSampleBuffer) -> bool {
    // SAFETY: reading the first attachment dictionary of a valid sample buffer.
    unsafe {
        let Some(attachments) = sample.sample_attachments_array(false) else {
            return false;
        };
        if attachments.count() == 0 {
            return false;
        }
        let Some(dict) = (attachments.value_at_index(0) as *const CFDictionary).as_ref() else {
            return false;
        };
        let key: *const CFString = &**SCStreamFrameInfoStatus as *const _ as *const CFString;
        let Some(status) = (dict.value(key.cast()) as *const CFNumber).as_ref() else {
            return false;
        };
        status.as_i64() == Some(SCFrameStatus::Complete.0 as i64)
    }
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
