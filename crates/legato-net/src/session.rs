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
use legato_proto::{Control, Datagram, Hello, PROTOCOL_VERSION, SESSION_ALPN, Screens};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::Shared;
use crate::framing::{expect_frame, read_frame, write_frame};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Connected(Arc<Session>),
    Control { peer: EndpointId, msg: Control },
    Datagram { peer: EndpointId, msg: Datagram },
    Disconnected { peer: EndpointId, reason: String },
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
    let result = tokio::select! {
        r = reader => r,
        r = datagrams => r,
        r = writer => r,
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
