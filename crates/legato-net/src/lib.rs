//! Legato networking over iroh.
//!
//! - **Identity**: each machine has an ed25519 key (its [`EndpointId`]); connections are
//!   TLS 1.3 authenticated against it and end-to-end encrypted, including via relays.
//! - **Discovery**: machines advertise themselves on the local network with mDNS, so
//!   unpaired devices can be listed without copying any addresses around.
//! - **Pairing**: an open ALPN on which both users confirm a 6-digit code derived from
//!   the TLS session. Paired ids go on an allow-list.
//! - **Sessions**: an allow-listed ALPN carrying the input-sharing protocol. Paired peers
//!   connect over the LAN when possible and via relays otherwise.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{
    AfterHandshakeOutcome, Connection, EndpointHooks, PortmapperConfig, QuicTransportConfig,
    presets,
};
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr};
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use legato_proto::{Os, PAIR_ALPN, SESSION_ALPN};
use n0_future::{Stream, StreamExt};
use tokio::sync::mpsc;

mod framing;
pub mod pairing;
pub mod session;
pub mod store;

pub use iroh::EndpointId;
pub use pairing::{PairAttempt, PairOutcome, PeerInfo};
pub use session::{IncomingFile, PathKind, Session, SessionEvent};
pub use store::{PairedPeer, Store};

/// mDNS service name; only Legato instances see each other.
const MDNS_SERVICE: &str = "legato";
/// Prefix of the mDNS user data, so other data on the same service is ignored.
const ADVERT_PREFIX: &str = "legato1";

#[derive(Debug, Clone)]
pub struct NetConfig {
    /// State directory (identity and paired peers). Defaults to [`store::default_dir`].
    pub store_dir: Option<PathBuf>,
    /// Human-readable name shown to other machines.
    pub name: String,
    pub os: Os,
    pub app_version: String,
    /// Use relays and DNS lookup so paired peers can connect across networks.
    pub relays: bool,
    /// Advertise and discover on the local network.
    pub mdns: bool,
}

impl NetConfig {
    /// Defaults for this machine: its device name and OS, relays and mDNS on.
    pub fn for_this_machine(app_version: impl Into<String>) -> Self {
        Self {
            store_dir: None,
            name: whoami::devicename().unwrap_or_else(|_| "Unnamed device".into()),
            os: if cfg!(target_os = "macos") {
                Os::MacOs
            } else {
                Os::Windows
            },
            app_version: app_version.into(),
            relays: true,
            mdns: true,
        }
    }
}

/// A Legato machine seen on the local network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nearby {
    pub id: EndpointId,
    pub name: String,
    pub os: Option<Os>,
    pub paired: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NearbyEvent {
    Found(Nearby),
    Lost(EndpointId),
}

/// State shared with the protocol handlers and hooks.
#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) config: NetConfig,
    pub(crate) store: Store,
    /// Paired peers allowed to open sessions.
    pub(crate) allowed: RwLock<HashSet<EndpointId>>,
    /// Where incoming pairing requests go while the user is pairing.
    pub(crate) pair_listener: Mutex<Option<mpsc::Sender<PairAttempt>>>,
    /// Only one pairing at a time.
    pub(crate) pairing_busy: AtomicBool,
    pub(crate) sessions: Mutex<Option<session::Hub>>,
    pub(crate) endpoint: OnceLock<Endpoint>,
}

impl Shared {
    pub(crate) fn add_paired(self: &Arc<Self>, peer: &PeerInfo) -> Result<PairedPeer> {
        let paired = self.store.add_peer(peer.id, peer.name.clone(), peer.os)?;
        self.allowed.write().unwrap().insert(peer.id);
        // If sessions are running, include the new peer right away.
        if let Some(endpoint) = self.endpoint.get() {
            session::add_peer(self, endpoint, peer.id);
        }
        Ok(paired)
    }
}

