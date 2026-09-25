//! A virtual display, captured and encoded: everything the Mac runs while its extra
//! display is shown on another machine.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use super::{EncodedFrame, Encoder, EncoderConfig, Mode, ScreenCapture, VirtualDisplay};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// Display size in pixels.
    pub width: u32,
    pub height: u32,
    pub hidpi: bool,
    pub fps: u32,
    pub bitrate: u32,
}

impl StreamConfig {
    fn mode(&self) -> Mode {
        Mode {
            width: self.width,
            height: self.height,
            hidpi: self.hidpi,
            refresh: self.fps as f64,
        }
    }
}

/// Where encoded frames go, whichever encoder made them.
type Sink = Arc<Mutex<Box<dyn FnMut(EncodedFrame) + Send>>>;

/// The encoder and the picture size it was made for.
struct Encoding {
    encoder: Encoder,
    size: (usize, usize),
}

struct Shared {
    /// Locked around each `encode` so timestamps reach the encoder in order, and while
    /// the encoder is replaced for a new size.
    encoding: Mutex<Encoding>,
    start: Instant,
    /// The latest picture, re-encoded when a keyframe is wanted and nothing has changed.
    last: Mutex<Option<LastImage>>,
    keyframe: AtomicBool,
    /// Set while the network can't keep up: new pictures are skipped, not queued.
    backlogged: AtomicBool,
    /// When the picture being encoded appeared on the display.
    shown_at: Arc<Mutex<Option<Instant>>>,
}

struct LastImage(CFRetained<CVPixelBuffer>);
// SAFETY: pixel buffers are reference counted and safe to hand between threads.
unsafe impl Send for LastImage {}

impl Shared {
    fn encode(&self, image: &CVPixelBuffer, shown_at: Instant) {
        let encoding = self.encoding.lock().unwrap();
        // While the size changes, capture can still deliver a few old-size pictures.
        let size = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
        if size != encoding.size {
            return;
        }
        // Read by the encoder's output callback, which runs before `encode_now` returns.
        *self.shown_at.lock().unwrap() = Some(shown_at);
        let keyframe = self.keyframe.swap(false, Ordering::Relaxed);
        if let Err(e) = encoding
            .encoder
            .encode_now(image, self.start.elapsed(), keyframe)
        {
            tracing::debug!("{e:#}");
        }
    }
}

fn encoder(
    config: StreamConfig,
    sink: &Sink,
    shown_at: &Arc<Mutex<Option<Instant>>>,
) -> Result<Encoding> {
    let (sink, shown_at) = (sink.clone(), shown_at.clone());
    let encoder = Encoder::new(
        EncoderConfig {
            width: config.width,
            height: config.height,
            fps: config.fps,
            bitrate: config.bitrate,
        },
        move |frame| match frame {
            Ok(mut frame) => {
                frame.shown_at = *shown_at.lock().unwrap();
                (sink.lock().unwrap())(frame);
            }
            Err(e) => tracing::debug!("{e:#}"),
        },
    )?;
    Ok(Encoding {
        encoder,
        size: (config.width as usize, config.height as usize),
    })
}

/// Shareable between threads: everything it offers only touches thread-safe state.
pub struct DisplayStream {
    // Field order is drop order: stop capturing, then encoding, then remove the display.
    capture: ScreenCapture,
    shared: Arc<Shared>,
    display: VirtualDisplay,
    sink: Sink,
    config: Mutex<StreamConfig>,
}

impl DisplayStream {
    /// Adds a display to this Mac and starts streaming it. `on_frame` gets each encoded
    /// frame, on an encoder thread.
    pub fn start(
        name: &str,
        config: StreamConfig,
        on_frame: impl FnMut(EncodedFrame) + Send + 'static,
    ) -> Result<Self> {
        let shown_at: Arc<Mutex<Option<Instant>>> = Arc::default();
        let sink: Sink = Arc::new(Mutex::new(Box::new(on_frame)));
        let display = VirtualDisplay::create(name, config.mode())?;
        let shared = Arc::new(Shared {
            encoding: Mutex::new(encoder(config, &sink, &shown_at)?),
            start: Instant::now(),
            last: Mutex::new(None),
            keyframe: AtomicBool::new(true),
            backlogged: AtomicBool::new(false),
            shown_at,
        });
        let capture = {
            let shared = shared.clone();
            ScreenCapture::start(
                display.id(),
                config.width,
                config.height,
                config.fps,
                move |frame| {
                    let mut last = shared.last.lock().unwrap();
                    *last = Some(LastImage(frame.image.clone()));
                    let wants_keyframe = shared.keyframe.load(Ordering::Relaxed);
                    if wants_keyframe || !shared.backlogged.load(Ordering::Relaxed) {
                        shared.encode(&frame.image, frame.shown_at);
                    }
                },
            )?
        };
        Ok(Self {
            capture,
            shared,
            display,
            sink,
            config: Mutex::new(config),
        })
    }

    pub fn display(&self) -> &VirtualDisplay {
        &self.display
    }

    pub fn config(&self) -> StreamConfig {
        *self.config.lock().unwrap()
    }

    /// Changes the display's size, Retina mode or frame rate while streaming. The next
    /// frame is a keyframe at the new size.
    pub fn reconfigure(&self, config: StreamConfig) -> Result<()> {
        let mut current = self.config.lock().unwrap();
        if *current == config {
            return Ok(());
        }
        self.display.set_mode(config.mode())?;
        {
            // Same lock order as the capture callback: the last picture, then the encoder.
            let mut last = self.shared.last.lock().unwrap();
            let mut encoding = self.shared.encoding.lock().unwrap();
            *encoding = encoder(config, &self.sink, &self.shared.shown_at)?;
            *last = None;
            self.shared.keyframe.store(true, Ordering::Relaxed);
        }
        self.capture
            .reconfigure(config.width, config.height, config.fps)?;
        *current = config;
        Ok(())
    }

    /// Sends a keyframe as soon as possible (a viewer joined or lost frames).
    pub fn request_keyframe(&self) {
        self.shared.keyframe.store(true, Ordering::Relaxed);
        // If the screen is still, no new picture is coming: re-send the last one.
        let last = self.shared.last.lock().unwrap();
        if let Some(LastImage(image)) = &*last {
            // Still on screen, so it's current as of now.
            self.shared.encode(image, Instant::now());
        }
    }

    /// While the network is backlogged, new pictures are skipped rather than queued.
    pub fn set_backlogged(&self, backlogged: bool) {
        self.shared.backlogged.store(backlogged, Ordering::Relaxed);
    }
}
