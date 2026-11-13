//! The C ABI behind the native macOS app (`native/macos`).
//!
//! Swift reads state as a JSON [`Snapshot`] and sends JSON [`Command`]s. Rust calls the
//! observer registered with [`legato_set_observer`] when the snapshot may have changed;
//! calls are coalesced until Swift takes the next snapshot. There is one engine per
//! process. Swift must free every returned string exactly once with [`legato_free`].
#![cfg(target_os = "macos")]

use anyhow::{Context, Result, bail};
use legato_engine::{
    Engine, Status,
    arrange::{self, Editor, EditorPeer},
    config::{Config, Remap},
};
use legato_net::{EndpointId, Nearby, NearbyEvent, PairAttempt, PairOutcome};
use legato_proto::{FilePurpose, Os, Point, Rect};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// The most notices kept for the log view; the window shows the latest.
const MAX_NOTICES: usize = 50;

// ---- What Swift reads ----

#[derive(Debug, Default, Serialize)]
pub struct Snapshot {
    pub ready: bool,
    /// Why the engine isn't running, if it failed to start.
    pub error: Option<String>,
    pub version: String,
    pub id: String,
    pub name: String,
    pub state_dir: String,
    pub sharing: bool,
    pub settings: Settings,
    pub paired: Vec<Device>,
    pub nearby: Vec<Device>,
    pub pairing: Option<Pairing>,
    pub notice: Option<Notice>,
    pub failures: Vec<String>,
    pub reveal: Option<Reveal>,
    pub arrangement: Arrangement,
}

#[derive(Debug, Default, Serialize)]
pub struct Settings {
    pub clipboard: bool,
    /// Alt is Command and Win is Option when driving a Mac.
    pub remap: bool,
    pub invert_wheel: bool,
    pub push_distance: f64,
    /// The configured controller's id or id prefix, if one is set.
    pub controller: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Shown to people: "macOS" or "Windows".
    pub os: Option<&'static str>,
    pub connected: bool,
    /// How it's connected, e.g. "direct · 2 ms".
    pub path: Option<String>,
    /// This device's input is driving it, or its input is driving this device.
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Reaching the other device.
    Connecting,
    /// Showing the code; waiting for this side to decide.
    Confirm,
    /// Accepted here; waiting for the other side.
    Waiting,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pairing {
    pub id: String,
    pub name: String,
    /// Six digits, or empty while connecting.
    pub code: String,
    pub incoming: bool,
    pub stage: Stage,
}

#[derive(Debug, Clone, Serialize)]
pub struct Notice {
    /// Increases with every notice, so the same text twice still shows.
    pub id: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Reveal {
    /// Increases with every drop received, so Swift reveals each once.
    pub sequence: u64,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Default, Serialize)]
pub struct Arrangement {
    pub local: Vec<Rect>,
    pub local_primary: Option<usize>,
    pub peers: Vec<ArrangedPeer>,
    /// Why devices can't be arranged or reached yet.
    pub problems: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ArrangedPeer {
    pub id: String,
    pub name: String,
    pub placed: bool,
    pub displays: Vec<Rect>,
    pub primary: Option<usize>,
}

fn os_name(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macOS",
        Os::Windows => "Windows",
    }
}

// ---- What Swift sends ----

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Sharing {
        enabled: bool,
    },
    Pair {
        id: EndpointId,
    },
    ConfirmPair {
        accept: bool,
    },
    Unpair {
        id: EndpointId,
    },
    Setting {
        key: String,
        value: Value,
    },
    /// Drop a peer's displays with their top-left here (desk units); it snaps into place.
    Place {
        id: EndpointId,
        x: f64,
        y: f64,
    },
    SendFiles {
        id: EndpointId,
        paths: Vec<PathBuf>,
    },
    DismissFailure {
        index: usize,
    },
    DismissNotice,
}

// ---- State ----

#[derive(Default)]
struct State {
    connections: HashMap<EndpointId, String>,
    nearby: HashMap<EndpointId, Nearby>,
    placed_us: HashMap<EndpointId, Point>,
    active: Option<EndpointId>,
    pairing: Option<Pairing>,
    attempt: Option<PairAttempt>,
    pairing_generation: u64,
    pairing_task: Option<tokio::task::AbortHandle>,
    notices: Vec<Notice>,
    next_notice: u64,
    failures: Vec<String>,
    reveal: Option<Reveal>,
}

