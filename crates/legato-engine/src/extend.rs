//! Virtual monitor mode: a Mac's extra display, shown in a window on a Windows PC.
//!
//! The viewer asks with `ExtendRequest`. The Mac adds a display, streams it on one video
//! stream per track (two with adaptive quality, see `legato_screen::adaptive`), and
//! replies `Extended` with where the display sits. The viewer shows whichever track's
//! picture is newest. While the pointer is over the viewer window's picture, the
//! viewer's capture sends input there (a portal).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use legato_proto::Rect;
use tokio::sync::watch;

/// A picture from the Mac's extra display, ready to draw.
pub type ViewerFrame = Arc<Picture>;

#[derive(Debug)]
pub struct Picture {
    pub nv12: legato_screen::Nv12,
    /// When decoding finished, so the viewer can tell how long it took to show.
    pub decoded_at: Instant,
    /// How long it spent on the Mac, on the network and in the decoder.
    pub mac: Duration,
    pub network: Duration,
    pub decode: Duration,
}

/// How the extra display's stream is doing, averaged over the last half second or so.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerStats {
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub megabits_per_second: f32,
    /// Capture, encoding and queueing on the Mac.
    pub mac: Duration,
    /// Half the round trip.
    pub network: Duration,
    pub decode: Duration,
    pub relayed: bool,
    /// The worst frames: what stutters.
    pub mac_max: Duration,
    pub decode_max: Duration,
    /// The longest wait between pictures while they were coming (pauses of more than a
    /// quarter of a second are the screen standing still, not a stutter).
    pub longest_gap: Duration,
    /// Pictures the Mac skipped (the network backed up) or its encoder dropped.
    pub skipped: u32,
    /// Adaptive quality: the picture shown is the smaller one sent while the screen
    /// moves.
    pub moving: bool,
}

/// How often the stats are updated.
const STATS_EVERY: Duration = Duration::from_millis(500);
/// Longer than this between pictures is the screen standing still, not a stutter.
const STILL: Duration = Duration::from_millis(250);

/// Running totals for [`ViewerStats`].
#[derive(Debug, Default)]
struct Totals {
    frames: u32,
    bytes: u64,
    mac: Duration,
    decode: Duration,
    mac_max: Duration,
    decode_max: Duration,
    longest_gap: Duration,
    skipped: u32,
}

/// A frame from the Mac, decoded, for the stats.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct Decoded {
    pub(crate) bytes: usize,
    /// How long it spent on the Mac.
    pub(crate) mac: Duration,
    pub(crate) decode: Duration,
    /// Pictures the Mac skipped or dropped just before it.
    pub(crate) skipped: u16,
}

/// The pictures of the display being shown, from all of its tracks: the newest is shown,
/// and the stats cover them all.
#[derive(Debug)]
pub(crate) struct Showing {
    inner: Mutex<ShowingInner>,
}

