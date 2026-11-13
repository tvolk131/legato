//! Owned UTF-8 JSON messages between SwiftUI and the existing Rust engine.
//! There is one engine per process. Swift must free every returned string exactly once.

use anyhow::{Context, Result, bail};
use legato_core::{Layout, MachineId};
use legato_engine::{
    Engine, Status,
    arrange::{self, ConnectedPeer},
    config::{Config, Neighbor, Remap},
};
use legato_net::{EndpointId, Nearby, NearbyEvent, PairAttempt, PairOutcome};
use legato_proto::{Point, Rect};
use n0_future::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::sync::{Arc, Mutex, OnceLock};

// Use precisely the same placement rules as the existing arrangement editor.
#[path = "../../legato-app/src/snap.rs"]
mod snap;

#[derive(Default)]
struct State {
    connections: HashMap<EndpointId, String>,
    nearby: HashMap<EndpointId, Nearby>,
    placed_us: HashMap<EndpointId, Point>,
    active: Option<EndpointId>,
    pairing: Option<Value>,
    attempt: Option<PairAttempt>,
    pairing_generation: u64,
    pairing_task: Option<tokio::task::AbortHandle>,
    notices: Vec<String>,
    failures: Vec<String>,
    reveal_sequence: u64,
    reveal: Option<Value>,
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
}
struct Bridge {
    engine: Arc<Engine>,
    state: Arc<Mutex<State>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);
static ERROR: Mutex<Option<String>> = Mutex::new(None);
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().expect("Tokio runtime"))
}
fn encoded(value: Value) -> *mut c_char {
    CString::new(value.to_string())
        .expect("JSON escapes NUL")
        .into_raw()
}
fn record(state: &Arc<Mutex<State>>, text: String) {
    let mut state = state.lock().unwrap();
    if state.notices.len() == 100 {
        state.notices.remove(0);
    }
    state.notices.push(text);
}

fn init_logging() {
    static LOGGING: OnceLock<()> = OnceLock::new();
    LOGGING.get_or_init(|| {
        let file = legato_net::store::default_dir().ok().and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
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
    });
}