impl State {
    fn cancel_pairing(&mut self) -> Option<PairAttempt> {
        self.pairing_generation += 1;
        if let Some(task) = self.pairing_task.take() {
            task.abort();
        }
        self.pairing = None;
        self.attempt.take()
    }

    fn notify(&mut self, text: String) {
        tracing::info!("{text}");
        if self.notices.len() == MAX_NOTICES {
            self.notices.remove(0);
        }
        self.next_notice += 1;
        self.notices.push(Notice {
            id: self.next_notice,
            text,
        });
    }

    fn failed(&mut self, text: String) {
        tracing::warn!("{text}");
        if !self.failures.contains(&text) {
            self.failures.push(text);
        }
    }
}

type Shared = Arc<Mutex<State>>;

struct Bridge {
    engine: Arc<Engine>,
    state: Shared,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);
static START_ERROR: Mutex<Option<String>> = Mutex::new(None);
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static OBSERVER: Mutex<Option<extern "C" fn()>> = Mutex::new(None);
/// Swift has been told about a change it hasn't taken a snapshot for yet.
static PENDING: AtomicBool = AtomicBool::new(false);

fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().expect("Tokio runtime"))
}

/// Locks, ignoring poisoning: one panicked call must not take the app down with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Tells Swift the snapshot may have changed, once until it takes the next one.
fn changed() {
    if !PENDING.swap(true, Ordering::AcqRel)
        && let Some(observer) = *lock(&OBSERVER)
    {
        observer();
    }
}

fn encoded(value: &impl Serialize) -> *mut c_char {
    let json = serde_json::to_string(value).unwrap_or_else(|e| {
        serde_json::json!({"ok": false, "error": format!("encoding: {e}")}).to_string()
    });
    CString::new(json).expect("JSON escapes NUL").into_raw()
}

fn name_of(engine: &Engine, id: &EndpointId) -> String {
    engine
        .net()
        .paired_peers()
        .into_iter()
        .find(|p| p.id == *id)
        .map(|p| p.name)
        .unwrap_or_else(|| id.fmt_short().to_string())
}

fn init_logging() {
    static LOGGING: OnceLock<()> = OnceLock::new();
    LOGGING.get_or_init(|| {
        let file = legato_net::store::default_dir().ok().and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            // Keep the last run's log: after a crash, it's the one that says why.
            let path = dir.join("legato-app.log");
            let _ = std::fs::rename(&path, dir.join("legato-app.previous.log"));
            std::fs::File::create(path).ok()
        });
        let filter =
            tracing_subscriber::EnvFilter::try_from_env("LEGATO_LOG").unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "error,legato=info,legato_engine=info,legato_net=info,legato_swift_bridge=info",
                )
            });
        if let Some(file) = file {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(Mutex::new(file))
                .try_init();
        }
        // The app has no console, so a crash would otherwise leave no trace.
        let report = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            tracing::error!(
                "crashed on thread {}: {info}",
                thread.name().unwrap_or("unnamed")
            );
            report(info);
        }));
    });
}

// ---- Engine events ----