/// Lets anyone attempt pairing, but only paired peers open sessions.
#[derive(Debug)]
struct PairedOnly(Arc<Shared>);

impl EndpointHooks for PairedOnly {
    async fn after_handshake<'a>(&'a self, conn: &'a Connection) -> AfterHandshakeOutcome {
        let alpn = conn.alpn();
        if alpn == PAIR_ALPN || self.0.allowed.read().unwrap().contains(&conn.remote_id()) {
            AfterHandshakeOutcome::Accept
        } else {
            AfterHandshakeOutcome::Reject {
                error_code: 403u32.into(),
                reason: b"not paired".to_vec(),
            }
        }
    }
}

/// A running Legato network node.
#[derive(Debug, Clone)]
pub struct Net {
    shared: Arc<Shared>,
    endpoint: Endpoint,
    router: Router,
    mdns: Option<MdnsAddressLookup>,
    /// Addresses learned out of band (tests, "add by address").
    memory: MemoryLookup,
}

impl Net {
    /// Loads (or creates) this machine's identity and starts listening.
    pub async fn start(config: NetConfig) -> Result<Self> {
        let dir = match &config.store_dir {
            Some(dir) => dir.clone(),
            None => store::default_dir()?,
        };
        let store = Store::open(dir)?;
        let secret = store.identity()?;
        let id = secret.public();
        let allowed = store.peer_ids();
        let shared = Arc::new(Shared {
            config: config.clone(),
            store,
            allowed: RwLock::new(allowed),
            pair_listener: Mutex::new(None),
            pairing_busy: AtomicBool::new(false),
            sessions: Mutex::new(None),
            endpoint: OnceLock::new(),
        });

        let transport = QuicTransportConfig::builder()
            .keep_alive_interval(Duration::from_secs(1))
            .max_idle_timeout(Some(Duration::from_secs(5).try_into()?))
            .build();
        let mut builder = if config.relays {
            Endpoint::builder(presets::N0)
        } else {
            Endpoint::builder(presets::Minimal)
        }
        .secret_key(secret)
        .transport_config(transport)
        // UPnP/NAT-PMP probing triggers firewall prompts and isn't needed on a LAN.
        .portmapper_config(PortmapperConfig::Disabled)
        .hooks(PairedOnly(shared.clone()))
        .alpns(vec![SESSION_ALPN.to_vec(), PAIR_ALPN.to_vec()]);

        let mdns = if config.mdns {
            let mdns = MdnsAddressLookup::builder()
                .service_name(MDNS_SERVICE)
                .build(id)
                .context("starting local network discovery")?;
            builder = builder.address_lookup(mdns.clone());
            Some(mdns)
        } else {
            None
        };

        let memory = MemoryLookup::new();
        builder = builder.address_lookup(memory.clone());

        let endpoint = builder.bind().await.context("binding network endpoint")?;
        let _ = shared.endpoint.set(endpoint.clone());
        endpoint.set_user_data_for_address_lookup(advert(&config).parse().ok());

        let router = Router::builder(endpoint.clone())
            .accept(PAIR_ALPN, pairing::Handler(shared.clone()))
            .accept(SESSION_ALPN, session::Handler(shared.clone()))
            .spawn();

        Ok(Self {
            shared,
            endpoint,
            router,
            mdns,
            memory,
        })
    }

    /// Remembers where a peer can be reached, for when discovery can't find it.
    pub fn remember(&self, addr: EndpointAddr) {
        self.memory.add_endpoint_info(addr);
    }

    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// This machine's current addresses (direct and relay).
    pub fn addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    pub fn config(&self) -> &NetConfig {
        &self.shared.config
    }

    pub fn store(&self) -> &Store {
        &self.shared.store
    }

    pub fn paired_peers(&self) -> Vec<PairedPeer> {
        self.shared.store.peers()
    }

