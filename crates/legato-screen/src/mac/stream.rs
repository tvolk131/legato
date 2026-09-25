//! A virtual display, captured and encoded: everything the Mac runs while its extra
//! display is shown on another machine.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    /// The size it's captured and encoded at: the display's own, or smaller to be faster.
    pub stream_width: u32,
    pub stream_height: u32,
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

    fn frame_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps.max(1) as f64)
    }
}

/// Where encoded frames go, whichever encoder made them.
type Sink = Arc<Mutex<Box<dyn FnMut(EncodedFrame) + Send>>>;

/// When each picture handed to the encoder appeared on the display, by timestamp: with
/// frames overlapping, the one coming out isn't the one just handed in.
type ShownAt = Arc<Mutex<std::collections::VecDeque<(Duration, Instant)>>>;

/// The encoder, the picture size it was made for, and how it's being fed.
struct Encoding {
    encoder: Encoder,
    size: (usize, usize),
    interval: Duration,
    /// Each frame is left in flight while the next is encoded (see
    /// [`Encoder::encode_overlapped`]), because frames take most of their time budget.
    overlapped: bool,
    /// Frames in a row that took most of their time budget, while not overlapped.
    slow: u32,
    /// The last frame sent to the encoder, and whether it may still be in flight.
    last_pts: Option<Duration>,
    in_flight: bool,
    last_encode: Instant,
    /// The encoder's dropped-frame count already reported.
    dropped_seen: u32,
}

/// Overlap frames when they're expected to take more than this share of their budget
/// (about 2 ns per pixel on Apple silicon: 4K at 60 fps, 1440p at 120).
const OVERLAP_ABOVE: f64 = 0.6;
/// Or when this many in a row actually took more than 90% of it.
const SLOW_FRAMES: u32 = 3;

impl Encoding {
    fn new(config: StreamConfig, sink: &Sink, shown_at: &ShownAt) -> Result<Self> {
        let (sink, shown_at) = (sink.clone(), shown_at.clone());
        let encoder = Encoder::new(
            EncoderConfig {
                width: config.stream_width,
                height: config.stream_height,
                fps: config.fps,
                bitrate: config.bitrate,
            },
            move |frame| match frame {
                Ok(mut frame) => {
                    let mut shown = shown_at.lock().unwrap();
                    while let Some(&(pts, at)) = shown.front() {
                        if pts > frame.pts {
                            break;
                        }
                        shown.pop_front();
                        if pts == frame.pts {
                            frame.shown_at = Some(at);
                        }
                    }
                    drop(shown);
                    (sink.lock().unwrap())(frame);
                }
                Err(e) => tracing::debug!("{e:#}"),
            },
        )?;
        let interval = config.frame_interval();
        let megapixels = config.stream_width as f64 * config.stream_height as f64 / 1e6;
        let expected = Duration::from_secs_f64(megapixels * 1.95e-3);
        Ok(Self {
            encoder,
            size: (config.stream_width as usize, config.stream_height as usize),
            interval,
            overlapped: expected.as_secs_f64() > interval.as_secs_f64() * OVERLAP_ABOVE,
            slow: 0,
            last_pts: None,
            in_flight: false,
            last_encode: Instant::now(),
            dropped_seen: 0,
        })
    }

    fn encode(&mut self, image: &CVPixelBuffer, pts: Duration, keyframe: bool) -> Result<()> {
        let started = Instant::now();
        if self.overlapped {
            self.encoder
                .encode_overlapped(image, pts, keyframe, self.last_pts)?;
            self.in_flight = true;
        } else {
            self.encoder.encode_now(image, pts, keyframe)?;
            // Falling behind at this size: overlap from now on.
            if started.elapsed() > self.interval.mul_f64(0.9) {
                self.slow += 1;
                if self.slow >= SLOW_FRAMES {
                    tracing::info!("encoding takes most of each frame's time: overlapping frames");
                    self.overlapped = true;
                }
            } else {
                self.slow = 0;
            }
        }
        self.last_pts = Some(pts);
        self.last_encode = Instant::now();
        Ok(())
    }

    /// Sends the frame left in flight if no other has followed it for a while (the
    /// screen went still), so the viewer isn't left a frame behind.
    fn flush_if_idle(&mut self) {
        if self.in_flight && self.last_encode.elapsed() > self.interval * 2 {
            self.encoder.flush();
            self.in_flight = false;
        }
    }

