//! Pairing: both users confirm the same 6-digit code, derived from the TLS session, so a
//! machine in the middle can't impersonate either side.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use legato_proto::{Os, PAIR_ALPN, PAIRING_CODE_LABEL, PROTOCOL_VERSION, Pair};
use tokio::time::timeout;

use crate::framing::{expect_frame, write_frame};
use crate::{PairedPeer, Shared};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the other person to confirm the code.
const DECISION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub id: EndpointId,
    pub name: String,
    pub os: Os,
    pub app_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairOutcome {
    Paired(PairedPeer),
    /// This side declined.
    DeclinedHere,
    /// The other side declined (or their codes didn't match).
    DeclinedThere,
}

/// A pairing in progress, waiting for the local user to confirm the code.
#[derive(Debug)]
pub struct PairAttempt {
    pub peer: PeerInfo,
    /// The 6-digit code both sides display; see [`legato_proto::format_pairing_code`].
    pub code: u32,
    /// Whether the other machine started this pairing.
    pub incoming: bool,
    shared: Arc<Shared>,
    conn: Connection,
    send: SendStream,
    recv: RecvStream,
    _busy: Option<BusyGuard>,
}

impl PairAttempt {
    /// Sends the local user's answer and waits for the other side's. The peers are paired
    /// (and saved) only if both accept.
    pub async fn decide(mut self, accept: bool) -> Result<PairOutcome> {
        write_frame(&mut self.send, &Pair::Decision { accept }).await?;
        self.send.finish().ok();
        if !accept {
            // Let the decision reach the peer before closing.
            let _ = timeout(HANDSHAKE_TIMEOUT, self.send.stopped()).await;
            self.conn.close(0u32.into(), b"declined");
            return Ok(PairOutcome::DeclinedHere);
        }
        let theirs = timeout(DECISION_TIMEOUT, expect_frame::<Pair>(&mut self.recv))
            .await
            .context("timed out waiting for the other device to confirm")?
            .context("the other device cancelled pairing")?;
        let Pair::Decision {
            accept: they_accept,
        } = theirs
        else {
            bail!("unexpected pairing message");
        };
        // Make sure they got our decision too before either side saves anything more.
        let _ = timeout(HANDSHAKE_TIMEOUT, self.send.stopped()).await;
        self.conn.close(0u32.into(), b"done");
        if !they_accept {
            return Ok(PairOutcome::DeclinedThere);
        }
        Ok(PairOutcome::Paired(self.shared.add_paired(&self.peer)?))
    }
}

/// Resets the one-pairing-at-a-time flag when an incoming attempt finishes.
#[derive(Debug)]
struct BusyGuard(Arc<Shared>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.pairing_busy.store(false, Ordering::SeqCst);
    }
}

fn our_hello(shared: &Shared) -> Pair {
    Pair::Hello {
        protocol: PROTOCOL_VERSION,
        app_version: shared.config.app_version.clone(),
        name: shared.config.name.clone(),
        os: shared.config.os,
    }
}

fn peer_info(id: EndpointId, hello: Pair) -> Result<PeerInfo> {
    match hello {
        Pair::Hello {
            protocol,
            app_version,
            name,
            os,
        } => {
            if protocol != PROTOCOL_VERSION {
                bail!(
                    "the other device runs Legato {app_version} (protocol {protocol}); this one \
                     speaks protocol {PROTOCOL_VERSION}. Update both to the same version."
                );
            }
            Ok(PeerInfo {
                id,
                name,
                os,
                app_version,
            })
        }
        Pair::Decision { .. } => bail!("unexpected pairing message"),
    }
}

fn code(conn: &Connection) -> Result<u32> {
    let mut material = [0u8; 4];
    conn.export_keying_material(&mut material, PAIRING_CODE_LABEL, b"")
        .map_err(|e| anyhow::anyhow!("deriving pairing code: {e:?}"))?;
    Ok(legato_proto::pairing_code(material))
}

pub(crate) async fn initiate(
    shared: Arc<Shared>,
    endpoint: &Endpoint,
    peer: EndpointAddr,
) -> Result<PairAttempt> {
    let conn = timeout(HANDSHAKE_TIMEOUT, endpoint.connect(peer, PAIR_ALPN))
        .await
        .context("timed out connecting to the other device")?
        .context("connecting to the other device")?;
    let result = async {
        let (mut send, mut recv) = conn.open_bi().await?;
        write_frame(&mut send, &our_hello(&shared)).await?;
        let theirs = timeout(HANDSHAKE_TIMEOUT, expect_frame::<Pair>(&mut recv))
            .await
            .context("timed out waiting for the other device")??;
        let peer = peer_info(conn.remote_id(), theirs)?;
        Ok::<_, anyhow::Error>((peer, send, recv))
    }
    .await;
    let (peer, send, recv) = match result {
        Ok(ok) => ok,
        Err(e) => {
            return Err(match conn.close_reason() {
                Some(reason) => anyhow::anyhow!("the other device refused: {reason}"),
                None => e,
            });
        }
    };
    Ok(PairAttempt {
        peer,
        code: code(&conn)?,
        incoming: false,
        shared,
        conn,
        send,
        recv,
        _busy: None,
    })
}

/// Handles incoming pairing connections.
#[derive(Debug, Clone)]
pub(crate) struct Handler(pub(crate) Arc<Shared>);

impl ProtocolHandler for Handler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let shared = &self.0;
        let Some(listener) = shared.pair_listener.lock().unwrap().clone() else {
            conn.close(1u32.into(), b"not in pairing mode");
            return Ok(());
        };
        if shared.pairing_busy.swap(true, Ordering::SeqCst) {
            conn.close(2u32.into(), b"busy pairing with another device");
            return Ok(());
        }
        let busy = BusyGuard(shared.clone());

        let result = async {
            let (mut send, mut recv) = timeout(HANDSHAKE_TIMEOUT, conn.accept_bi()).await??;
            let theirs = timeout(HANDSHAKE_TIMEOUT, expect_frame::<Pair>(&mut recv)).await??;
            let peer = peer_info(conn.remote_id(), theirs)?;
            write_frame(&mut send, &our_hello(shared)).await?;
            Ok::<_, anyhow::Error>((peer, send, recv))
        }
        .await;
        let (peer, send, recv) = match result {
            Ok(ok) => ok,
            Err(e) => {
                tracing::warn!("incoming pairing failed: {e:#}");
                conn.close(3u32.into(), b"pairing handshake failed");
                return Ok(());
            }
        };
        let code = match code(&conn) {
            Ok(code) => code,
            Err(e) => {
                tracing::warn!("incoming pairing failed: {e:#}");
                conn.close(3u32.into(), b"pairing handshake failed");
                return Ok(());
            }
        };
        let attempt = PairAttempt {
            peer,
            code,
            incoming: true,
            shared: shared.clone(),
            conn: conn.clone(),
            send,
            recv,
            _busy: Some(busy),
        };
        if listener.send(attempt).await.is_err() {
            conn.close(1u32.into(), b"not in pairing mode");
        }
        Ok(())
    }
}