#[derive(Debug)]
struct ShowingInner {
    /// The timestamp and track of the picture on screen.
    newest: Option<(Duration, u8)>,
    size: (u32, u32),
    last_shown: Option<Instant>,
    totals: Totals,
    since: Instant,
    decoders: usize,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Showing {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(ShowingInner {
                newest: None,
                size: (0, 0),
                last_shown: None,
                totals: Totals::default(),
                since: Instant::now(),
                decoders: 0,
            }),
        }
    }

    /// Shows `picture` (from `track`, taken at `pts`) in `frames` if it's newer than the
    /// one there. Returns whether it was.
    pub(crate) fn show(
        &self,
        track: u8,
        pts: Duration,
        picture: Picture,
        frames: &watch::Sender<Option<ViewerFrame>>,
    ) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.newest.is_some_and(|(newest, _)| pts <= newest) {
            return false;
        }
        inner.newest = Some((pts, track));
        inner.size = (picture.nv12.width, picture.nv12.height);
        let at = picture.decoded_at;
        if let Some(previous) = inner.last_shown.replace(at) {
            let gap = at.saturating_duration_since(previous);
            if gap < STILL {
                inner.totals.longest_gap = inner.totals.longest_gap.max(gap);
            }
        }
        // Under the lock, so a slower track can't show an older picture after this one.
        frames.send_replace(Some(Arc::new(picture)));
        true
    }

    /// Counts a frame. Every [`STATS_EVERY`], returns the stats since the last time.
    pub(crate) fn record(
        &self,
        frame: Decoded,
        network: Duration,
        relayed: bool,
    ) -> Option<ViewerStats> {
        let mut inner = self.inner.lock().unwrap();
        let t = &mut inner.totals;
        t.frames += 1;
        t.bytes += frame.bytes as u64;
        t.mac += frame.mac;
        t.decode += frame.decode;
        t.mac_max = t.mac_max.max(frame.mac);
        t.decode_max = t.decode_max.max(frame.decode);
        t.skipped += u32::from(frame.skipped);
        let elapsed = inner.since.elapsed();
        if elapsed < STATS_EVERY {
            return None;
        }
        let t = std::mem::take(&mut inner.totals);
        inner.since = Instant::now();
        let secs = elapsed.as_secs_f32();
        Some(ViewerStats {
            width: inner.size.0,
            height: inner.size.1,
            fps: t.frames as f32 / secs,
            megabits_per_second: t.bytes as f32 * 8.0 / secs / 1e6,
            mac: t.mac / t.frames,
            network,
            decode: t.decode / t.frames,
            relayed,
            mac_max: t.mac_max,
            decode_max: t.decode_max,
            longest_gap: t.longest_gap,
            skipped: t.skipped,
            moving: inner
                .newest
                .is_some_and(|(_, track)| track == legato_proto::video_track::MOVING),
        })
    }

    /// A track's decoder started.
    pub(crate) fn decoder_started(&self) {
        self.inner.lock().unwrap().decoders += 1;
    }

    /// A track's decoder stopped. Returns whether it was the last.
    pub(crate) fn decoder_stopped(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.decoders = inner.decoders.saturating_sub(1);
        inner.decoders == 0
    }
}

#[cfg(target_os = "macos")]
pub(crate) use host::Host;

#[cfg(target_os = "macos")]
mod host {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use anyhow::Result;
    use legato_net::{EndpointId, Session, VideoFrame, VideoSender};
    use legato_proto::{Control, ExtendRequest, Rect, video_track};
    use legato_screen::mac::{DisplayStream, EncodedFrame, StreamConfig};
    use tokio::sync::mpsc;

    /// Frames waiting to go out on a track before its new pictures are skipped.
    const BACKLOG: usize = 3;

    fn to_rect(r: objc2_core_foundation::CGRect) -> Rect {
        Rect::new(r.origin.x, r.origin.y, r.size.width, r.size.height)
    }

    fn log_config(what: &str, c: StreamConfig) {
        let moving = c.moving.map_or(String::new(), |(w, h)| {
            format!(
                ", {w}x{h} while moving (sharp at up to {} fps)",
                c.sharp_fps
            )
        });
        tracing::info!(
            "{what}: {}x{}, sent at {}x{} and {} fps{moving}",
            c.width,
            c.height,
            c.stream_width,
            c.stream_height,
            c.fps
        );
    }

    /// This Mac's extra display, streaming to one viewer.
    pub(crate) struct Host {
        pub(crate) peer: EndpointId,
        session: Arc<Session>,
        stream: Arc<DisplayStream>,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    }

    /// What the stream should be for `request`, within what the Mac can encode.
    pub(super) fn stream_config(request: ExtendRequest) -> StreamConfig {
        use legato_core::extend::{fit_within, max_fps, usable_size};
        let (width, height) = usable_size(request.width, request.height);
        let stream = if request.stream_width == 0 {
            (width, height)
        } else {
            fit_within(
                usable_size(request.stream_width, request.stream_height),
                (width, height),
            )
        };
        let moving = (request.moving_width > 0)
            .then(|| {
                fit_within(
                    usable_size(request.moving_width, request.moving_height),
                    stream,
                )
            })
            .filter(|&moving| moving != stream);
        // Encoding sets the pace, at the size sent while things move.
        let pace = moving.unwrap_or(stream);
        StreamConfig {
            width,
            height,
            hidpi: request.hidpi,
            stream_width: stream.0,
            stream_height: stream.1,
            moving,
            fps: request.fps.clamp(1, max_fps(pace.0, pace.1)),
            sharp_fps: max_fps(stream.0, stream.1),
            bitrate: request.bitrate.clamp(1_000_000, 200_000_000),
        }
    }