    /// Forgets a paired peer and drops any session with it.
    pub fn unpair(&self, id: &EndpointId) -> Result<bool> {
        self.shared.allowed.write().unwrap().remove(id);
        if let Some(hub) = self.shared.sessions.lock().unwrap().as_ref() {
            hub.close(id);
        }
        self.shared.store.remove_peer(id)
    }

    /// Legato machines on the local network, as they come and go.
    pub async fn nearby(&self) -> Result<impl Stream<Item = NearbyEvent> + Unpin + use<>> {
        let mdns = self
            .mdns
            .as_ref()
            .context("local network discovery is disabled")?;
        let own = self.id();
        let shared = self.shared.clone();
        let events = mdns.subscribe().await;
        Ok(events.filter_map(move |event| match event {
            DiscoveryEvent::Discovered { endpoint_info, .. } => {
                let id = endpoint_info.endpoint_id;
                let (os, name) = parse_advert(endpoint_info.user_data()?.as_ref())?;
                (id != own).then(|| {
                    NearbyEvent::Found(Nearby {
                        id,
                        name,
                        os,
                        paired: shared.allowed.read().unwrap().contains(&id),
                    })
                })
            }
            DiscoveryEvent::Expired { endpoint_id } => Some(NearbyEvent::Lost(endpoint_id)),
            _ => None,
        }))
    }

    /// Starts pairing with a peer. Show [`PairAttempt::code`] to the user, then call
    /// [`PairAttempt::decide`] with their answer.
    pub async fn pair(&self, peer: impl Into<EndpointAddr>) -> Result<PairAttempt> {
        pairing::initiate(self.shared.clone(), &self.endpoint, peer.into()).await
    }

    /// Accepts incoming pairing requests until the returned receiver is dropped.
    pub fn listen_for_pairing(&self) -> mpsc::Receiver<PairAttempt> {
        let (tx, rx) = mpsc::channel(1);
        *self.shared.pair_listener.lock().unwrap() = Some(tx);
        rx
    }

    /// Starts sessions with all paired peers, announcing `hello` (which must describe this
    /// machine's current screens). Events for all sessions arrive on the returned channel.
    pub fn start_sessions(
        &self,
        hello: legato_proto::Hello,
    ) -> mpsc::UnboundedReceiver<SessionEvent> {
        session::start(self.shared.clone(), self.endpoint.clone(), hello)
    }

    /// Stops all sessions (reconnecting stops too). Pairing keeps working.
    pub fn stop_sessions(&self) {
        if let Some(hub) = self.shared.sessions.lock().unwrap().take() {
            hub.close_all();
        }
    }

    /// Updates the screens announced to peers, and tells connected peers.
    pub fn update_screens(&self, screens: legato_proto::Screens) {
        if let Some(hub) = self.shared.sessions.lock().unwrap().as_mut() {
            hub.update_screens(screens);
        }
    }

    /// Closes all connections and stops listening.
    pub async fn shutdown(self) {
        if let Some(hub) = self.shared.sessions.lock().unwrap().take() {
            hub.close_all();
        }
        if let Err(e) = self.router.shutdown().await {
            tracing::debug!("router shutdown: {e:#}");
        }
    }
}

fn advert(config: &NetConfig) -> String {
    let os = match config.os {
        Os::MacOs => "mac",
        Os::Windows => "win",
    };
    let mut advert = format!("{ADVERT_PREFIX}|{os}|{}", config.name);
    // mDNS user data is limited to 245 bytes.
    while advert.len() > 245 {
        advert.pop();
    }
    advert
}

fn parse_advert(advert: &str) -> Option<(Option<Os>, String)> {
    let mut parts = advert.splitn(3, '|');
    if parts.next()? != ADVERT_PREFIX {
        return None;
    }
    let os = match parts.next()? {
        "mac" => Some(Os::MacOs),
        "win" => Some(Os::Windows),
        _ => None,
    };
    Some((os, parts.next().unwrap_or_default().to_string()))
}

#[cfg(test)]
mod tests;