fn on_status(engine: &Engine, state: &Shared, status: Status) {
    if let Some(text) = legato_engine::describe(&status, |id| name_of(engine, id)) {
        tracing::info!("{text}");
    }
    let mut s = lock(state);
    match status {
        Status::Sharing(on) => {
            if !on {
                s.connections.clear();
                s.active = None;
            }
        }
        Status::PeerConnected { id, path, .. } => {
            s.connections
                .insert(id, legato_engine::describe_path(path).trim().to_string());
        }
        Status::PeerDisconnected { id, .. } => {
            s.connections.remove(&id);
            if s.active == Some(id) {
                s.active = None;
            }
        }
        Status::Controlling(id) | Status::ControlledBy(id) => s.active = id,
        Status::PeerPlacedUs { id, offset } => match offset {
            Some(offset) => {
                s.placed_us.insert(id, offset);
            }
            None => {
                s.placed_us.remove(&id);
            }
        },
        Status::Problem(problem) => {
            // Layout problems are recomputed with every snapshot; show anything else.
            let layout = editor(engine, &s).problems;
            if !layout.contains(&problem) {
                s.notify(problem);
            }
        }
        Status::Error(e) => s.notify(format!("Sharing stopped: {e}")),
        Status::FilesSending { to, count } => {
            let name = name_of(engine, &to);
            s.notify(format!("Sending {count} item(s) to \"{name}\"…"));
        }
        Status::FilesSent { to, bytes } => {
            let name = name_of(engine, &to);
            let size = legato_engine::human_bytes(bytes);
            s.notify(format!("Sent {size} to \"{name}\"."));
        }
        ref failed @ Status::FileTransferFailed { .. } => {
            if let Some(text) = legato_engine::describe(failed, |id| name_of(engine, id)) {
                s.failed(text);
            }
        }
        Status::FilesReceived(received) => {
            let name = name_of(engine, &received.from);
            if received.purpose == FilePurpose::Clipboard {
                s.notify(format!("Files copied on \"{name}\" are ready to paste."));
            } else {
                let count = received.paths.len();
                s.notify(format!(
                    "Received {count} item(s) from \"{name}\" in Downloads/Legato."
                ));
                if received.purpose == FilePurpose::Drop {
                    let sequence = s.reveal.as_ref().map_or(1, |r| r.sequence + 1);
                    s.reveal = Some(Reveal {
                        sequence,
                        paths: received.paths,
                    });
                }
            }
        }
        // Viewer windows are Windows-only, and screens arrive through `known_screens`.
        Status::PeerScreens { .. } | Status::Extended { .. } | Status::ExtendEnded { .. } => {}
    }
}

fn install_attempt(state: &Shared, attempt: PairAttempt, generation: Option<u64>) {
    let mut s = lock(state);
    let stale = match generation {
        // Our own request: only if it wasn't cancelled meanwhile.
        Some(g) => g != s.pairing_generation,
        // Someone else's: only if nothing else is in progress.
        None => s.pairing.is_some(),
    };
    if stale || s.attempt.is_some() {
        drop(s);
        runtime().spawn(async move {
            let _ = attempt.decide(false).await;
        });
        return;
    }
    if generation.is_none() {
        s.pairing_generation += 1;
    }
    s.pairing = Some(Pairing {
        id: attempt.peer.id.to_string(),
        name: attempt.peer.name.clone(),
        code: format!("{:06}", attempt.code),
        incoming: attempt.incoming,
        stage: Stage::Confirm,
    });
    s.attempt = Some(attempt);
    drop(s);
    changed();
}

/// Starts networking, discovery and pairing, and resumes sharing with paired devices.
/// Returns false if the engine couldn't start; the snapshot's `error` says why. It can
/// be called again to retry. It does not ask for any macOS permission.
#[unsafe(no_mangle)]
pub extern "C" fn legato_start() -> bool {
    catch_unwind(start).unwrap_or_else(|_| {
        *lock(&START_ERROR) = Some("Legato failed to start. See the log for details.".into());
        false
    })
}