    /// Pictures the encoder dropped since the last call.
    fn newly_dropped(&mut self) -> u32 {
        let dropped = self.encoder.dropped();
        let new = dropped.saturating_sub(self.dropped_seen);
        self.dropped_seen = dropped;
        new
    }
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
    /// Pictures skipped (or dropped by the encoder) since the viewer last heard.
    missed: AtomicU32,
    shown_at: ShownAt,
    stop: AtomicBool,
}

struct LastImage(CFRetained<CVPixelBuffer>);
// SAFETY: pixel buffers are reference counted and safe to hand between threads.
unsafe impl Send for LastImage {}

impl Shared {
    fn encode(&self, image: &CVPixelBuffer, shown_at: Instant) {
        let mut encoding = self.encoding.lock().unwrap();
        // While the size changes, capture can still deliver a few old-size pictures.
        let size = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
        if size != encoding.size {
            return;
        }
        // Whole microseconds, as the encoder hands timestamps back.
        let pts = Duration::from_micros(self.start.elapsed().as_micros() as u64);
        self.shown_at.lock().unwrap().push_back((pts, shown_at));
        let keyframe = self.keyframe.swap(false, Ordering::Relaxed);
        if let Err(e) = encoding.encode(image, pts, keyframe) {
            tracing::debug!("{e:#}");
        }
        let dropped = encoding.newly_dropped();
        if dropped > 0 {
            self.missed.fetch_add(dropped, Ordering::Relaxed);
        }
    }
}

/// Shareable between threads: everything it offers only touches thread-safe state.
pub struct DisplayStream {
    // Field order is drop order: stop capturing, then encoding, then remove the display.
    capture: ScreenCapture,
    flusher: Option<std::thread::JoinHandle<()>>,
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
        let shown_at: ShownAt = Arc::default();
        let sink: Sink = Arc::new(Mutex::new(Box::new(on_frame)));
        let display = VirtualDisplay::create(name, config.mode())?;
        let shared = Arc::new(Shared {
            encoding: Mutex::new(Encoding::new(config, &sink, &shown_at)?),
            start: Instant::now(),
            last: Mutex::new(None),
            keyframe: AtomicBool::new(true),
            backlogged: AtomicBool::new(false),
            missed: AtomicU32::new(0),
            shown_at,
            stop: AtomicBool::new(false),
        });
        let capture = {
            let shared = shared.clone();
            ScreenCapture::start(
                display.id(),
                config.stream_width,
                config.stream_height,
                config.fps,
                move |frame| {
                    let mut last = shared.last.lock().unwrap();
                    *last = Some(LastImage(frame.image.clone()));
                    let wants_keyframe = shared.keyframe.load(Ordering::Relaxed);
                    if wants_keyframe || !shared.backlogged.load(Ordering::Relaxed) {
                        shared.encode(&frame.image, frame.shown_at);
                    } else {
                        shared.missed.fetch_add(1, Ordering::Relaxed);
                    }
                },
            )?
        };
        let flusher = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("legato-encode-flush".into())
                .spawn(move || {
                    while !shared.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(10));
                        shared.encoding.lock().unwrap().flush_if_idle();
                    }
                })?
        };
        Ok(Self {
            capture,
            flusher: Some(flusher),
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

    /// Changes the display's size, Retina mode, stream size or frame rate while
    /// streaming. The next frame is a keyframe at the new size.
    pub fn reconfigure(&self, config: StreamConfig) -> Result<()> {
        let mut current = self.config.lock().unwrap();
        if *current == config {
            return Ok(());
        }
        if config.mode() != current.mode() {
            self.display.set_mode(config.mode())?;
        }
        {
            // Same lock order as the capture callback: the last picture, then the encoder.
            let mut last = self.shared.last.lock().unwrap();
            let mut encoding = self.shared.encoding.lock().unwrap();
            *encoding = Encoding::new(config, &self.sink, &self.shared.shown_at)?;
            *last = None;
            self.shared.keyframe.store(true, Ordering::Relaxed);
        }
        self.capture
            .reconfigure(config.stream_width, config.stream_height, config.fps)?;
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

    /// Pictures skipped or dropped since the last call, for the viewer's stats.
    pub fn take_missed(&self) -> u32 {
        self.shared.missed.swap(0, Ordering::Relaxed)
    }
}

impl Drop for DisplayStream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(flusher) = self.flusher.take() {
            let _ = flusher.join();
        }
    }
}
