//! Legato's engine: pairing state, settings, and the sharing loop, shared by the `legato`
//! CLI and the Legato app.
//!
//! An [`Engine`] owns the network node for the life of the process. Sharing (capturing
//! this machine's input for paired peers, or letting them drive this one) can be started
//! and stopped independently, and follows settings changes live. Everything that happens
//! is reported as [`Status`] events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use legato_net::{EndpointId, Net, NetConfig, PathKind};
use legato_proto::{Os, Screens};
use tokio::sync::{broadcast, oneshot, watch};

pub mod arrange;
pub mod config;
#[cfg(target_os = "macos")]
mod receive;
#[cfg(windows)]
mod share;

pub use config::Config;
pub use legato_net;

const SCREENS_FILE: &str = "screens.json";

/// Something that happened, for display or logging.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Sharing started or stopped.
    Sharing(bool),
    PeerConnected {
        id: EndpointId,
        name: String,
        os: Os,
        path: Option<(PathKind, Duration)>,
    },
    PeerDisconnected {
        id: EndpointId,
        name: String,
        reason: String,
    },
    /// A connected peer's displays (also on connect), e.g. for the arrangement editor.
    PeerScreens { id: EndpointId, screens: Screens },
    /// This machine's keyboard and mouse are driving `id` (or nothing, `None`).
    Controlling(Option<EndpointId>),
    /// A peer is driving this machine (or nothing, `None`).
    ControlledBy(Option<EndpointId>),
    /// A settings problem worth showing, e.g. a peer with no position.
    Problem(String),
    /// Sharing stopped because of an error.
    Error(String),
}

struct Sharing {
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

pub struct Engine {
    net: Net,
    dir: PathBuf,
    config: watch::Sender<Config>,
    status: broadcast::Sender<Status>,
    sharing: Mutex<Option<Sharing>>,
    known_screens: Mutex<HashMap<EndpointId, Screens>>,
}

impl Engine {
    /// Loads this machine's identity and settings and starts the network node.
    pub async fn start(home: Option<PathBuf>, app_version: &str) -> Result<Arc<Self>> {
        let mut net_config = NetConfig::for_this_machine(app_version);
        net_config.store_dir = home;
        let net = Net::start(net_config).await?;
        let dir = net.store().dir().to_path_buf();
        let config = config::load(&dir)?;
        let known_screens = load_known_screens(&dir);
        let (status, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            net,
            dir,
            config: watch::Sender::new(config),
            status,
            sharing: Mutex::new(None),
            known_screens: Mutex::new(known_screens),
        }))
    }

    pub fn net(&self) -> &Net {
        &self.net
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn config(&self) -> Config {
        self.config.borrow().clone()
    }

    /// Changes settings, saves them, and applies them to a running session.
    pub fn update_config(&self, change: impl FnOnce(&mut Config)) -> Result<()> {
        let mut config = self.config();
        change(&mut config);
        config::save(&self.dir, &config)?;
        self.config.send_replace(config);
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Status> {
        self.status.subscribe()
    }

    /// The last displays seen from each paired peer, so they can be arranged while offline.
    pub fn known_screens(&self) -> HashMap<EndpointId, Screens> {
        self.known_screens.lock().unwrap().clone()
    }

    pub fn is_sharing(&self) -> bool {
        self.sharing
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| !s.task.is_finished())
    }

    /// Starts sharing with all paired peers. Does nothing if already sharing.
    pub fn start_sharing(self: &Arc<Self>) -> Result<()> {
        let mut sharing = self.sharing.lock().unwrap();
        if sharing.as_ref().is_some_and(|s| !s.task.is_finished()) {
            return Ok(());
        }
        if self.net.paired_peers().is_empty() {
            bail!("no paired devices yet");
        }
        let (stop, stop_rx) = oneshot::channel();
        let engine = self.clone();
        let task = tokio::spawn(async move {
            let _ = engine.status.send(Status::Sharing(true));
            if let Err(e) = engine.share(stop_rx).await {
                tracing::error!("sharing stopped: {e:#}");
                let _ = engine.status.send(Status::Error(format!("{e:#}")));
            }
            engine.net.stop_sessions();
            let _ = engine.status.send(Status::Sharing(false));
        });
        *sharing = Some(Sharing { stop, task });
        Ok(())
    }

    /// Stops sharing and waits until input is back to normal.
    pub async fn stop_sharing(&self) {
        let sharing = self.sharing.lock().unwrap().take();
        if let Some(sharing) = sharing {
            let _ = sharing.stop.send(());
            let _ = sharing.task.await;
        }
    }

    /// Restarts sharing if it's running, e.g. so newly paired peers are included.
    pub async fn restart_sharing(self: &Arc<Self>) -> Result<()> {
        if self.is_sharing() {
            self.stop_sharing().await;
            self.start_sharing()?;
        }
        Ok(())
    }

    async fn share(self: &Arc<Self>, stop: oneshot::Receiver<()>) -> Result<()> {
        let screens = local_screens();
        let hello = legato_proto::Hello {
            protocol: legato_proto::PROTOCOL_VERSION,
            app_version: self.net.config().app_version.clone(),
            name: self.net.config().name.clone(),
            os: this_os(),
            screens: screens.clone(),
        };
        let events = self.net.start_sessions(hello);
        let ctx = Ctx {
            engine: self.clone(),
            config: self.config.subscribe(),
        };
        #[cfg(windows)]
        return share::run(ctx, screens, events, stop).await;
        #[cfg(target_os = "macos")]
        {
            let _ = screens;
            receive::run(ctx, events, stop).await
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = (ctx, screens, events, stop);
            bail!("sharing isn't supported on this platform");
        }
    }

    fn remember_screens(&self, id: EndpointId, screens: &Screens) {
        let mut known = self.known_screens.lock().unwrap();
        if known.get(&id) == Some(screens) {
            return;
        }
        known.insert(id, screens.clone());
        let by_id: HashMap<String, &Screens> =
            known.iter().map(|(k, v)| (k.to_string(), v)).collect();
        if let Ok(json) = serde_json::to_vec_pretty(&by_id)
            && let Err(e) = std::fs::write(self.dir.join(SCREENS_FILE), json)
        {
            tracing::warn!("couldn't save peer displays: {e}");
        }
    }
}

