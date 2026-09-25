//! Virtual monitor mode: a Mac's extra display, shown in a window on a Windows PC.
//!
//! The viewer asks with `ExtendRequest`. The Mac adds a display, streams it on a video
//! stream, and replies `Extended` with where the display sits. While the pointer is over
//! the viewer window's picture, the viewer's capture sends input there (a portal).

use std::sync::Arc;
use std::time::{Duration, Instant};

use legato_proto::Rect;

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
}

#[cfg(target_os = "macos")]
pub(crate) use host::Host;

#[cfg(target_os = "macos")]
mod host {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use anyhow::Result;
    use legato_net::{EndpointId, Session};
    use legato_proto::{Control, ExtendRequest, Rect};
    use legato_screen::mac::{DisplayStream, EncodedFrame, StreamConfig};
    use tokio::sync::mpsc;

    /// Frames waiting to go out before new pictures are skipped.
    const BACKLOG: usize = 3;

    fn to_rect(r: objc2_core_foundation::CGRect) -> Rect {
        Rect::new(r.origin.x, r.origin.y, r.size.width, r.size.height)
    }

    /// This Mac's extra display, streaming to one viewer.
    pub(crate) struct Host {
        pub(crate) peer: EndpointId,
        stream: Arc<DisplayStream>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Host {
        pub(crate) async fn start(
            session: Arc<Session>,
            request: ExtendRequest,
            name: String,
        ) -> Result<Self> {
            let (tx, mut rx) = mpsc::unbounded_channel::<EncodedFrame>();
            let queued = Arc::new(AtomicUsize::new(0));
            let config = StreamConfig {
                width: request.width,
                height: request.height,
                hidpi: request.hidpi,
                fps: request.fps,
                bitrate: request.bitrate,
            };
            let stream = {
                let queued = queued.clone();
                tokio::task::spawn_blocking(move || {
                    DisplayStream::start(&name, config, move |frame| {
                        queued.fetch_add(1, Ordering::Relaxed);
                        let _ = tx.send(frame);
                    })
                })
                .await??
            };
            let stream = Arc::new(stream);
            let mut bounds = to_rect(stream.display().bounds());
            session.send(Control::Extended { bounds });
            tracing::info!(
                "showing an extra display on \"{}\" ({}x{} pixels at {bounds:?})",
                session.remote.name,
                request.width,
                request.height
            );
            let task = {
                let stream = stream.clone();
                let session = session.clone();
                tokio::spawn(async move {
                    let result = async {
                        let mut video = session.open_video().await?;
                        let mut check = tokio::time::interval(Duration::from_secs(1));
                        loop {
                            tokio::select! {
                                frame = rx.recv() => {
                                    let Some(frame) = frame else { break };
                                    let waiting = queued.fetch_sub(1, Ordering::Relaxed) - 1;
                                    let spent = frame.shown_at.map_or(Duration::ZERO, |t| t.elapsed());
                                    video.send(&frame.data, frame.keyframe, spent).await?;
                                    stream.set_backlogged(waiting >= BACKLOG);
                                }
                                _ = check.tick() => {
                                    // The user may rearrange displays in System Settings.
                                    let now = to_rect(stream.display().bounds());
                                    if now != bounds {
                                        bounds = now;
                                        session.send(Control::Extended { bounds });
                                    }
                                }
                            }
                        }
                        video.finish();
                        Ok::<_, anyhow::Error>(())
                    }
                    .await;
                    if let Err(e) = result {
                        tracing::debug!("video stream ended: {e:#}");
                    }
                })
            };
            Ok(Self {
                peer: session.peer,
                stream,
                task,
            })
        }

        pub(crate) fn request_keyframe(&self) {
            self.stream.request_keyframe();
        }

        /// Stops streaming and removes the display.
        pub(crate) async fn stop(self) {
            self.task.abort();
            let _ = self.task.await;
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

    use super::{Picture, ViewerFrame, ViewerStats};

    /// How often the stats are updated.
    const STATS_EVERY: Duration = Duration::from_millis(500);

    /// Running totals for [`ViewerStats`].
    #[derive(Default)]
    struct Totals {
        frames: u32,
        bytes: u64,
        mac: Duration,
        decode: Duration,
    }

    /// Decodes a video stream from the Mac into `frames` until it ends.
    pub(crate) fn decode(
        video: Arc<IncomingVideo>,
        session: Arc<Session>,
        frames: watch::Sender<Option<ViewerFrame>>,
        stats: watch::Sender<Option<ViewerStats>>,
    ) {
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
        let spawned = std::thread::Builder::new()
            .name("legato-decode".into())
            .spawn(move || {
                let mut decoder = match Decoder::new() {
                    Ok(d) => d,
                    Err(e) => {
                        session.send(Control::ExtendStop {
                            reason: format!("{e:#}"),
                        });
                        return;
                    }
                };
                // Pictures only make sense from a keyframe on.
                let mut synced = false;
                let mut totals = Totals::default();
                let mut since = Instant::now();
                let mut size = (0, 0);
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
                            totals.frames += 1;
                            totals.bytes += frame.data.len() as u64;
                            totals.mac += frame.sender_time;
                            totals.decode += decode;
                            if let Some(nv12) = pictures.into_iter().last() {
                                size = (nv12.width, nv12.height);
                                frames.send_replace(Some(Arc::new(Picture {
                                    nv12,
                                    decoded_at,
                                    mac: frame.sender_time,
                                    network,
                                    decode,
                                })));
                            }
                        }
                        Err(e) => {
                            tracing::debug!("{e:#}; asking for a keyframe");
                            synced = false;
                            session.send(Control::Keyframe);
                            if let Ok(fresh) = Decoder::new() {
                                decoder = fresh;
                            }
                        }
                    }
                    let elapsed = since.elapsed();
                    if elapsed >= STATS_EVERY && totals.frames > 0 {
                        let secs = elapsed.as_secs_f32();
                        stats.send_replace(Some(ViewerStats {
                            width: size.0,
                            height: size.1,
                            fps: totals.frames as f32 / secs,
                            megabits_per_second: totals.bytes as f32 * 8.0 / secs / 1e6,
                            mac: totals.mac / totals.frames,
                            network,
                            decode: totals.decode / totals.frames,
                            relayed: matches!(path, Some((PathKind::Relay, _))),
                        }));
                        totals = Totals::default();
                        since = Instant::now();
                    }
                }
                stats.send_replace(None);
            });
        if let Err(e) = spawned {
            tracing::warn!("couldn't start decoding: {e}");
        }
    }
}

/// What the viewer knows about the display it's showing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Viewing {
    pub(crate) peer: legato_net::EndpointId,
    /// Where the display sits on the Mac, once it exists.
    pub(crate) bounds: Option<Rect>,
}
