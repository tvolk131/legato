//! Virtual monitor mode: a Mac's extra display, shown in a window on a Windows PC.
//!
//! The viewer asks with `ExtendRequest`. The Mac adds a display, streams it on a video
//! stream, and replies `Extended` with where the display sits. While the pointer is over
//! the viewer window's picture, the viewer's capture sends input there (a portal).

use std::sync::Arc;

use legato_proto::Rect;

/// A picture from the Mac's extra display, ready to draw.
pub type ViewerFrame = Arc<legato_screen::Nv12>;

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
                                    video.send(&frame.data, frame.keyframe).await?;
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

    use legato_net::{IncomingVideo, Session};
    use legato_proto::Control;
    use legato_screen::win::Decoder;
    use tokio::sync::{mpsc, watch};

    use super::ViewerFrame;

    /// Decodes a video stream from the Mac into `frames` until it ends.
    pub(crate) fn decode(
        video: Arc<IncomingVideo>,
        session: Arc<Session>,
        frames: watch::Sender<Option<ViewerFrame>>,
    ) {
        // Bounded, so a slow decoder slows the stream down (and the Mac skips pictures)
        // rather than falling ever further behind.
        let (tx, mut rx) = mpsc::channel::<(Vec<u8>, bool)>(4);
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
                while let Some((data, keyframe)) = rx.blocking_recv() {
                    if !synced && !keyframe {
                        continue;
                    }
                    synced = true;
                    match decoder.decode(&data) {
                        Ok(pictures) => {
                            if let Some(picture) = pictures.into_iter().last() {
                                frames.send_replace(Some(Arc::new(picture)));
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
                }
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