fn start() -> bool {
    init_logging();
    if lock(&BRIDGE).is_some() {
        return true;
    }
    let engine = match runtime().block_on(Engine::start(None, env!("CARGO_PKG_VERSION"))) {
        Ok(engine) => engine,
        Err(e) => {
            tracing::error!("couldn't start: {e:#}");
            *lock(&START_ERROR) = Some(format!("{e:#}"));
            changed();
            return false;
        }
    };
    *lock(&START_ERROR) = None;
    let state = Shared::default();
    let mut tasks = Vec::new();

    let mut statuses = engine.subscribe();
    let (e, s) = (engine.clone(), state.clone());
    tasks.push(runtime().spawn(async move {
        loop {
            match statuses.recv().await {
                Ok(status) => on_status(&e, &s, status),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("skipped {n} status updates");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
            changed();
        }
    }));

    // Settings and this Mac's displays change outside of status updates.
    let mut config = engine.config_updates();
    let mut screens = engine.screen_updates();
    tasks.push(runtime().spawn(async move {
        loop {
            tokio::select! {
                r = config.changed() => if r.is_err() { break },
                r = screens.changed() => if r.is_err() { break },
            }
            changed();
        }
    }));

    let (e, s) = (engine.clone(), state.clone());
    tasks.push(runtime().spawn(async move {
        match e.net().nearby().await {
            Ok(mut events) => {
                while let Some(event) = events.next().await {
                    let mut state = lock(&s);
                    match event {
                        NearbyEvent::Found(peer) => {
                            state.nearby.insert(peer.id, peer);
                        }
                        NearbyEvent::Lost(id) => {
                            state.nearby.remove(&id);
                        }
                    }
                    drop(state);
                    changed();
                }
            }
            Err(e) => {
                lock(&s).notify(format!("Couldn't look for nearby devices: {e:#}"));
                changed();
            }
        }
    }));

    let mut incoming = engine.net().listen_for_pairing();
    let s = state.clone();
    tasks.push(runtime().spawn(async move {
        while let Some(attempt) = incoming.recv().await {
            install_attempt(&s, attempt, None);
        }
    }));

    // Like the Iced app: resume sharing with already paired devices.
    if !engine.net().paired_peers().is_empty() {
        let _entered = runtime().enter();
        if let Err(e) = engine.start_sharing() {
            lock(&state).notify(format!("{e:#}"));
        }
    }
    *lock(&BRIDGE) = Some(Bridge {
        engine,
        state,
        tasks,
    });
    changed();
    true
}

/// Registers the function to call when the snapshot may have changed, from any thread.
/// It should schedule [`legato_snapshot`] and return quickly. Null removes it.
#[unsafe(no_mangle)]
pub extern "C" fn legato_set_observer(observer: Option<extern "C" fn()>) {
    *lock(&OBSERVER) = observer;
    PENDING.store(false, Ordering::Release);
}

// ---- Snapshots ----

fn editor(engine: &Engine, state: &State) -> Editor {
    let peers = engine.net().paired_peers();
    let known = engine.known_screens();
    let peers: Vec<_> = peers
        .iter()
        .filter_map(|p| {
            Some(EditorPeer {
                id: p.id,
                name: &p.name,
                screens: known.get(&p.id).filter(|s| !s.displays.is_empty())?,
                placed_us_at: state.placed_us.get(&p.id).copied(),
            })
        })
        .collect();
    Editor::new(&engine.screen_updates().borrow(), &peers, &engine.config())
}

fn settings(config: &Config) -> Settings {
    Settings {
        clipboard: config.clipboard.enabled,
        remap: config.keys.remap == Remap::Auto,
        invert_wheel: config.scrolling.invert_wheel,
        push_distance: config.switching.push_distance,
        controller: config.control.controller.clone(),
    }
}

fn snapshot(engine: &Engine, state: &State) -> Snapshot {
    let paired = engine.net().paired_peers();
    let known = engine.known_screens();
    let editor = editor(engine, state);
    let mut problems = editor.problems;
    for p in &paired {
        if known.get(&p.id).is_none_or(|s| s.displays.is_empty()) {
            problems.push(format!(
                "Waiting to connect to \"{}\" once to learn its displays.",
                p.name
            ));
        }
    }
    let mut nearby: Vec<_> = state
        .nearby
        .values()
        .filter(|n| !paired.iter().any(|p| p.id == n.id))
        .map(|n| Device {
            id: n.id.to_string(),
            name: n.name.clone(),
            os: n.os.map(os_name),
            connected: false,
            path: None,
            active: false,
        })
        .collect();
    nearby.sort_by(|a, b| a.name.cmp(&b.name));
    Snapshot {
        ready: true,
        error: None,
        version: env!("CARGO_PKG_VERSION").into(),
        id: engine.net().id().to_string(),
        name: engine.net().config().name.clone(),
        state_dir: engine.dir().to_string_lossy().into_owned(),
        sharing: engine.is_sharing(),
        settings: settings(&engine.config()),
        paired: paired
            .iter()
            .map(|p| Device {
                id: p.id.to_string(),
                name: p.name.clone(),
                os: Some(os_name(p.os)),
                connected: state.connections.contains_key(&p.id),
                path: state.connections.get(&p.id).cloned(),
                active: state.active == Some(p.id),
            })
            .collect(),
        nearby,
        pairing: state.pairing.clone(),
        notice: state.notices.last().cloned(),
        failures: state.failures.clone(),
        reveal: state.reveal.clone(),
        arrangement: Arrangement {
            local: editor.local,
            local_primary: editor.primary,
            peers: editor
                .peers
                .into_iter()
                .map(|p| ArrangedPeer {
                    id: p.id.to_string(),
                    name: p.name,
                    placed: p.placed,
                    displays: p.displays,
                    primary: p.primary,
                })
                .collect(),
            problems,
        },
    }
}

/// Returns a newly allocated JSON [`Snapshot`]. Free it using [`legato_free`].
#[unsafe(no_mangle)]
pub extern "C" fn legato_snapshot() -> *mut c_char {
    // Taken before reading, so a change while reading notifies again.
    PENDING.store(false, Ordering::Release);
    let result = catch_unwind(|| {
        let (engine, state) = match lock(&BRIDGE).as_ref() {
            Some(b) => (b.engine.clone(), b.state.clone()),
            None => {
                return Snapshot {
                    error: lock(&START_ERROR).clone(),
                    ..Snapshot::default()
                };
            }
        };
        let state = lock(&state);
        snapshot(&engine, &state)
    });
    match result {
        Ok(snapshot) => encoded(&snapshot),
        Err(_) => encoded(&Snapshot {
            error: Some("Legato couldn't read its state. See the log for details.".into()),
            ..Snapshot::default()
        }),
    }
}

// ---- Commands ----

fn setting(config: &mut Config, key: &str, value: Value) -> Result<()> {
    match key {
        "clipboard" => config.clipboard.enabled = serde_json::from_value(value)?,
        "remap" => {
            let on: bool = serde_json::from_value(value)?;
            config.keys.remap = if on { Remap::Auto } else { Remap::None };
        }
        "invert_wheel" => config.scrolling.invert_wheel = serde_json::from_value(value)?,
        "push_distance" => {
            let distance: f64 = serde_json::from_value(value)?;
            if !distance.is_finite() || !(0.0..=200.0).contains(&distance) {
                bail!("push distance must be between 0 and 200");
            }
            config.switching.push_distance = distance;
        }
        "controller" => {
            config.control.controller = serde_json::from_value(value)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            // Several changes within one second must still be newer.
            config.control.updated_at = now.max(config.control.updated_at.saturating_add(1));
        }
        _ => bail!("unknown setting: {key}"),
    }
    Ok(())
}

fn command(command: Command, engine: Arc<Engine>, state: Shared) -> Result<()> {
    let _entered = runtime().enter();
    match command {
        Command::Sharing { enabled: true } => engine.start_sharing()?,
        Command::Sharing { enabled: false } => runtime().block_on(engine.stop_sharing()),
        Command::Pair { id } => {
            let generation = {
                let mut s = lock(&state);
                if s.pairing.is_some() {
                    bail!("pairing already in progress");
                }
                s.pairing_generation += 1;
                let name = s
                    .nearby
                    .get(&id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| id.fmt_short().to_string());
                s.pairing = Some(Pairing {
                    id: id.to_string(),
                    name,
                    code: String::new(),
                    incoming: false,
                    stage: Stage::Connecting,
                });
                s.pairing_generation
            };
            let s = state.clone();
            let task = runtime().spawn(async move {
                match engine.net().pair(id).await {
                    Ok(a) => install_attempt(&s, a, Some(generation)),
                    Err(e) => {
                        let mut state = lock(&s);
                        if state.pairing_generation == generation {
                            state.pairing = None;
                            state.notify(format!("Pairing failed: {e:#}"));
                        }
                        drop(state);
                        changed();
                    }
                }
            });
            lock(&state).pairing_task = Some(task.abort_handle());
        }
        Command::ConfirmPair { accept } => {
            let (attempt, generation) = {
                let mut s = lock(&state);
                if !accept {
                    if let Some(attempt) = s.cancel_pairing() {
                        runtime().spawn(async move {
                            let _ = attempt.decide(false).await;
                        });
                    }
                    return Ok(());
                }
                let attempt = s.attempt.take().context("no pairing attempt")?;
                if let Some(p) = s.pairing.as_mut() {
                    p.stage = Stage::Waiting;
                }
                (attempt, s.pairing_generation)
            };
            let s = state.clone();
            let task = runtime().spawn(async move {
                let result = attempt.decide(true).await;
                let mut state = lock(&s);
                if state.pairing_generation != generation {
                    return;
                }
                state.pairing = None;
                state.pairing_task = None;
                match result {
                    Ok(PairOutcome::Paired(p)) => {
                        state.notify(format!(
                            "Paired with \"{}\". Arrange its displays to start sharing.",
                            p.name
                        ));
                        // Pairing registers the peer with running sessions; this only
                        // starts sharing if it was off.
                        if let Err(e) = engine.start_sharing() {
                            state.notify(format!("{e:#}"));
                        }
                    }
                    Ok(PairOutcome::DeclinedThere) => {
                        state.notify("The other device declined pairing.".into());
                    }
                    Ok(PairOutcome::DeclinedHere) => {}
                    Err(e) => state.notify(format!("Pairing failed: {e:#}")),
                }
                drop(state);
                changed();
            });
            lock(&state).pairing_task = Some(task.abort_handle());
        }
        Command::Unpair { id } => {
            // Net saves the removal before changing its allow-list or sessions, so a
            // failed save leaves the device paired and connected.
            engine.net().unpair(&id)?;
            {
                let mut s = lock(&state);
                s.connections.remove(&id);
                if s.active == Some(id) {
                    s.active = None;
                }
            }
            engine
                .update_config(|c| {
                    c.forget_neighbor(&id.to_string());
                })
                .context("Device forgotten, but its saved display position couldn't be removed")?;
        }
        Command::Setting { key, value } => {
            // Validate first, then apply only this field to the latest saved settings,
            // so policy updates from peers aren't overwritten.
            setting(&mut engine.config(), &key, value.clone())?;
            let mut error = None;
            engine.update_config(|c| error = setting(c, &key, value).err())?;
            if let Some(e) = error {
                return Err(e);
            }
        }
        Command::Place { id, x, y } => {
            if !x.is_finite() || !y.is_finite() {
                bail!("invalid position");
            }
            let snapped = editor(&engine, &lock(&state))
                .snap(id, Point::new(x, y))
                .context("this device's displays aren't known yet")?;
            engine.update_config(|c| arrange::place(c, id, snapped))?;
        }
        Command::SendFiles { id, paths } => {
            if paths.is_empty() {
                bail!("select files or folders first");
            }
            if let Err(e) = engine.send_files(id, paths) {
                let name = name_of(&engine, &id);
                lock(&state).failed(format!("Couldn't send files to \"{name}\": {e:#}"));
            }
        }
        Command::DismissFailure { index } => {
            let mut s = lock(&state);
            if index < s.failures.len() {
                s.failures.remove(index);
            }
        }
        Command::DismissNotice => lock(&state).notices.clear(),
    }
    Ok(())
}

#[derive(Serialize)]
struct Reply {
    ok: bool,
    error: Option<String>,
}

/// Runs a JSON [`Command`] and returns a JSON reply, `{"ok":true}` or
/// `{"ok":false,"error":"…"}`. Free it using [`legato_free`].
///
/// # Safety
/// `input` must be null or a NUL-terminated string that stays readable for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn legato_command(input: *const c_char) -> *mut c_char {
    let run = || -> Result<()> {
        if input.is_null() {
            bail!("null command");
        }
        // SAFETY: the caller supplies a valid C string for the duration of this call.
        let input = unsafe { CStr::from_ptr(input) }.to_str()?;
        let cmd: Command = serde_json::from_str(input)?;
        let (engine, state) = {
            let bridge = lock(&BRIDGE);
            let b = bridge.as_ref().context("Legato isn't running")?;
            (b.engine.clone(), b.state.clone())
        };
        command(cmd, engine, state)
    };
    let result = catch_unwind(AssertUnwindSafe(run))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("internal error; see the log for details")));
    changed();
    encoded(&match result {
        Ok(()) => Reply {
            ok: true,
            error: None,
        },
        Err(e) => Reply {
            ok: false,
            error: Some(format!("{e:#}")),
        },
    })
}

/// Stops sharing and networking, returning native input before the app exits.
#[unsafe(no_mangle)]
pub extern "C" fn legato_shutdown() {
    let _ = catch_unwind(|| {
        *lock(&OBSERVER) = None;
        let Some(bridge) = lock(&BRIDGE).take() else {
            return;
        };
        {
            let mut s = lock(&bridge.state);
            if let Some(task) = s.pairing_task.take() {
                task.abort();
            }
            s.attempt = None;
        }
        runtime().block_on(async {
            bridge.engine.stop_sharing().await;
            bridge.engine.net().clone().shutdown().await;
        });
        for task in bridge.tasks {
            task.abort();
        }
    });
}

/// # Safety
/// `string` must be null or a pointer returned by this library, not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn legato_free(string: *mut c_char) {
    if !string.is_null() {
        // SAFETY: ownership of this allocation is returned exactly once by the caller.
        drop(unsafe { CString::from_raw(string) });
    }
}

#[cfg(test)]
mod tests;
