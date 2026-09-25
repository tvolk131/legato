//! A virtual display, captured and encoded: everything the Mac runs while its extra
//! display is shown on another machine.
//!
//! Pictures go on one or two tracks (see [`crate::adaptive`]): the stream size, and with
//! adaptive quality a smaller size while much of the screen moves, scaled on the GPU.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use super::scale::Scaler;
use super::{EncodedFrame, Encoder, EncoderConfig, Mode, ScreenCapture, VirtualDisplay};
use crate::adaptive::{Decision, MOVING, Policy, SHARP};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// Display size in pixels.
    pub width: u32,
    pub height: u32,
    pub hidpi: bool,
    /// The size it's captured and encoded at: the display's own, or smaller to be faster.
    pub stream_width: u32,
    pub stream_height: u32,
    /// Adaptive quality: the smaller size sent while much of the screen moves.
    pub moving: Option<(u32, u32)>,
    /// The display's refresh rate, and the most frames sent a second.
    pub fps: u32,
    /// With `moving`: the most frames a second at the stream size (it's slower to
    /// encode than the display runs).
    pub sharp_fps: u32,
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

/// Overlap frames when they're expected to take more than this share of their budget
/// (about 2 ns per pixel on Apple silicon: 4K at 60 fps, 1440p at 120).
const OVERLAP_ABOVE: f64 = 0.6;
/// Or when this many in a row actually took more than 90% of it.
const SLOW_FRAMES: u32 = 3;
/// Frames following the last within this many intervals count as a continuous run, worth
/// overlapping; a lone picture (a keystroke) is sent straight away.
const CONTINUOUS: f64 = 1.5;
/// A frame left in flight is sent after this many intervals with nothing following it.
const FLUSH_AFTER: f64 = 1.25;
/// How often waiting pictures and frames in flight are looked at.
const TICK: Duration = Duration::from_millis(4);
/// Scaling a picture on the GPU takes about this long, whatever the sizes (measured on
/// an M-series Mac: mostly the round trip).
const SCALE_COST: Duration = Duration::from_millis(3);

/// One track's encoder, the picture size it was made for, and how it's being fed.
struct Encoding {
    encoder: Encoder,
    size: (usize, usize),
    interval: Duration,
    /// Whether frames may be left in flight while the next is encoded (see
    /// [`Encoder::encode_overlapped`]), because they take most of their time budget.
    overlap: bool,
    /// Never overlap (the sharp track, while adaptive: it sends lone pictures).
    never_overlap: bool,
    /// Frames in a row that took most of their time budget, while not overlapped.
    slow: u32,
    /// The last frame sent to the encoder, and whether it may still be in flight.
    last_pts: Option<Duration>,
    in_flight: bool,
    last_encode: Option<Instant>,
    /// The encoder's dropped-frame count already reported.
    dropped_seen: u32,
    /// When each picture handed to the encoder appeared on the display, by timestamp:
    /// with frames overlapping, the one coming out isn't the one just handed in.
    shown_at: Arc<Mutex<VecDeque<(Duration, Instant)>>>,
}