    /// Sends one track's frames, opening its stream when the first comes.
    async fn send_track(
        session: Arc<Session>,
        stream: Arc<DisplayStream>,
        track: u8,
        mut frames: mpsc::UnboundedReceiver<EncodedFrame>,
        queued: Arc<AtomicUsize>,
    ) {
        let result = async {
            let mut video: Option<VideoSender> = None;
            while let Some(frame) = frames.recv().await {
                let waiting = queued.fetch_sub(1, Ordering::Relaxed) - 1;
                let sender = match &mut video {
                    Some(sender) => sender,
                    None => video.insert(session.open_video(track).await?),
                };
                sender
                    .send(&VideoFrame {
                        sender_time: frame.shown_at.map_or(Duration::ZERO, |t| t.elapsed()),
                        skipped: stream.take_missed().min(u16::MAX.into()) as u16,
                        keyframe: frame.keyframe,
                        pts: frame.pts,
                        data: frame.data,
                    })
                    .await?;
                stream.set_backlogged(track, waiting >= BACKLOG);
            }
            if let Some(video) = video {
                video.finish();
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(e) = result {
            tracing::debug!("video stream ended: {e:#}");
        }
    }

    impl Host {
        pub(crate) async fn start(
            session: Arc<Session>,
            request: ExtendRequest,
            name: String,
        ) -> Result<Self> {
            let (sharp_tx, sharp_rx) = mpsc::unbounded_channel::<EncodedFrame>();
            let (moving_tx, moving_rx) = mpsc::unbounded_channel::<EncodedFrame>();
            let queued = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
            let config = stream_config(request);
            let stream = {
                let queued = queued.clone();
                tokio::task::spawn_blocking(move || {
                    DisplayStream::start(&name, config, move |frame| {
                        let (tx, queued) = if frame.track == video_track::MOVING {
                            (&moving_tx, &queued[1])
                        } else {
                            (&sharp_tx, &queued[0])
                        };
                        queued.fetch_add(1, Ordering::Relaxed);
                        let _ = tx.send(frame);
                    })
                })
                .await??
            };
            let stream = Arc::new(stream);
            let mut bounds = to_rect(stream.display().bounds());
            session.send(Control::Extended { bounds });
            log_config("showing an extra display", config);
            let [sharp_queued, moving_queued] = queued;
            let mut tasks = vec![
                tokio::spawn(send_track(
                    session.clone(),
                    stream.clone(),
                    video_track::SHARP,
                    sharp_rx,
                    sharp_queued,
                )),
                tokio::spawn(send_track(
                    session.clone(),
                    stream.clone(),
                    video_track::MOVING,
                    moving_rx,
                    moving_queued,
                )),
            ];
            tasks.push({
                let stream = stream.clone();
                let session = session.clone();
                tokio::spawn(async move {
                    let mut check = tokio::time::interval(Duration::from_secs(1));
                    loop {
                        check.tick().await;
                        // The user may rearrange displays in System Settings.
                        let now = to_rect(stream.display().bounds());
                        if now != bounds {
                            bounds = now;
                            session.send(Control::Extended { bounds });
                        }
                    }
                })
            });
            Ok(Self {
                peer: session.peer,
                session,
                stream,
                tasks,
            })
        }

        pub(crate) fn request_keyframe(&self, track: u8) {
            let stream = self.stream.clone();
            // Re-encoding a still picture takes a few milliseconds.
            tokio::task::spawn_blocking(move || stream.request_keyframe(track));
        }

        /// Tells the viewer where the display is now.
        fn report_bounds(&self) {
            let bounds = to_rect(self.stream.display().bounds());
            self.session.send(Control::Extended { bounds });
        }

        /// Changes the display's size or frame rate, as the viewer asked.
        pub(crate) async fn reconfigure(&self, request: ExtendRequest) {
            let config = stream_config(request);
            let stream = self.stream.clone();
            match tokio::task::spawn_blocking(move || stream.reconfigure(config)).await {
                Ok(Ok(())) => {
                    log_config("extra display changed", config);
                    self.report_bounds();
                }
                Ok(Err(e)) => tracing::warn!("couldn't resize the extra display: {e:#}"),
                Err(e) => tracing::warn!("couldn't resize the extra display: {e}"),
            }
        }

        /// Moves the display in the Mac's arrangement, as the viewer asked.
        pub(crate) async fn arrange(&self, origin: legato_proto::Point) {
            let stream = self.stream.clone();
            let (x, y) = (origin.x.round() as i32, origin.y.round() as i32);
            match tokio::task::spawn_blocking(move || stream.display().set_origin(x, y)).await {
                Ok(Ok(())) => self.report_bounds(),
                Ok(Err(e)) => tracing::warn!("couldn't arrange the extra display: {e:#}"),
                Err(e) => tracing::warn!("couldn't arrange the extra display: {e}"),
            }
        }

        /// Stops streaming and removes the display.
        pub(crate) async fn stop(self) {
            for task in self.tasks {
                task.abort();
                let _ = task.await;
            }
            let stream = self.stream;
            // Stopping capture waits on ScreenCaptureKit.
            let _ = tokio::task::spawn_blocking(move || drop(stream)).await;
            tracing::info!("stopped showing the extra display");
        }
    }
}

#[cfg(windows)]
pub(crate) use viewer::decode;

#[cfg(windows)]
mod viewer {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use legato_net::{IncomingVideo, PathKind, Session, VideoFrame};
    use legato_proto::Control;
    use legato_screen::win::Decoder;
    use tokio::sync::{mpsc, watch};

    use super::{Decoded, Picture, Showing, ViewerFrame, ViewerStats};

    /// Decodes one of the Mac's video tracks until it ends, showing its pictures in
    /// `frames` when they're the newest of any track.
    pub(crate) fn decode(
        video: Arc<IncomingVideo>,
        session: Arc<Session>,
        showing: Arc<Showing>,
        frames: watch::Sender<Option<ViewerFrame>>,
        stats: watch::Sender<Option<ViewerStats>>,
    ) {
        let track = video.track;
        // Bounded, so a slow decoder slows the stream down (and the Mac skips pictures)
        // rather than falling ever further behind.
        let (tx, mut rx) = mpsc::channel::<VideoFrame>(4);
        tokio::spawn(async move {
            loop {
                match video.next().await {
                    Ok(Some(frame)) => {
                        if tx.send(frame).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!("video stream ended: {e:#}");
                        break;
                    }
                }
            }
        });
        showing.decoder_started();
        let spawned = std::thread::Builder::new()
            .name(format!("legato-decode-{track}"))
            .spawn(move || {
                let mut decoder = match Decoder::new() {
                    Ok(d) => d,
                    Err(e) => {
                        session.send(Control::ExtendStop {
                            reason: format!("{e:#}"),
                        });
                        showing.decoder_stopped();
                        return;
                    }
                };
                // Pictures only make sense from a keyframe on.
                let mut synced = false;
                while let Some(frame) = rx.blocking_recv() {
                    if !synced && !frame.keyframe {
                        continue;
                    }
                    synced = true;
                    let path = session.path();
                    let network = path.map_or(Duration::ZERO, |(_, rtt)| rtt / 2);
                    let started = Instant::now();
                    match decoder.decode(&frame.data) {
                        Ok(pictures) => {
                            let decoded_at = Instant::now();
                            let decode = decoded_at - started;
                            if let Some(nv12) = pictures.into_iter().last() {
                                let picture = Picture {
                                    nv12,
                                    decoded_at,
                                    mac: frame.sender_time,
                                    network,
                                    decode,
                                };
                                showing.show(track, frame.pts, picture, &frames);
                            }
                            let decoded = Decoded {
                                bytes: frame.data.len(),
                                mac: frame.sender_time,
                                decode,
                                skipped: frame.skipped,
                            };
                            let relayed = matches!(path, Some((PathKind::Relay, _)));
                            if let Some(s) = showing.record(decoded, network, relayed) {
                                stats.send_replace(Some(s));
                            }
                        }
                        Err(e) => {
                            tracing::debug!("{e:#}; asking for a keyframe");
                            synced = false;
                            session.send(Control::Keyframe { track });
                            if let Ok(fresh) = Decoder::new() {
                                decoder = fresh;
                            }
                        }
                    }
                }
                if showing.decoder_stopped() {
                    stats.send_replace(None);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("couldn't start decoding: {e}");
        }
    }
}

/// What the viewer knows about the display it's showing.
#[derive(Debug)]
pub(crate) struct Viewing {
    pub(crate) peer: legato_net::EndpointId,
    /// Its pictures, from every track.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) showing: Arc<Showing>,
    /// Where the display sits on the Mac, once it exists.
    pub(crate) bounds: Option<Rect>,
    /// Where it's shown here, in native coordinates.
    pub(crate) area: Option<Rect>,
    /// The area and display size the Mac was last asked to arrange for, so a request
    /// isn't repeated when macOS nudges the display a little.
    pub(crate) arranged_for: Option<(Rect, (f64, f64))>,
}

impl Viewing {
    pub(crate) fn new(peer: legato_net::EndpointId) -> Self {
        Self {
            peer,
            showing: Arc::new(Showing::new()),
            bounds: None,
            area: None,
            arranged_for: None,
        }
    }

    /// Where the Mac should put the display so it matches where it's shown here, if that
    /// hasn't been asked for already.
    pub(crate) fn arrangement(
        &mut self,
        layout: &legato_core::Layout,
        machine: legato_core::MachineId,
    ) -> Option<legato_proto::Point> {
        let (bounds, area) = (self.bounds?, self.area?);
        let key = (area, (bounds.width, bounds.height));
        if self.arranged_for == Some(key) {
            return None;
        }
        let origin = legato_core::extend::place_extra_display(
            layout,
            machine,
            area,
            (bounds.width, bounds.height),
        )?;
        self.arranged_for = Some(key);
        Some(origin)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use legato_proto::video_track::{MOVING, SHARP};
    use tokio::sync::watch;

    use super::{Decoded, Picture, Showing};

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn picture(width: u32) -> Picture {
        Picture {
            nv12: legato_screen::Nv12 {
                width,
                height: 2,
                stride: width,
                data: vec![0; width as usize * 3],
            },
            decoded_at: Instant::now(),
            mac: ms(5),
            network: ms(1),
            decode: ms(2),
        }
    }

    #[test]
    fn the_newest_picture_of_any_track_is_shown() {
        let showing = Showing::new();
        let (frames, shown) = watch::channel::<Option<super::ViewerFrame>>(None);
        let width = || shown.borrow().as_ref().map(|p| p.nv12.width);
        assert!(showing.show(SHARP, ms(100), picture(3840), &frames));
        // Dragging a window: the smaller pictures take over...
        assert!(showing.show(MOVING, ms(110), picture(1920), &frames));
        assert_eq!(width(), Some(1920));
        // ...and a sharp one that was on its way arrives too late to show.
        assert!(!showing.show(SHARP, ms(105), picture(3840), &frames));
        assert_eq!(width(), Some(1920));
        // Still again: sharpened.
        assert!(showing.show(SHARP, ms(300), picture(3840), &frames));
        assert_eq!(width(), Some(3840));
    }

    #[test]
    fn stats_cover_every_track() {
        let showing = Showing::new();
        let (frames, _shown) = watch::channel(None);
        let frame = |mac, skipped| Decoded {
            bytes: 125_000,
            mac: ms(mac),
            decode: ms(3),
            skipped,
        };
        showing.show(SHARP, ms(10), picture(3840), &frames);
        assert!(showing.record(frame(20, 0), ms(1), false).is_none());
        showing.show(MOVING, ms(20), picture(1920), &frames);
        showing.inner.lock().unwrap().since = Instant::now() - Duration::from_secs(1);
        let stats = showing.record(frame(8, 2), ms(1), false).unwrap();
        assert!(stats.moving, "showing the moving picture");
        assert_eq!((stats.width, stats.height), (1920, 2));
        assert_eq!(stats.mac, ms(14));
        assert_eq!(stats.mac_max, ms(20));
        assert_eq!(stats.skipped, 2);
        assert!((stats.megabits_per_second - 2.0).abs() < 0.1);
    }

    #[test]
    fn stats_stop_with_the_last_track() {
        let showing = Showing::new();
        showing.decoder_started();
        showing.decoder_started();
        assert!(!showing.decoder_stopped());
        assert!(showing.decoder_stopped());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_mac_streams_what_was_asked_within_what_it_can_encode() {
        let request = legato_proto::ExtendRequest {
            width: 3840,
            height: 2160,
            hidpi: true,
            stream_width: 0,
            stream_height: 0,
            moving_width: 1920,
            moving_height: 1080,
            fps: 144,
            bitrate: 40_000_000,
        };
        let c = super::host::stream_config(request);
        assert_eq!((c.stream_width, c.stream_height), (3840, 2160));
        assert_eq!(c.moving, Some((1920, 1080)));
        assert_eq!((c.fps, c.sharp_fps), (144, 60));
        // A moving size no smaller than the stream is no moving track at all.
        let c = super::host::stream_config(legato_proto::ExtendRequest {
            stream_width: 1920,
            stream_height: 1080,
            ..request
        });
        assert_eq!((c.moving, c.fps), (None, 144));
        let c = super::host::stream_config(legato_proto::ExtendRequest {
            moving_width: 0,
            moving_height: 0,
            ..request
        });
        assert_eq!((c.moving, c.fps), (None, 60));
    }
}
