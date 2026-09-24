//! Input-sharing sessions with paired peers.
//!
//! Exactly one connection per pair of machines: the machine with the smaller id dials,
//! the other accepts. (iroh also misbehaves with several connections to one peer.)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId};
use legato_proto::{
    Control, Datagram, Hello, PROTOCOL_VERSION, SESSION_ALPN, Screens, VideoFrameHeader,
};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::Shared;
use crate::framing::{expect_frame, read_frame, write_frame};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(10);
/// Stream priorities: input (the control stream, 0) beats video, which beats bulk data.
const VIDEO_PRIORITY: i32 = -1;
const BULK_PRIORITY: i32 = -2;

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Connected(Arc<Session>),
    Control {
        peer: EndpointId,
        msg: Control,
    },
    Datagram {
        peer: EndpointId,
        msg: Datagram,
    },
    /// A small bulk transfer (clipboard contents), read into memory.
    Blob {
        peer: EndpointId,
        tag: u8,
        data: Vec<u8>,
    },
    /// A file transfer. Read it with [`IncomingFile::recv`].
    File {
        peer: EndpointId,
        file: Arc<IncomingFile>,
    },
    /// A video stream (virtual monitor mode). Read frames with [`IncomingVideo::next`].
    Video {
        peer: EndpointId,
        video: Arc<IncomingVideo>,
    },
    Disconnected {
        peer: EndpointId,
        reason: String,
    },
}

/// A file arriving on its own stream.
#[derive(Debug)]
pub struct IncomingFile {
    pub header: legato_proto::FileHeader,
    recv: tokio::sync::Mutex<Option<RecvStream>>,
}

impl IncomingFile {
    /// Streams the file's bytes into `out`. Can only be called once.
    pub async fn recv(&self, out: &mut (impl tokio::io::AsyncWrite + Unpin)) -> Result<u64> {
        let mut recv = self.recv.lock().await.take().context("already received")?;
        let copied = tokio::io::copy(&mut recv, out).await?;
        if copied != self.header.size {
            bail!("expected {} bytes, got {copied}", self.header.size);
        }
        Ok(copied)
    }
}

/// A video stream arriving from a peer.
#[derive(Debug)]
pub struct IncomingVideo {
    recv: tokio::sync::Mutex<RecvStream>,
}

