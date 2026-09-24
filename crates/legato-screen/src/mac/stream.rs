//! A virtual display, captured and encoded: everything the Mac runs while its extra
//! display is shown on another machine.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use objc2_core_foundation::CFRetained;
use objc2_core_video::CVPixelBuffer;

use super::{EncodedFrame, Encoder, EncoderConfig, ScreenCapture, VirtualDisplay};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// Display size in pixels.
    pub width: u32,
    pub height: u32,
    pub hidpi: bool,
    pub fps: u32,
    pub bitrate: u32,
}

struct Shared {
    /// Locked around each `encode` so timestamps reach the encoder in order.
    encoder: Mutex<Encoder>,
    start: Instant,
    /// The latest picture, re-encoded when a keyframe is wanted and nothing has changed.
    last: Mutex<Option<LastImage>>,
    keyframe: AtomicBool,
    /// Set while the network can't keep up: new pictures are skipped, not queued.
    backlogged: AtomicBool,
}

struct LastImage(CFRetained<CVPixelBuffer>);
// SAFETY: pixel buffers are reference counted and safe to hand between threads.
unsafe impl Send for LastImage {}

impl Shared {
    fn encode(&self, image: &CVPixelBuffer) {
        let encoder = self.encoder.lock().unwrap();
        let keyframe = self.keyframe.swap(false, Ordering::Relaxed);
        if let Err(e) = encoder.encode_now(image, self.start.elapsed(), keyframe) {
            tracing::debug!("{e:#}");
        }
    }
}

/// Shareable between threads: `request_keyframe` and `set_backlogged` only touch
/// thread-safe state.
pub struct DisplayStream {
    // Field order is drop order: stop capturing, then encoding, then remove the display.
    _capture: ScreenCapture,
    shared: Arc<Shared>,
    display: VirtualDisplay,
}

impl DisplayStream {
    /// Adds a display to this Mac and starts streaming it. `on_frame` gets each encoded
    /// frame, on an encoder thread.
    pub fn start(
        name: &str,
        config: StreamConfig,
        on_frame: impl FnMut(EncodedFrame) + Send + 'static,
    ) -> Result<Self> {
        let display = VirtualDisplay::create(
            name,
            super::Mode {
                width: config.width,
                height: config.height,
                hidpi: config.hidpi,
                refresh: config.fps as f64,
            },
        )?;
        let mut on_frame = on_frame;
        let encoder = Encoder::new(
            EncoderConfig {
                width: config.width,
                height: config.height,
                fps: config.fps,
                bitrate: config.bitrate,
            },
            move |frame| match frame {
                Ok(frame) => on_frame(frame),
                Err(e) => tracing::debug!("{e:#}"),
            },
        )?;
        let shared = Arc::new(Shared {
            encoder: Mutex::new(encoder),
            start: Instant::now(),
            last: Mutex::new(None),
            keyframe: AtomicBool::new(true),
            backlogged: AtomicBool::new(false),
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
                        shared.encode(&frame.image);
                    }
                },
            )?
        };
        Ok(Self {
            _capture: capture,
            shared,
            display,
        })
    }

    pub fn display(&self) -> &VirtualDisplay {
        &self.display
    }

    /// Sends a keyframe as soon as possible (a viewer joined or lost frames).
    pub fn request_keyframe(&self) {
        self.shared.keyframe.store(true, Ordering::Relaxed);
        // If the screen is still, no new picture is coming: re-send the last one.
        let last = self.shared.last.lock().unwrap();
        if let Some(LastImage(image)) = &*last {
            self.shared.encode(image);
        }
    }

    /// While the network is backlogged, new pictures are skipped rather than queued.
    pub fn set_backlogged(&self, backlogged: bool) {
        self.shared.backlogged.store(backlogged, Ordering::Relaxed);
    }
}
