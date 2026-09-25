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
mod clipboard;
pub mod config;
pub mod extend;
pub mod files;
mod platform;
mod run;

pub use config::Config;
pub use extend::{Picture, ViewerFrame, ViewerStats};
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
    PeerScreens {
        id: EndpointId,
        screens: Screens,
    },
    /// This machine's keyboard and mouse are driving `id` (or nothing, `None`).
    Controlling(Option<EndpointId>),
    /// A peer is driving this machine (or nothing, `None`).
    ControlledBy(Option<EndpointId>),
    /// A peer arranged this machine (its desk origin in the peer's desk space).
    PeerPlacedUs {
        id: EndpointId,
        offset: Option<legato_proto::Point>,
    },
    FilesSending {
        to: EndpointId,
        count: usize,
    },
    FilesSent {
        to: EndpointId,
        bytes: u64,
    },
    /// A whole batch of files arrived.
    FilesReceived(files::Received),
    /// Virtual monitor mode: the Mac `id` added the display this machine asked for, at
    /// `bounds` in its coordinates (sent again if it moves).
    Extended {
        id: EndpointId,
        bounds: legato_proto::Rect,
    },
    /// The extra display from `id` went away; `reason` says why if it wasn't asked for.
    ExtendEnded {
        id: EndpointId,
        reason: String,
    },
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
    /// Held for the engine's lifetime: only one Legato (app or CLI) per state directory,
    /// since they share an identity.
    _lock: std::fs::File,
    dir: PathBuf,
    config: watch::Sender<Config>,
    status: broadcast::Sender<Status>,
    sharing: Mutex<Option<Sharing>>,
    known_screens: Mutex<HashMap<EndpointId, Screens>>,
    run_commands: Mutex<Option<tokio::sync::mpsc::UnboundedSender<run::RunCommand>>>,
    /// The latest picture of a Mac's extra display shown here, if any.
    viewer_frames: watch::Sender<Option<ViewerFrame>>,
    viewer_stats: watch::Sender<Option<ViewerStats>>,
}

impl Engine {
    /// Loads this machine's identity and settings and starts the network node.
    pub async fn start(home: Option<PathBuf>, app_version: &str) -> Result<Arc<Self>> {
        let dir = match home {
            Some(dir) => dir,
            None => legato_net::store::default_dir()?,
        };
        let lock = lock(&dir)?;
        let mut net_config = NetConfig::for_this_machine(app_version);
        net_config.store_dir = Some(dir.clone());
        let net = Net::start(net_config).await?;
        let config = config::load(&dir)?;
        let known_screens = load_known_screens(&dir);
        let (status, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            net,
            _lock: lock,
            dir,
            config: watch::Sender::new(config),
            status,
            sharing: Mutex::new(None),
            known_screens: Mutex::new(known_screens),
            run_commands: Mutex::new(None),
            viewer_frames: watch::Sender::new(None),
            viewer_stats: watch::Sender::new(None),
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

    /// Sends files and folders to a connected peer; they land in its Downloads folder.
    pub fn send_files(&self, to: EndpointId, paths: Vec<PathBuf>) -> Result<()> {
        self.run_command(run::RunCommand::SendFiles {
            to,
            paths,
            purpose: legato_proto::FilePurpose::Send,
        })
    }

    fn run_command(&self, command: run::RunCommand) -> Result<()> {
        let commands = self.run_commands.lock().unwrap();
        let Some(commands) = commands.as_ref() else {
            bail!("sharing isn't running");
        };
        commands
            .send(command)
            .map_err(|_| anyhow::anyhow!("sharing isn't running"))
    }

    /// Asks the Mac `to` for an extra display to show here, sized by the `[extend]`
    /// settings. Frames arrive on [`Engine::viewer_frames`].
    pub fn extend(&self, to: EndpointId) -> Result<()> {
        let request = self.config().extend.request();
        self.run_command(run::RunCommand::Extend { to, request })
    }

    /// Stops showing the extra display from `to`.
    pub fn stop_extend(&self, to: EndpointId) {
        let _ = self.run_command(run::RunCommand::StopExtend { to });
    }

    /// The window the extra display is drawn in (an `HWND` on Windows), so input over it
    /// goes to the Mac. `None` when it closes.
    pub fn set_viewer_window(&self, window: Option<u64>) {
        let _ = self.run_command(run::RunCommand::ViewerWindow(window));
    }

    /// Pictures of the extra display shown here: the latest, or `None` when there's none.
    pub fn viewer_frames(&self) -> watch::Receiver<Option<ViewerFrame>> {
        self.viewer_frames.subscribe()
    }

    /// How the extra display shown here is streaming, updated twice a second.
    pub fn viewer_stats(&self) -> watch::Receiver<Option<ViewerStats>> {
        self.viewer_stats.subscribe()
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
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        *self.run_commands.lock().unwrap() = Some(commands);
        let result = run::run(ctx, screens, events, command_rx, stop).await;
        *self.run_commands.lock().unwrap() = None;
        result
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

fn lock(dir: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("legato.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            bail!("Legato is already running (the app or `legato run`); quit it first")
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
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

/// "1.5 MB" and the like.
pub fn human_bytes(bytes: u64) -> String {
    let mut value = bytes as f64;
    for unit in ["bytes", "KB", "MB", "GB"] {
        if value < 1000.0 || unit == "GB" {
            return if unit == "bytes" {
                format!("{bytes} bytes")
            } else {
                format!("{value:.1} {unit}")
            };
        }
        value /= 1000.0;
    }
    unreachable!()
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
        Status::FilesSending { count, to } => {
            format!("Sending {count} item(s) to \"{}\"…", name_of(to))
        }
        Status::FilesSent { to, bytes } => {
            format!("Sent {} to \"{}\".", human_bytes(*bytes), name_of(to))
        }
        Status::FilesReceived(r) => format!(
            "Received {} item(s) from \"{}\"{}.",
            r.paths.len(),
            name_of(&r.from),
            match r.purpose {
                legato_proto::FilePurpose::Clipboard => ", ready to paste".to_string(),
                _ => format!(" in {}", files::downloads_dir().display()),
            }
        ),
        Status::ExtendEnded { id, reason } if !reason.is_empty() => {
            format!(
                "The extra display from \"{}\" closed: {reason}",
                name_of(id)
            )
        }
        Status::Controlling(None)
        | Status::ControlledBy(None)
        | Status::PeerScreens { .. }
        | Status::PeerPlacedUs { .. }
        | Status::Extended { .. }
        | Status::ExtendEnded { .. } => {
            return None;
        }
    })
}

#[allow(dead_code)]
fn assert_engine_is_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<Engine>();
}