/// What the platform sharing loops get from the engine.
pub(crate) struct Ctx {
    pub(crate) engine: Arc<Engine>,
    pub(crate) config: watch::Receiver<Config>,
}

impl Ctx {
    pub(crate) fn status(&self, status: Status) {
        if let Status::PeerScreens { id, screens } = &status {
            self.engine.remember_screens(*id, screens);
        }
        let _ = self.engine.status.send(status);
    }
}

fn load_known_screens(dir: &Path) -> HashMap<EndpointId, Screens> {
    let Ok(bytes) = std::fs::read(dir.join(SCREENS_FILE)) else {
        return HashMap::new();
    };
    let by_id: HashMap<String, Screens> = serde_json::from_slice(&bytes).unwrap_or_default();
    by_id
        .into_iter()
        .filter_map(|(k, v)| Some((k.parse().ok()?, v)))
        .collect()
}

/// This machine's displays.
pub fn local_screens() -> Screens {
    #[cfg(target_os = "macos")]
    return legato_macos::screens();
    #[cfg(windows)]
    return legato_windows::screens();
    #[cfg(not(any(target_os = "macos", windows)))]
    Screens {
        displays: vec![],
        native_per_desk: 1.0,
    }
}

pub fn this_os() -> Os {
    if cfg!(target_os = "macos") {
        Os::MacOs
    } else {
        Os::Windows
    }
}

/// " (direct, 1.2 ms)" or " (via relay, 60 ms: …)", for messages.
pub fn describe_path(path: Option<(PathKind, Duration)>) -> String {
    match path {
        Some((PathKind::Direct, rtt)) => format!(" (direct, {:.1} ms)", rtt.as_secs_f64() * 1000.0),
        Some((PathKind::Relay, rtt)) => format!(
            " (via relay, {:.0} ms: expect lag until a direct path is found)",
            rtt.as_secs_f64() * 1000.0
        ),
        None => String::new(),
    }
}

/// Human-readable text for a status event, for logs and the CLI.
pub fn describe(status: &Status, name_of: impl Fn(&EndpointId) -> String) -> Option<String> {
    Some(match status {
        Status::Sharing(true) => "Sharing started; waiting for paired devices.".into(),
        Status::Sharing(false) => "Sharing stopped.".into(),
        Status::PeerConnected { name, path, .. } => {
            format!("Connected to \"{name}\"{}.", describe_path(*path))
        }
        Status::PeerDisconnected { name, reason, .. } => {
            format!("Disconnected from \"{name}\": {reason}")
        }
        Status::Problem(p) => p.clone(),
        Status::Error(e) => format!("error: {e}"),
        Status::Controlling(Some(id)) => format!("Controlling \"{}\".", name_of(id)),
        Status::ControlledBy(Some(id)) => {
            format!("\"{}\" is controlling this machine.", name_of(id))
        }
        Status::Controlling(None) | Status::ControlledBy(None) | Status::PeerScreens { .. } => {
            return None;
        }
    })
}

#[allow(dead_code)]
fn assert_engine_is_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<Engine>();
}