/// Starts networking, discovery and pairing. Does not grant OS input permissions.
#[unsafe(no_mangle)]
pub extern "C" fn legato_start() -> bool {
    init_logging();
    let mut bridge = BRIDGE.lock().unwrap();
    if bridge.is_some() {
        return true;
    }
    let result = runtime().block_on(Engine::start(None, env!("CARGO_PKG_VERSION")));
    let engine = match result {
        Ok(engine) => engine,
        Err(e) => {
            *ERROR.lock().unwrap() = Some(format!("{e:#}"));
            return false;
        }
    };
    *ERROR.lock().unwrap() = None;
    let state = Arc::new(Mutex::new(State::default()));
    let mut statuses = engine.subscribe();
    let e = engine.clone();
    let s = state.clone();
    let status_task = runtime().spawn(async move {
        loop {
            match statuses.recv().await {
                Ok(status) => {
                    if let Some(text) = legato_engine::describe(&status, |id| {
                        e.net()
                            .paired_peers()
                            .iter()
                            .find(|p| p.id == *id)
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| id.fmt_short().to_string())
                    }) {
                        tracing::info!("{text}");
                        record(&s, text);
                    }
                    let mut s = s.lock().unwrap();
                    match status {
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
                        Status::Sharing(false) => {
                            s.connections.clear();
                            s.active = None;
                        }
                        Status::Controlling(id) | Status::ControlledBy(id) => s.active = id,
                        Status::PeerPlacedUs { id, offset } => {
                            if let Some(offset) = offset {
                                s.placed_us.insert(id, offset);
                            } else {
                                s.placed_us.remove(&id);
                            }
                        }
                        Status::FilesReceived(received)
                            if received.purpose == legato_proto::FilePurpose::Drop =>
                        {
                            s.reveal_sequence += 1;
                            s.reveal =
                                Some(json!({"sequence":s.reveal_sequence,"paths":received.paths}));
                        }
                        Status::FileTransferFailed {
                            peer,
                            sending,
                            reason,
                        } => {
                            s.failures.push(format!(
                                "{} {}: {reason}",
                                if sending {
                                    "Sending to"
                                } else {
                                    "Receiving from"
                                },
                                peer.fmt_short()
                            ));
                        }
                        _ => {}
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => record(
                    &s,
                    format!("Skipped {n} status updates; inspect the log for details."),
                ),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let e = engine.clone();
    let s = state.clone();
    let discovery = runtime().spawn(async move {
        match e.net().nearby().await {
            Ok(mut events) => {
                while let Some(event) = events.next().await {
                    let mut state = s.lock().unwrap();
                    match event {
                        NearbyEvent::Found(peer) => {
                            state.nearby.insert(peer.id, peer);
                        }
                        NearbyEvent::Lost(id) => {
                            state.nearby.remove(&id);
                        }
                    }
                }
            }
            Err(e) => record(&s, format!("Discovery failed: {e:#}")),
        }
    });
    let mut incoming = engine.net().listen_for_pairing();
    let s = state.clone();
    let pairing = runtime().spawn(async move {
        while let Some(attempt) = incoming.recv().await {
            install_attempt(&s, attempt, None);
        }
    });
    // Like the existing GUI, resume sharing for already paired machines.
    if !engine.net().paired_peers().is_empty() {
        let _entered = runtime().enter();
        if let Err(e) = engine.start_sharing() {
            record(&state, format!("{e:#}"));
        }
    }
    *bridge = Some(Bridge {
        engine,
        state,
        tasks: vec![status_task, discovery, pairing],
    });
    true
}
fn install_attempt(state: &Arc<Mutex<State>>, attempt: PairAttempt, generation: Option<u64>) {
    let mut s = state.lock().unwrap();
    if generation.is_some_and(|g| g != s.pairing_generation)
        || (generation.is_none() && s.pairing.is_some())
        || s.attempt.is_some()
        || s.pairing.as_ref().is_some_and(|p| p["stage"] == "waiting")
    {
        runtime().spawn(async move {
            let _ = attempt.decide(false).await;
        });
        return;
    }
    if generation.is_none() {
        s.pairing_generation += 1;
    }
    s.pairing = Some(
        json!({"id":attempt.peer.id.to_string(), "name":attempt.peer.name,"code":format!("{:06}",attempt.code),"incoming":attempt.incoming,"stage":"confirm"}),
    );
    s.attempt = Some(attempt);
}

fn arrangement(engine: &Engine, state: &State) -> Value {
    let local = engine.screen_updates().borrow().clone();
    let peers = engine.net().paired_peers();
    let known = engine.known_screens();
    let missing: Vec<_> = peers
        .iter()
        .filter(|p| {
            known
                .get(&p.id)
                .is_none_or(|screens| screens.displays.is_empty())
        })
        .map(|p| {
            format!(
                "Connect to \"{}\" with Sharing on to learn its displays before arranging it.",
                p.name
            )
        })
        .collect();
    let local_primary: Vec<_> = local
        .displays
        .iter()
        .enumerate()
        .filter_map(|(i, d)| d.primary.then_some(i))
        .collect();
    let known: Vec<_> = peers
        .iter()
        .filter_map(|p| known.get(&p.id).map(|s| (p, s)))
        .collect();
    let connected: Vec<_> = known
        .iter()
        .enumerate()
        .map(|(i, (p, s))| ConnectedPeer {
            machine: MachineId(i as u32 + 1),
            id: p.id.to_string(),
            name: &p.name,
            screens: s,
            placed_us_at: state.placed_us.get(&p.id).copied(),
        })
        .collect();
    let (layout, mut problems) = arrange::build(&local, &connected, &engine.config());
    problems.extend(missing);
    let local: Vec<_> = layout.local().desk_rects().collect();
    let mut unplaced_x =
        legato_core::layout::bounding_box(local.iter().copied()).map_or(0.0, |b| b.right() + 200.0);
    let peers:Vec<_> = known.iter().enumerate().map(|(i,(p,s))| {
        let id = MachineId(i as u32+1);
        let placed = layout.machine(id);
        let displays:Vec<_> = if let Some(m) = placed { m.desk_rects().collect() } else {
            let mut l = Layout::new(engine.screen_updates().borrow().clone());
            l.place_at(id, (*s).clone(), Point::new(unplaced_x,0.0));
            let rects:Vec<_> = l.machine(id).map(|m| m.desk_rects().collect()).unwrap_or_default();
            if let Some(b) = legato_core::layout::bounding_box(rects.iter().copied()) { unplaced_x = b.right()+200.0; }
            rects
        };
        let primary: Vec<_> = s.displays.iter().enumerate().filter_map(|(i,d)| d.primary.then_some(i)).collect();
        json!({"id":p.id.to_string(),"name":p.name,"placed":placed.is_some(),"displays":displays,"primary":primary})
    }).collect();
    json!({"local":local,"local_primary":local_primary,"peers":peers,"problems":problems})
}

/// Returns a newly allocated JSON snapshot. Free using `legato_free`.
#[unsafe(no_mangle)]
pub extern "C" fn legato_snapshot() -> *mut c_char {
    let bridge = BRIDGE.lock().unwrap();
    let Some(bridge) = bridge.as_ref() else {
        return encoded(json!({"ready":false,"error":*ERROR.lock().unwrap()}));
    };
    let engine = &bridge.engine;
    let state = bridge.state.lock().unwrap();
    let paired = engine.net().paired_peers();
    let peers:Vec<_> = paired.iter().map(|p| json!({"id":p.id.to_string(),"name":p.name,"os":p.os,"connected":state.connections.contains_key(&p.id),"path":state.connections.get(&p.id),"active":state.active==Some(p.id)})).collect();
    let mut nearby: Vec<_> = state
        .nearby
        .values()
        .filter(|n| !paired.iter().any(|p| p.id == n.id))
        .collect();
    nearby.sort_by(|a, b| a.name.cmp(&b.name));
    let nearby: Vec<_> = nearby
        .iter()
        .map(|p| json!({"id":p.id.to_string(),"name":p.name,"os":p.os}))
        .collect();
    encoded(
        json!({"ready":true,"id":engine.net().id().to_string(),"name":engine.net().config().name,"version":env!("CARGO_PKG_VERSION"),"state_dir":engine.dir().to_string_lossy(),"sharing":engine.is_sharing(),"config":engine.config(),"paired":peers,"nearby":nearby,"local":engine.screen_updates().borrow().clone(),"pairing":state.pairing,"notices":state.notices,"failures":state.failures,"reveal":state.reveal,"arrangement":arrangement(engine,&state)}),
    )
}

#[derive(Deserialize)]
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
    Place {
        id: EndpointId,
        x: f64,
        y: f64,
    },
    SendFiles {
        id: EndpointId,
        paths: Vec<std::path::PathBuf>,
    },
    DismissFailures {
        index: Option<usize>,
    },
    DismissNotices,
}
fn setting(config: &mut Config, key: &str, value: Value) -> Result<()> {
    match key {
        "clipboard" => config.clipboard.enabled = serde_json::from_value(value)?,
        "remap" => {
            config.keys.remap = if value == Value::Bool(true) || value == "auto" {
                Remap::Auto
            } else if value == Value::Bool(false) || value == "none" {
                Remap::None
            } else {
                bail!("invalid remap")
            }
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
            config.control.updated_at = now.max(config.control.updated_at.saturating_add(1));
        }
        _ => bail!("unknown setting: {key}"),
    }
    Ok(())
}
fn command(command: Command, engine: Arc<Engine>, state: Arc<Mutex<State>>) -> Result<()> {
    let _entered = runtime().enter();
    match command {
        Command::Sharing { enabled } => {
            if enabled {
                engine.start_sharing()?;
            } else {
                runtime().block_on(engine.stop_sharing());
            }
        }
        Command::Pair { id } => {
            let generation = {
                let mut s = state.lock().unwrap();
                if s.pairing.is_some() {
                    bail!("pairing already in progress");
                }
                s.pairing_generation += 1;
                let name = s
                    .nearby
                    .get(&id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| id.fmt_short().to_string());
                s.pairing = Some(
                    json!({"id":id.to_string(),"name":name,"code":"","incoming":false,"stage":"connecting"}),
                );
                s.pairing_generation
            };
            let s = state.clone();
            let task = runtime().spawn(async move {
                match engine.net().pair(id).await {
                    Ok(a) => install_attempt(&s, a, Some(generation)),
                    Err(e) => {
                        let mut state = s.lock().unwrap();
                        if state.pairing_generation == generation {
                            state.pairing = None;
                            state.notices.push(format!("Pairing failed: {e:#}"));
                        }
                    }
                }
            });
            state.lock().unwrap().pairing_task = Some(task.abort_handle());
        }
        Command::ConfirmPair { accept } => {
            let (attempt, generation) = {
                let mut s = state.lock().unwrap();
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
                    p["stage"] = "waiting".into();
                }
                (attempt, s.pairing_generation)
            };
            let s = state.clone();
            let task = runtime().spawn(async move {
                let result = attempt.decide(true).await;
                {
                    let mut state = s.lock().unwrap();
                    if state.pairing_generation != generation {
                        return;
                    }
                    state.pairing = None;
                    state.pairing_task = None;
                }
                match result {
                    Ok(PairOutcome::Paired(p)) => {
                        record(
                            &s,
                            format!(
                                "Paired with {}. Arrange its displays to start sharing.",
                                p.name
                            ),
                        );
                        // PairAttempt already registers the peer with running
                        // sessions through Net::add_paired. Preserve existing input
                        // connections; only start the engine if it was stopped.
                        if let Err(e) = engine.start_sharing() {
                            record(&s, format!("{e:#}"));
                        }
                    }
                    Ok(PairOutcome::DeclinedThere) => {
                        record(&s, "The other device declined pairing.".into())
                    }
                    Ok(PairOutcome::DeclinedHere) => {}
                    Err(e) => record(&s, format!("Pairing failed: {e:#}")),
                }
            });
            state.lock().unwrap().pairing_task = Some(task.abort_handle());
        }
        Command::Unpair { id } => {
            // Net persists the removal before changing its allow-list or sessions.
            // A failed peer-store save must leave the connected identity untouched.
            engine.net().unpair(&id)?;
            {
                let mut s = state.lock().unwrap();
                s.connections.remove(&id);
                if s.active == Some(id) {
                    s.active = None;
                }
            }
            engine
                .update_config(|c| {
                    c.neighbors
                        .retain(|n| n.peer.is_empty() || !id.to_string().starts_with(&n.peer))
                })
                .context(
                    "Device forgotten, but its saved display placement could not be removed",
                )?;
        }
        Command::Setting { key, value } => {
            // Validate before entering the save transaction; apply only this field to
            // the latest committed config so peer policy updates cannot be overwritten.
            setting(&mut engine.config(), &key, value.clone())?;
            let mut error = None;
            engine.update_config(|c| {
                if let Err(e) = setting(c, &key, value) {
                    error = Some(e);
                }
            })?;
            if let Some(e) = error {
                return Err(e);
            }
        }
        Command::Place { id, x, y } => {
            if !x.is_finite() || !y.is_finite() {
                bail!("invalid placement");
            }
            let s = state.lock().unwrap();
            let editor = arrangement(&engine, &s);
            let local: Vec<Rect> = serde_json::from_value(editor["local"].clone())?;
            let peer = editor["peers"]
                .as_array()
                .and_then(|p| p.iter().find(|p| p["id"] == id.to_string()))
                .context("peer displays unavailable")?;
            let rects: Vec<Rect> = serde_json::from_value(peer["displays"].clone())?;
            let bounds =
                legato_core::layout::bounding_box(rects).context("peer has no displays")?;
            let snapped = snap::snap(&local, Rect::new(x, y, bounds.width, bounds.height))
                .context("no usable local display")?;
            drop(s);
            engine.update_config(|c| {
                c.neighbors
                    .retain(|n| n.peer.is_empty() || !id.to_string().starts_with(&n.peer));
                c.neighbors.push(Neighbor {
                    peer: id.to_string(),
                    side: snapped.side,
                    display: snapped.display + 1,
                    align: legato_core::Align::Center,
                    nudge: 0.0,
                    offset: Some([snapped.top_left.x, snapped.top_left.y]),
                });
            })?;
        }
        Command::SendFiles { id, paths } => {
            if paths.is_empty() {
                bail!("select files or folders first");
            }
            engine.send_files(id, paths)?;
        }
        Command::DismissFailures { index } => {
            let mut s = state.lock().unwrap();
            if let Some(index) = index {
                if index < s.failures.len() {
                    s.failures.remove(index);
                }
            } else {
                s.failures.clear();
            }
        }
        Command::DismissNotices => state.lock().unwrap().notices.clear(),
    }
    Ok(())
}

/// Execute a command. Input must point to a valid NUL-terminated UTF-8 string.
///
/// # Safety
/// `input` must remain readable through its terminating NUL for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn legato_command(input: *const c_char) -> *mut c_char {
    let result = (|| -> Result<()> {
        if input.is_null() {
            bail!("null command");
        }
        // SAFETY: the caller supplies a valid C string for the duration of this call.
        let input = unsafe { CStr::from_ptr(input) }.to_str()?;
        let cmd: Command = serde_json::from_str(input)?;
        let (engine, state) = {
            let bridge = BRIDGE.lock().unwrap();
            let b = bridge.as_ref().context("engine is not running")?;
            (b.engine.clone(), b.state.clone())
        };
        command(cmd, engine, state)
    })();
    match result {
        Ok(()) => encoded(json!({"ok":true})),
        Err(e) => encoded(json!({"ok":false,"error":format!("{e:#}")})),
    }
}

/// Stops sharing and networking, returning native input before the app exits.
#[unsafe(no_mangle)]
pub extern "C" fn legato_shutdown() {
    if let Some(bridge) = BRIDGE.lock().unwrap().take() {
        if let Some(task) = bridge.state.lock().unwrap().pairing_task.take() {
            task.abort();
        }
        bridge.state.lock().unwrap().attempt = None;
        runtime().block_on(async {
            bridge.engine.stop_sharing().await;
            bridge.engine.net().clone().shutdown().await;
        });
        for task in bridge.tasks {
            task.abort();
        }
    }
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
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancel_pairing_aborts_network_work_and_invalidates_stale_completion() {
        for stage in ["connecting", "waiting", "confirm"] {
            let task = tokio::spawn(std::future::pending::<()>());
            let mut state = State {
                pairing: Some(json!({"stage":stage})),
                pairing_generation: 4,
                pairing_task: Some(task.abort_handle()),
                ..State::default()
            };
            assert!(state.cancel_pairing().is_none());
            assert!(state.pairing.is_none());
            assert!(state.pairing_task.is_none());
            assert_eq!(state.pairing_generation, 5);
            assert!(task.await.unwrap_err().is_cancelled());
        }
    }

    #[test]
    fn settings_edit_only_the_requested_field_and_reject_invalid_values() {
        let mut c = Config::default();
        c.control.controller = Some("peer".into());
        setting(&mut c, "clipboard", json!(false)).unwrap();
        assert!(!c.clipboard.enabled);
        assert_eq!(c.control.controller.as_deref(), Some("peer"));
        assert!(setting(&mut c, "push_distance", json!(-1)).is_err());
        assert!(setting(&mut c, "remap", json!("typo")).is_err());
        setting(&mut c, "controller", Value::Null).unwrap();
        let before = c.control.updated_at;
        setting(&mut c, "controller", json!("local")).unwrap();
        assert!(c.control.updated_at > before);
    }
    #[test]
    fn rejects_unknown_commands_and_null_input() {
        assert!(
            serde_json::from_str::<Command>(
                r#"{"action":"setting","key":"clipboard","value":true,"typo":1}"#
            )
            .is_err()
        );
        // SAFETY: null is explicitly supported and returns an error object.
        let p = unsafe { legato_command(std::ptr::null()) };
        // SAFETY: p is an owned C string returned by the bridge.
        let value: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(p) }.to_str().unwrap()).unwrap();
        assert_eq!(value["ok"], false);
        // SAFETY: returns this allocation exactly once.
        unsafe { legato_free(p) };
    }
}