impl IncomingVideo {
    /// The next frame and whether it's a keyframe, or `None` once the stream ends.
    pub async fn next(&self) -> Result<Option<(Vec<u8>, bool)>> {
        let mut recv = self.recv.lock().await;
        let mut header = [0u8; VideoFrameHeader::SIZE];
        match recv.read_exact(&mut header).await {
            Ok(()) => {}
            Err(iroh::endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let header = VideoFrameHeader::decode(header).context("bad video frame header")?;
        let mut data = vec![0u8; header.len as usize];
        recv.read_exact(&mut data).await?;
        Ok(Some((data, header.keyframe)))
    }
}

/// Sends frames on a video stream, in order.
#[derive(Debug)]
pub struct VideoSender {
    send: SendStream,
}

impl VideoSender {
    /// Resolves once the frame is handed to the connection (not when it arrives).
    pub async fn send(&mut self, frame: &[u8], keyframe: bool) -> Result<()> {
        let header = VideoFrameHeader {
            len: frame.len().try_into().context("frame too large")?,
            keyframe,
        };
        self.send.write_all(&header.encode()).await?;
        self.send.write_all(frame).await?;
        Ok(())
    }

    pub fn finish(mut self) {
        let _ = self.send.finish();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// Straight between the two machines (LAN, or hole-punched).
    Direct,
    /// Through a relay server.
    Relay,
}

/// A live session with one peer. Sending never blocks, so it's safe from input hooks.
#[derive(Debug)]
pub struct Session {
    pub peer: EndpointId,
    /// The peer's `Hello`.
    pub remote: Hello,
    conn: Connection,
    control: mpsc::UnboundedSender<Control>,
}

impl Session {
    /// Queues a reliable control message. Returns false if the session has ended.
    pub fn send(&self, msg: Control) -> bool {
        self.control.send(msg).is_ok()
    }

    /// Sends an unreliable datagram. Returns false if it couldn't be sent.
    pub fn send_datagram(&self, msg: &Datagram) -> bool {
        let bytes = Bytes::from(legato_proto::encode_datagram(msg));
        self.conn.send_datagram(bytes).is_ok()
    }

    /// How the connection currently travels, and its round-trip time.
    pub fn path(&self) -> Option<(PathKind, Duration)> {
        self.conn.paths().iter().find(|p| p.is_selected()).map(|p| {
            let kind = if p.is_relay() {
                PathKind::Relay
            } else {
                PathKind::Direct
            };
            (kind, p.rtt())
        })
    }

    pub fn close(&self) {
        self.conn.close(0u32.into(), b"closed");
    }

    /// Sends a blob on its own stream, below input in priority, without waiting.
    pub fn send_blob(&self, tag: u8, data: Vec<u8>) {
        let conn = self.conn.clone();
        tokio::spawn(async move {
            let result = async {
                let mut send = conn.open_uni().await?;
                send.set_priority(BULK_PRIORITY)?;
                send.write_all(&[tag]).await?;
                send.write_all(&(data.len() as u64).to_le_bytes()).await?;
                send.write_all(&data).await?;
                send.finish()?;
                let _ = send.stopped().await;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(e) = result {
                tracing::debug!("sending blob failed: {e:#}");
            }
        });
    }

    /// Streams a file to the peer, below input in priority. Resolves once it's sent.
    pub async fn send_file(
        &self,
        header: legato_proto::FileHeader,
        data: &mut (impl tokio::io::AsyncRead + Unpin),
    ) -> Result<()> {
        let mut send = self.conn.open_uni().await?;
        send.set_priority(BULK_PRIORITY)?;
        send.write_all(&[legato_proto::blob::FILE]).await?;
        let size = header.size;
        write_frame(&mut send, &header).await?;
        let sent =
            tokio::io::copy(&mut tokio::io::AsyncReadExt::take(data, size), &mut send).await?;
        if sent != size {
            bail!("file changed while sending ({sent} of {size} bytes)");
        }
        send.finish()?;
        let _ = send.stopped().await;
        Ok(())
    }

    /// Opens a video stream to the peer.
    pub async fn open_video(&self) -> Result<VideoSender> {
        let mut send = self.conn.open_uni().await?;
        send.set_priority(VIDEO_PRIORITY)?;
        send.write_all(&[legato_proto::blob::VIDEO]).await?;
        Ok(VideoSender { send })
    }
}

/// Session state while sessions are running.
#[derive(Debug)]
pub(crate) struct Hub {
    hello: Hello,
    events: mpsc::UnboundedSender<SessionEvent>,
    active: HashMap<EndpointId, Arc<Session>>,
    dialers: Vec<tokio::task::JoinHandle<()>>,
}

impl Hub {
    pub(crate) fn update_screens(&mut self, screens: Screens) {
        self.hello.screens = screens.clone();
        for session in self.active.values() {
            session.send(Control::Screens(screens.clone()));
        }
    }

    pub(crate) fn close(&self, peer: &EndpointId) {
        if let Some(session) = self.active.get(peer) {
            session.close();
        }
    }

    pub(crate) fn close_all(&self) {
        for session in self.active.values() {
            session.close();
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        for dialer in &self.dialers {
            dialer.abort();
        }
    }
}

pub(crate) fn start(
    shared: Arc<Shared>,
    endpoint: Endpoint,
    hello: Hello,
) -> mpsc::UnboundedReceiver<SessionEvent> {
    let (events, rx) = mpsc::unbounded_channel();
    let own = endpoint.id();
    let dialers = shared
        .store
        .peer_ids()
        .into_iter()
        .filter(|peer| should_dial(own, *peer))
        .map(|peer| tokio::spawn(dial_loop(shared.clone(), endpoint.clone(), peer)))
        .collect();
    let old = shared.sessions.lock().unwrap().replace(Hub {
        hello,
        events,
        active: HashMap::new(),
        dialers,
    });
    if let Some(old) = old {
        old.close_all();
    }
    rx
}

/// Starts dialing a newly paired peer if sessions are running and it's our turn to dial.
pub(crate) fn add_peer(shared: &Arc<Shared>, endpoint: &Endpoint, peer: EndpointId) {
    let mut hub = shared.sessions.lock().unwrap();
    if let Some(hub) = hub.as_mut()
        && should_dial(endpoint.id(), peer)
    {
        hub.dialers.push(tokio::spawn(dial_loop(
            shared.clone(),
            endpoint.clone(),
            peer,
        )));
    }
}

/// The machine with the smaller id dials.
fn should_dial(own: EndpointId, peer: EndpointId) -> bool {
    own.as_bytes() < peer.as_bytes()
}

async fn dial_loop(shared: Arc<Shared>, endpoint: Endpoint, peer: EndpointId) {
    let mut backoff = MIN_BACKOFF;
    loop {
        if !shared.allowed.read().unwrap().contains(&peer) {
            return;
        }
        let started = Instant::now();
        if let Err(e) = dial(&shared, &endpoint, peer).await {
            tracing::debug!(peer = %peer.fmt_short(), "session attempt failed: {e:#}");
        }
        if started.elapsed() > Duration::from_secs(30) {
            backoff = MIN_BACKOFF;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn dial(shared: &Arc<Shared>, endpoint: &Endpoint, peer: EndpointId) -> Result<()> {
    let conn = timeout(CONNECT_TIMEOUT, endpoint.connect(peer, SESSION_ALPN))
        .await
        .context("timed out connecting")??;
    let (mut send, mut recv) = conn.open_bi().await?;
    write_frame(&mut send, &Control::Hello(our_hello(shared)?)).await?;
    let remote = read_hello(&mut recv).await?;
    run(shared, conn, send, recv, remote).await
}

fn our_hello(shared: &Shared) -> Result<Hello> {
    shared
        .sessions
        .lock()
        .unwrap()
        .as_ref()
        .map(|hub| hub.hello.clone())
        .context("sessions are not running")
}

async fn read_hello(recv: &mut RecvStream) -> Result<Hello> {
    let msg = timeout(HELLO_TIMEOUT, expect_frame::<Control>(recv))
        .await
        .context("timed out waiting for hello")??;
    let Control::Hello(hello) = msg else {
        bail!("expected hello, got {msg:?}");
    };
    if hello.protocol != PROTOCOL_VERSION {
        bail!(
            "peer speaks protocol {} (Legato {}), we speak {PROTOCOL_VERSION}",
            hello.protocol,
            hello.app_version
        );
    }
    Ok(hello)
}

/// Runs a session until the connection ends.
async fn run(
    shared: &Arc<Shared>,
    conn: Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    remote: Hello,
) -> Result<()> {
    let peer = conn.remote_id();
    let (control, mut outgoing) = mpsc::unbounded_channel();
    let session = Arc::new(Session {
        peer,
        remote,
        conn: conn.clone(),
        control,
    });
    let events = {
        let mut hub = shared.sessions.lock().unwrap();
        let hub = hub.as_mut().context("sessions are not running")?;
        if let Some(old) = hub.active.insert(peer, session.clone()) {
            old.close();
        }
        hub.events.clone()
    };
    let _ = events.send(SessionEvent::Connected(session.clone()));

    let reader = async {
        while let Some(msg) = read_frame::<Control>(&mut recv).await? {
            let _ = events.send(SessionEvent::Control { peer, msg });
        }
        Ok::<_, anyhow::Error>(())
    };
    let datagrams = async {
        loop {
            let bytes = conn.read_datagram().await?;
            match legato_proto::decode_datagram(&bytes) {
                Ok(msg) => {
                    let _ = events.send(SessionEvent::Datagram { peer, msg });
                }
                Err(e) => tracing::debug!("ignoring bad datagram: {e}"),
            }
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    };
    let writer = async {
        while let Some(msg) = outgoing.recv().await {
            write_frame(&mut send, &msg).await?;
        }
        Ok::<_, anyhow::Error>(())
    };
    let blobs = async {
        loop {
            let recv = conn.accept_uni().await?;
            let events = events.clone();
            tokio::spawn(async move {
                if let Err(e) = receive_blob(peer, recv, &events).await {
                    tracing::debug!("receiving blob failed: {e:#}");
                }
            });
        }
        #[allow(unreachable_code)]
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! {
        r = reader => r,
        r = datagrams => r,
        r = writer => r,
        r = blobs => r,
    };
    conn.close(0u32.into(), b"bye");

    let still_active = {
        let mut hub = shared.sessions.lock().unwrap();
        match hub.as_mut() {
            Some(hub)
                if hub
                    .active
                    .get(&peer)
                    .is_some_and(|s| Arc::ptr_eq(s, &session)) =>
            {
                hub.active.remove(&peer);
                true
            }
            _ => false,
        }
    };
    if still_active {
        let reason = match &result {
            Ok(()) => "peer ended the session".to_string(),
            Err(e) => format!("{e:#}"),
        };
        let _ = events.send(SessionEvent::Disconnected { peer, reason });
    }
    result
}

async fn receive_blob(
    peer: EndpointId,
    mut recv: RecvStream,
    events: &mpsc::UnboundedSender<SessionEvent>,
) -> Result<()> {
    let mut tag = [0u8; 1];
    recv.read_exact(&mut tag).await?;
    if tag[0] == legato_proto::blob::FILE {
        let header: legato_proto::FileHeader = expect_frame(&mut recv).await?;
        let file = Arc::new(IncomingFile {
            header,
            recv: tokio::sync::Mutex::new(Some(recv)),
        });
        let _ = events.send(SessionEvent::File { peer, file });
        return Ok(());
    }
    if tag[0] == legato_proto::blob::VIDEO {
        let video = Arc::new(IncomingVideo {
            recv: tokio::sync::Mutex::new(recv),
        });
        let _ = events.send(SessionEvent::Video { peer, video });
        return Ok(());
    }
    let mut len = [0u8; 8];
    recv.read_exact(&mut len).await?;
    let len = u64::from_le_bytes(len);
    if len > legato_proto::MAX_BLOB_LEN {
        recv.stop(1u32.into())?;
        bail!("blob of {len} bytes is too large");
    }
    let data = recv.read_to_end(len as usize).await?;
    let _ = events.send(SessionEvent::Blob {
        peer,
        tag: tag[0],
        data,
    });
    Ok(())
}

/// Accepts sessions from paired peers (the allow-list hook has already checked).
#[derive(Debug, Clone)]
pub(crate) struct Handler(pub(crate) Arc<Shared>);

impl ProtocolHandler for Handler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let shared = &self.0;
        let hello = match our_hello(shared) {
            Ok(hello) => hello,
            Err(_) => {
                conn.close(1u32.into(), b"not sharing input right now");
                return Ok(());
            }
        };
        let result = async {
            let (mut send, mut recv) = timeout(HELLO_TIMEOUT, conn.accept_bi()).await??;
            let remote = read_hello(&mut recv).await?;
            write_frame(&mut send, &Control::Hello(hello)).await?;
            run(shared, conn.clone(), send, recv, remote).await
        }
        .await;
        if let Err(e) = result {
            tracing::debug!(peer = %conn.remote_id().fmt_short(), "session ended: {e:#}");
        }
        Ok(())
    }
}