impl Encoding {
    /// `extra` is time spent on each picture before it's encoded (scaling it).
    fn new(
        track: u8,
        (width, height): (u32, u32),
        fps: u32,
        bitrate: u32,
        never_overlap: bool,
        extra: Duration,
        sink: &Sink,
    ) -> Result<Self> {
        let shown_at: Arc<Mutex<VecDeque<(Duration, Instant)>>> = Arc::default();
        let encoder = {
            let (sink, shown_at) = (sink.clone(), shown_at.clone());
            Encoder::new(
                EncoderConfig {
                    width,
                    height,
                    fps,
                    bitrate,
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
                        frame.track = track;
                        (sink.lock().unwrap())(frame);
                    }
                    Err(e) => tracing::debug!("{e:#}"),
                },
            )?
        };
        let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
        let megapixels = width as f64 * height as f64 / 1e6;
        let expected = Duration::from_secs_f64(megapixels * 1.95e-3) + extra;
        Ok(Self {
            encoder,
            size: (width as usize, height as usize),
            interval,
            overlap: !never_overlap
                && expected.as_secs_f64() > interval.as_secs_f64() * OVERLAP_ABOVE,
            never_overlap,
            slow: 0,
            last_pts: None,
            in_flight: false,
            last_encode: None,
            dropped_seen: 0,
            shown_at,
        })
    }

    /// `started`: when work on the picture began (before scaling it).
    fn encode(
        &mut self,
        image: &CVPixelBuffer,
        pts: Duration,
        keyframe: bool,
        shown_at: Instant,
        started: Instant,
    ) -> Result<()> {
        self.shown_at.lock().unwrap().push_back((pts, shown_at));
        let continuous = self.last_encode.is_some_and(|t| {
            started.saturating_duration_since(t) < self.interval.mul_f64(CONTINUOUS)
        });
        if self.overlap && continuous {
            self.encoder
                .encode_overlapped(image, pts, keyframe, self.last_pts)?;
            self.in_flight = true;
        } else {
            // Also sends a frame left in flight before this one.
            self.encoder.encode_now(image, pts, keyframe)?;
            self.in_flight = false;
            // Falling behind at this size: overlap from now on.
            if !self.never_overlap && !self.overlap {
                if started.elapsed() > self.interval.mul_f64(0.9) {
                    self.slow += 1;
                    if self.slow >= SLOW_FRAMES {
                        tracing::info!(
                            "encoding takes most of each frame's time: overlapping frames"
                        );
                        self.overlap = true;
                    }
                } else {
                    self.slow = 0;
                }
            }
        }
        self.last_pts = Some(pts);
        self.last_encode = Some(Instant::now());
        Ok(())
    }

    /// Sends the frame left in flight if no other has followed it (the screen went
    /// still), so the viewer isn't left a frame behind.
    fn flush_if_idle(&mut self) {
        let idle = self
            .last_encode
            .is_none_or(|t| t.elapsed() > self.interval.mul_f64(FLUSH_AFTER));
        if self.in_flight && idle {
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

/// The smaller track while moving: its encoder, and the scaler feeding it.
struct Moving {
    encoding: Encoding,
    scaler: Scaler,
}

#[derive(Clone)]
struct LastImage {
    image: CFRetained<CVPixelBuffer>,
    shown_at: Instant,
}
// SAFETY: pixel buffers are reference counted and safe to hand between threads.
unsafe impl Send for LastImage {}

/// Everything touched per picture, behind one lock so pictures, ticks and keyframe
/// requests are handled one at a time, in order.
struct Pipeline {
    sharp: Encoding,
    moving: Option<Moving>,
    policy: Policy,
    /// The latest picture: re-encoded when the screen is still (sharpening, keyframes).
    last: Option<LastImage>,
    /// The last timestamp given out: every track shares the clock, and no two frames get
    /// the same time.
    last_pts: Option<Duration>,
}

impl Pipeline {
    fn new(config: StreamConfig, sink: &Sink) -> Result<Self> {
        let stream_size = (config.stream_width, config.stream_height);
        let (sharp, moving, policy) = match config.moving {
            Some(size) => {
                let sharp_fps = config.sharp_fps.clamp(1, config.fps.max(1));
                let sharp = Encoding::new(
                    SHARP,
                    stream_size,
                    sharp_fps,
                    config.bitrate,
                    true,
                    Duration::ZERO,
                    sink,
                )?;
                let moving = Moving {
                    encoding: Encoding::new(
                        MOVING,
                        size,
                        config.fps,
                        config.bitrate,
                        false,
                        SCALE_COST,
                        sink,
                    )?,
                    scaler: Scaler::new(size.0, size.1)?,
                };
                (sharp, Some(moving), Policy::adaptive(sharp_fps))
            }
            None => {
                let sharp = Encoding::new(
                    SHARP,
                    stream_size,
                    config.fps,
                    config.bitrate,
                    false,
                    Duration::ZERO,
                    sink,
                )?;
                (sharp, None, Policy::fixed())
            }
        };
        Ok(Self {
            sharp,
            moving,
            policy,
            last: None,
            last_pts: None,
        })
    }

    /// Whole microseconds from `start`, as the encoder hands timestamps back.
    fn next_pts(&mut self, start: Instant) -> Duration {
        let mut pts = Duration::from_micros(start.elapsed().as_micros() as u64);
        if let Some(last) = self.last_pts
            && pts <= last
        {
            pts = last + Duration::from_micros(1);
        }
        self.last_pts = Some(pts);
        pts
    }
}

struct Shared {
    pipeline: Mutex<Pipeline>,
    start: Instant,
    /// Per track: set while the network can't keep up, so new pictures are skipped
    /// rather than queued.
    backlogged: [AtomicBool; 2],
    /// Pictures skipped (or dropped by an encoder) since the viewer last heard.
    missed: AtomicU32,
    stop: AtomicBool,
}

impl Shared {
    /// Carries out `decision` for `picture`. `fresh`: a new picture, not one re-sent.
    fn run(&self, p: &mut Pipeline, decision: Decision, picture: &LastImage, fresh: bool) {
        let (track, keyframe, shown_at) = match decision {
            Decision::Wait => return,
            Decision::Encode { track, keyframe } => (track, keyframe, picture.shown_at),
            Decision::Sharpen { keyframe } => (SHARP, keyframe, Instant::now()),
        };
        let backlogged = self
            .backlogged
            .get(track as usize)
            .is_some_and(|b| b.load(Ordering::Relaxed));
        if backlogged && !keyframe {
            if fresh {
                self.missed.fetch_add(1, Ordering::Relaxed);
            }
            p.policy.deferred(decision);
            return;
        }
        let pts = p.next_pts(self.start);
        let started = Instant::now();
        let result = if track == MOVING {
            match p.moving.as_mut() {
                Some(m) => m.scaler.scale(&picture.image).and_then(|scaled| {
                    m.encoding.encode(&scaled, pts, keyframe, shown_at, started)
                }),
                None => Ok(()),
            }
        } else {
            p.sharp
                .encode(&picture.image, pts, keyframe, shown_at, started)
        };
        if let Err(e) = result {
            tracing::debug!("{e:#}");
        }
        let dropped =
            p.sharp.newly_dropped() + p.moving.as_mut().map_or(0, |m| m.encoding.newly_dropped());
        if dropped > 0 {
            self.missed.fetch_add(dropped, Ordering::Relaxed);
        }
    }

    fn on_picture(&self, image: CFRetained<CVPixelBuffer>, shown_at: Instant, changed: f64) {
        let mut p = self.pipeline.lock().unwrap();
        // While the size changes, capture can still deliver a few old-size pictures.
        let size = (
            CVPixelBufferGetWidth(&image),
            CVPixelBufferGetHeight(&image),
        );
        if size != p.sharp.size {
            return;
        }
        let picture = LastImage { image, shown_at };
        p.last = Some(picture.clone());
        let was_moving = p.policy.is_moving();
        let decision = p.policy.on_frame(Instant::now(), changed);
        if p.policy.is_moving() && !was_moving {
            tracing::debug!(
                "moving ({:.1}% changed): sending the smaller picture",
                changed * 100.0
            );
        }
        self.run(&mut p, decision, &picture, true);
    }

    fn on_tick(&self) {
        let mut p = self.pipeline.lock().unwrap();
        p.sharp.flush_if_idle();
        if let Some(m) = p.moving.as_mut() {
            m.encoding.flush_if_idle();
        }
        if let Some(decision) = p.policy.on_tick(Instant::now())
            && let Some(picture) = p.last.clone()
        {
            if let Decision::Sharpen { .. } = decision {
                tracing::debug!("still: sharpening the picture");
            }
            self.run(&mut p, decision, &picture, false);
        }
    }
}

/// Shareable between threads: everything it offers only touches thread-safe state.
pub struct DisplayStream {
    // Field order is drop order: stop capturing, then encoding, then remove the display.
    capture: ScreenCapture,
    ticker: Option<std::thread::JoinHandle<()>>,
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
        let sink: Sink = Arc::new(Mutex::new(Box::new(on_frame)));
        let display = VirtualDisplay::create(name, config.mode())?;
        let shared = Arc::new(Shared {
            pipeline: Mutex::new(Pipeline::new(config, &sink)?),
            start: Instant::now(),
            backlogged: [AtomicBool::new(false), AtomicBool::new(false)],
            missed: AtomicU32::new(0),
            stop: AtomicBool::new(false),
        });
        let capture = {
            let shared = shared.clone();
            ScreenCapture::start(
                display.id(),
                config.stream_width,
                config.stream_height,
                config.fps,
                move |frame| shared.on_picture(frame.image, frame.shown_at, frame.changed),
            )?
        };
        let ticker = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("legato-encode-tick".into())
                .spawn(move || {
                    while !shared.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(TICK);
                        shared.on_tick();
                    }
                })?
        };
        Ok(Self {
            capture,
            ticker: Some(ticker),
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

    /// Changes the display's size, Retina mode, stream sizes or frame rate while
    /// streaming. Every track starts again with a keyframe at its new size.
    pub fn reconfigure(&self, config: StreamConfig) -> Result<()> {
        let mut current = self.config.lock().unwrap();
        if *current == config {
            return Ok(());
        }
        if config.mode() != current.mode() {
            self.display.set_mode(config.mode())?;
        }
        {
            let mut p = self.shared.pipeline.lock().unwrap();
            let last_pts = p.last_pts;
            *p = Pipeline::new(config, &self.sink)?;
            p.last_pts = last_pts;
        }
        self.capture
            .reconfigure(config.stream_width, config.stream_height, config.fps)?;
        *current = config;
        Ok(())
    }

    /// Sends a keyframe on `track` as soon as possible (a viewer joined or lost frames).
    pub fn request_keyframe(&self, track: u8) {
        let mut p = self.shared.pipeline.lock().unwrap();
        // If the screen is still, no new picture is coming: re-send the last one, which
        // is still on screen and so current as of now.
        if let Some(decision) = p.policy.keyframe_now(track)
            && let Some(mut picture) = p.last.clone()
        {
            picture.shown_at = Instant::now();
            self.shared.run(&mut p, decision, &picture, false);
        }
    }

    /// While a track's frames back up on the network, its new pictures are skipped
    /// rather than queued.
    pub fn set_backlogged(&self, track: u8, backlogged: bool) {
        if let Some(b) = self.shared.backlogged.get(track as usize) {
            b.store(backlogged, Ordering::Relaxed);
        }
    }

    /// Pictures skipped or dropped since the last call, for the viewer's stats.
    pub fn take_missed(&self) -> u32 {
        self.shared.missed.swap(0, Ordering::Relaxed)
    }
}

impl Drop for DisplayStream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(ticker) = self.ticker.take() {
            let _ = ticker.join();
        }
    }
}
