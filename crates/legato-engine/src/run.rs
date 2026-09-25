//! The sharing loop, the same on every machine: capture this machine's input (when the
//! control mode allows) to drive peers, and inject input from whichever peer drives this
//! one. Also shares the clipboard, mirrors arrangements, and agrees on the control mode.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self as std_mpsc, RecvTimeoutError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use anyhow::Result;
use legato_core::controller::{
    Action, CaptureCommand, Controller, ControllerConfig, Event, Portal,
};
use legato_core::{ActivityFilter, Inject, KeyRemap, Layout, MachineId, Receiver};
use legato_net::{EndpointId, Session, SessionEvent};
use legato_proto::{
    ClipboardContent, Control, ControlMode, Datagram, ExtendRequest, FilePurpose, Os, Point,
    Screens, blob,
};
use tokio::sync::{mpsc, oneshot};

use crate::arrange::{self, ConnectedPeer};
use crate::clipboard::{ClipboardSync, Copied};
use crate::config::{Config, Remap};
use crate::extend::{self, Viewing};
use crate::files::{self, Inbox};
use crate::platform::{self, Capture, Injector};
use crate::{Ctx, Status};

type BySessions = Arc<RwLock<HashMap<MachineId, Arc<Session>>>>;

struct Peer {
    session: Arc<Session>,
    screens: Screens,
    /// Where the peer placed us, if it did.
    placed_us_at: Option<Point>,
}

fn controller_config(config: &Config) -> ControllerConfig {
    ControllerConfig {
        push_distance: config.switching.push_distance,
        ..Default::default()
    }
}

fn remap(config: &Config, peer: Os) -> KeyRemap {
    match (config.keys.remap, crate::this_os(), peer) {
        (Remap::Auto, Os::Windows, Os::MacOs) => KeyRemap::windows_keyboard_on_mac(),
        (Remap::Auto, Os::MacOs, Os::Windows) => KeyRemap::mac_keyboard_on_windows(),
        _ => KeyRemap::identity(),
    }
}

fn control_mode(config: &Config) -> ControlMode {
    ControlMode {
        controller: config.control.controller.clone(),
        updated_at: config.control.updated_at,
    }
}

/// Messages for the injection thread.
enum Input {
    Control(EndpointId, Control),
    Datagram(Datagram),
    LocalActivity,
    Disconnected(EndpointId),
    InvertWheel(bool),
    Stop,
}

/// Requests from the engine's API while sharing runs.
pub(crate) enum RunCommand {
    SendFiles {
        to: EndpointId,
        paths: Vec<PathBuf>,
        purpose: FilePurpose,
    },
    Extend {
        to: EndpointId,
        request: ExtendRequest,
    },
    StopExtend {
        to: EndpointId,
    },
    ViewerWindow(Option<u64>),
}

fn send_files_in_background(
    ctx: &Ctx,
    session: Arc<Session>,
    paths: Vec<PathBuf>,
    purpose: FilePurpose,
) {
    let status = ctx.engine.status.clone();
    let name = session.remote.name.clone();
    tokio::spawn(async move {
        let _ = status.send(Status::FilesSending {
            to: session.peer,
            count: paths.len(),
        });
        match files::send(&session, &paths, purpose).await {
            Ok(bytes) => {
                let _ = status.send(Status::FilesSent {
                    to: session.peer,
                    bytes,
                });
            }
            Err(e) => {
                let _ = status.send(Status::Problem(format!(
                    "Couldn't send files to \"{name}\": {e:#}"
                )));
            }
        }
    });
}

pub(crate) async fn run(
    mut ctx: Ctx,
    local: Screens,
    mut events: mpsc::UnboundedReceiver<SessionEvent>,
    mut commands: mpsc::UnboundedReceiver<RunCommand>,
    mut stop: oneshot::Receiver<()>,
) -> Result<()> {
    platform::check_permissions()?;
    let own_id = ctx.engine.net().id().to_string();
    let mut config = ctx.config.borrow_and_update().clone();

    // Being driven: inject on a thread of its own.
    let by_peer: Arc<RwLock<HashMap<EndpointId, Arc<Session>>>> = Arc::default();
    let (inject_tx, inject_rx) = std_mpsc::channel();
    let injector = {
        let by_peer = by_peer.clone();
        let invert = config.scrolling.invert_wheel;
        let status = ctx.engine.status.clone();
        std::thread::Builder::new()
            .name("legato-inject".into())
            .spawn(move || inject_loop(inject_rx, by_peer, invert, status))?
    };

    // Driving: capture this machine's input. It always runs, so the user's own input is
    // noticed even when this machine may not drive others.
    let sessions: BySessions = Arc::default();
    let (active_tx, mut active_rx) = mpsc::unbounded_channel::<Option<MachineId>>();
    // Files dragged across an edge and released on another machine.
    let (dropped_tx, mut dropped_rx) = mpsc::unbounded_channel::<(Arc<Session>, Vec<PathBuf>)>();
    let capture = {
        let sessions = sessions.clone();
        let inject_tx = Mutex::new(inject_tx.clone());
        let mut filter = ActivityFilter::default();
        Capture::start(
            Controller::new(controller_config(&config), Layout::new(local.clone())),
            move |action| {
                let sessions = sessions.read().unwrap();
                match action {
                    Action::Send { to, msg } => {
                        if matches!(msg, Control::Enter { .. }) {
                            let _ = active_tx.send(Some(to));
                        }
                        if let Some(s) = sessions.get(&to) {
                            s.send(msg);
                        }
                    }
                    Action::Datagram { to, msg } => {
                        if let Some(s) = sessions.get(&to) {
                            s.send_datagram(&msg);
                        }
                    }
                    Action::Release { .. } => {
                        let _ = active_tx.send(None);
                    }
                    Action::Capture => {}
                    Action::Drop { to, files } => {
                        if let Some(s) = sessions.get(&to) {
                            let _ = dropped_tx.send((s.clone(), files));
                        }
                    }
                }
            },
            move |input| {
                if filter.feed(Instant::now(), input) {
                    let _ = inject_tx.lock().unwrap().send(Input::LocalActivity);
                }
            },
        )?
    };

    // Clipboard and files.
    let (copied_tx, mut copied_rx) = mpsc::unbounded_channel::<Copied>();
    let (mut inbox, mut inbox_done) = Inbox::new();
    let start_clipboard = |enabled: bool| {
        let tx = copied_tx.clone();
        enabled
            .then(|| {
                ClipboardSync::start(move |c| {
                    let _ = tx.send(c);
                })
            })
            .flatten()
    };
    let mut clipboard = start_clipboard(config.clipboard.enabled);

    let mut ids: HashMap<EndpointId, MachineId> = HashMap::new();
    let mut peers: HashMap<MachineId, Peer> = HashMap::new();

    // Virtual monitor mode: this Mac's extra display shown elsewhere, or a Mac's shown here.
    #[cfg(target_os = "macos")]
    let mut host: Option<extend::Host> = None;
    let mut viewing: Option<Viewing> = None;
    let mut viewer_window: Option<u64> = None;
    let frames = ctx.engine.viewer_frames.clone();
    let set_portal =
        |viewing: &Option<Viewing>, window: Option<u64>, ids: &HashMap<EndpointId, MachineId>| {
            let portal = viewing.and_then(|v| {
                Some(Portal {
                    peer: *ids.get(&v.peer)?,
                    remote: v.bounds?,
                    window: window?,
                })
            });
            capture.send(CaptureCommand::SetPortal(portal));
        };

    // Rebuilds the layout and tells each peer where we've put it.
    let relayout = |peers: &HashMap<MachineId, Peer>, config: &Config, ctx: &Ctx| {
        let may_drive = config.control.allows(&own_id);
        let connected: Vec<ConnectedPeer<'_>> = peers
            .iter()
            .map(|(&machine, p)| ConnectedPeer {
                machine,
                id: p.session.peer.to_string(),
                name: &p.session.remote.name,
                screens: &p.screens,
                placed_us_at: p.placed_us_at,
            })
            .collect();
        let (layout, problems) = arrange::build(&local, &connected, config);
        if may_drive {
            for problem in problems {
                ctx.status(Status::Problem(problem));
            }
        }
        for (&machine, p) in peers {
            let id = p.session.peer.to_string();
            let ours = config
                .neighbors
                .iter()
                .any(|n| !n.peer.is_empty() && id.starts_with(&n.peer));
            let offset = ours
                .then(|| layout.machine(machine).map(|m| m.offset))
                .flatten();
            p.session.send(Control::Placement { offset });
        }
        let layout = if may_drive {
            layout
        } else {
            Layout::new(local.clone())
        };
        capture.send(CaptureCommand::SetLayout(layout));
    };

    loop {
        tokio::select! {
            _ = &mut stop => break,
            changed = ctx.config.changed() => {
                if changed.is_err() {
                    break;
                }
                let new = ctx.config.borrow_and_update().clone();
                capture.send(CaptureCommand::SetConfig(controller_config(&new)));
                for (&machine, p) in &peers {
                    capture.send(CaptureCommand::SetRemap(machine, remap(&new, p.session.remote.os)));
                }
                let _ = inject_tx.send(Input::InvertWheel(new.scrolling.invert_wheel));
                if new.control != config.control {
                    for p in peers.values() {
                        p.session.send(Control::ControlMode(control_mode(&new)));
                    }
                }
                if new.clipboard.enabled != config.clipboard.enabled {
                    // Stop the old watcher before starting another.
                    drop(clipboard.take());
                    clipboard = start_clipboard(new.clipboard.enabled);
                }
                config = new;
                relayout(&peers, &config, &ctx);
            }
            Some(active) = active_rx.recv() => {
                let id = active.and_then(|m| peers.get(&m)).map(|p| p.session.peer);
                ctx.status(Status::Controlling(id));
            }
            Some(copied) = copied_rx.recv() => match copied {
                Copied::Content(content) => {
                    let Ok(bytes) = postcard::to_stdvec(&content) else { continue };
                    for p in peers.values() {
                        p.session.send_blob(blob::CLIPBOARD, bytes.clone());
                    }
                }
                Copied::Files(paths) => {
                    let total: u64 = files::collect(&paths).map_or(u64::MAX, |f| f.iter().map(|(_, _, s)| s).sum());
                    if total > files::MAX_CLIPBOARD_BATCH {
                        ctx.status(Status::Problem(
                            "Copied files are too big to share through the clipboard; send them instead.".into(),
                        ));
                        continue;
                    }
                    for p in peers.values() {
                        send_files_in_background(&ctx, p.session.clone(), paths.clone(), FilePurpose::Clipboard);
                    }
                }
            },
            Some((session, paths)) = dropped_rx.recv() => {
                send_files_in_background(&ctx, session, paths, FilePurpose::Drop);
            }
            Some(command) = commands.recv() => match command {
                RunCommand::SendFiles { to, paths, purpose } => {
                    match by_peer.read().unwrap().get(&to).cloned() {
                        Some(session) => send_files_in_background(&ctx, session, paths, purpose),
                        None => ctx.status(Status::Problem("That device isn't connected.".into())),
                    }
                }
                RunCommand::Extend { to, request } => {
                    match by_peer.read().unwrap().get(&to).cloned() {
                        Some(session) => {
                            if let Some(old) = viewing.take()
                                && old.peer != to
                                && let Some(s) = by_peer.read().unwrap().get(&old.peer)
                            {
                                s.send(Control::ExtendStop { reason: String::new() });
                            }
                            viewing = Some(Viewing { peer: to, bounds: None });
                            session.send(Control::ExtendRequest(request));
                        }
                        None => ctx.status(Status::ExtendEnded {
                            id: to,
                            reason: "that device isn't connected".into(),
                        }),
                    }
                    set_portal(&viewing, viewer_window, &ids);
                }
                RunCommand::StopExtend { to } => {
                    if viewing.is_some_and(|v| v.peer == to) {
                        viewing = None;
                        frames.send_replace(None);
                        if let Some(s) = by_peer.read().unwrap().get(&to) {
                            s.send(Control::ExtendStop { reason: String::new() });
                        }
                        set_portal(&viewing, viewer_window, &ids);
                    }
                }
                RunCommand::ViewerWindow(window) => {
                    viewer_window = window;
                    set_portal(&viewing, viewer_window, &ids);
                }
            },
            Some(result) = inbox_done.recv() => match result {
                Ok((info, _path)) => {
                    if let Some(received) = inbox.finished(&info) {
                        if received.purpose == FilePurpose::Clipboard
                            && let Some(clipboard) = &clipboard
                        {
                            clipboard.set_files(received.paths.clone());
                        }
                        ctx.status(Status::FilesReceived(received));
                    }
                }
                Err(e) => ctx.status(Status::Problem(format!("Couldn't receive a file: {e:#}"))),
            },
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    SessionEvent::Connected(session) => {
                        let next = MachineId(ids.len() as u32 + 1);
                        let machine = *ids.entry(session.peer).or_insert(next);
                        capture.send(CaptureCommand::SetRemap(machine, remap(&config, session.remote.os)));
                        ctx.status(Status::PeerConnected {
                            id: session.peer,
                            name: session.remote.name.clone(),
                            os: session.remote.os,
                            path: session.path(),
                        });
                        ctx.status(Status::PeerScreens {
                            id: session.peer,
                            screens: session.remote.screens.clone(),
                        });
                        session.send(Control::ControlMode(control_mode(&config)));
                        sessions.write().unwrap().insert(machine, session.clone());
                        by_peer.write().unwrap().insert(session.peer, session.clone());
                        peers.insert(machine, Peer {
                            screens: session.remote.screens.clone(),
                            session,
                            placed_us_at: None,
                        });
                        relayout(&peers, &config, &ctx);
                    }
                    SessionEvent::Control { peer, msg } => {
                        let Some(&machine) = ids.get(&peer) else { continue };
                        match msg {
                            Control::Yield => capture.send(CaptureCommand::Event(Event::PeerYield(machine))),
                            Control::Screens(screens) => {
                                ctx.status(Status::PeerScreens { id: peer, screens: screens.clone() });
                                if let Some(p) = peers.get_mut(&machine) {
                                    p.screens = screens;
                                }
                                relayout(&peers, &config, &ctx);
                            }
                            Control::Placement { offset } => {
                                if let Some(p) = peers.get_mut(&machine) {
                                    if p.placed_us_at == offset {
                                        continue;
                                    }
                                    p.placed_us_at = offset;
                                }
                                ctx.status(Status::PeerPlacedUs { id: peer, offset });
                                relayout(&peers, &config, &ctx);
                            }
                            Control::ControlMode(mode) => {
                                if mode.updated_at > config.control.updated_at {
                                    let _ = ctx.engine.update_config(|c| {
                                        c.control.controller = mode.controller.clone();
                                        c.control.updated_at = mode.updated_at;
                                    });
                                }
                            }
                            Control::ExtendRequest(request) => {
                                let Some(session) = peers.get(&machine).map(|p| p.session.clone()) else {
                                    continue;
                                };
                                #[cfg(target_os = "macos")]
                                {
                                    if let Some(old) = host.take() {
                                        old.stop().await;
                                    }
                                    match extend::Host::start(session.clone(), request, "Legato".into()).await {
                                        Ok(h) => host = Some(h),
                                        Err(e) => {
                                            ctx.status(Status::Problem(format!(
                                                "Couldn't show an extra display on \"{}\": {e:#}",
                                                session.remote.name
                                            )));
                                            session.send(Control::ExtendStop { reason: format!("{e:#}") });
                                        }
                                    }
                                }
                                #[cfg(not(target_os = "macos"))]
                                {
                                    let _ = request;
                                    session.send(Control::ExtendStop {
                                        reason: "only a Mac can show an extra display".into(),
                                    });
                                }
                            }
                            Control::Extended { bounds } => {
                                if let Some(v) = viewing.as_mut().filter(|v| v.peer == peer) {
                                    v.bounds = Some(bounds);
                                    ctx.status(Status::Extended { id: peer, bounds });
                                    set_portal(&viewing, viewer_window, &ids);
                                }
                            }
                            Control::ExtendStop { reason } => {
                                #[cfg(target_os = "macos")]
                                if let Some(h) = host.take_if(|h| h.peer == peer) {
                                    h.stop().await;
                                }
                                if viewing.is_some_and(|v| v.peer == peer) {
                                    viewing = None;
                                    frames.send_replace(None);
                                    set_portal(&viewing, viewer_window, &ids);
                                    ctx.status(Status::ExtendEnded { id: peer, reason });
                                }
                            }
                            Control::Keyframe => {
                                #[cfg(target_os = "macos")]
                                if let Some(h) = host.as_ref().filter(|h| h.peer == peer) {
                                    h.request_keyframe();
                                }
                            }
                            Control::Hello(_) => {}
                            input => {
                                let _ = inject_tx.send(Input::Control(peer, input));
                            }
                        }
                    }
                    SessionEvent::Datagram { msg, .. } => {
                        let _ = inject_tx.send(Input::Datagram(msg));
                    }
                    SessionEvent::Blob { tag, data, .. } => {
                        if tag == blob::CLIPBOARD
                            && let Some(clipboard) = &clipboard
                            && let Ok(content) = postcard::from_bytes::<ClipboardContent>(&data)
                        {
                            clipboard.set(content);
                        }
                    }
                    SessionEvent::File { peer, file } => inbox.receive(peer, file),
                    SessionEvent::Video { peer, video } => {
                        #[cfg(windows)]
                        if viewing.is_some_and(|v| v.peer == peer)
                            && let Some(session) = by_peer.read().unwrap().get(&peer).cloned()
                        {
                            extend::decode(
                                video,
                                session,
                                frames.clone(),
                                ctx.engine.viewer_stats.clone(),
                            );
                            continue;
                        }
                        let _ = (peer, video);
                    }
                    SessionEvent::Disconnected { peer, reason } => {
                        let Some(&machine) = ids.get(&peer) else { continue };
                        if let Some(p) = peers.remove(&machine) {
                            ctx.status(Status::PeerDisconnected {
                                id: peer,
                                name: p.session.remote.name.clone(),
                                reason,
                            });
                        }
                        sessions.write().unwrap().remove(&machine);
                        by_peer.write().unwrap().remove(&peer);
                        #[cfg(target_os = "macos")]
                        if let Some(h) = host.take_if(|h| h.peer == peer) {
                            h.stop().await;
                        }
                        if viewing.is_some_and(|v| v.peer == peer) {
                            viewing = None;
                            frames.send_replace(None);
                            set_portal(&viewing, viewer_window, &ids);
                            ctx.status(Status::ExtendEnded { id: peer, reason: "disconnected".into() });
                        }
                        capture.send(CaptureCommand::Event(Event::PeerLost(machine)));
                        let _ = inject_tx.send(Input::Disconnected(peer));
                        relayout(&peers, &config, &ctx);
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(h) = host.take() {
        h.stop().await;
    }
    if let Some(v) = viewing {
        frames.send_replace(None);
        ctx.status(Status::ExtendEnded {
            id: v.peer,
            reason: String::new(),
        });
    }
    drop(clipboard);
    drop(capture);
    let _ = inject_tx.send(Input::Stop);
    let _ = injector.join();
    Ok(())
}

/// Owns the receiver and injector: turns a peer's input into local events.
fn inject_loop(
    rx: std_mpsc::Receiver<Input>,
    sessions: Arc<RwLock<HashMap<EndpointId, Arc<Session>>>>,
    invert_wheel: bool,
    status: tokio::sync::broadcast::Sender<Status>,
) {
    let mut receiver = Receiver::new(platform::receiver_config());
    let Some(mut injector) = Injector::new() else {
        tracing::error!("could not set up input injection");
        return;
    };
    injector.set_invert_wheel(invert_wheel);
    let mut out = Vec::new();
    // The peer currently driving this machine.
    let mut driver: Option<EndpointId> = None;
    let mut controlled = false;
    loop {
        let input = match receiver.next_deadline() {
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let now = Instant::now();
        let stop = match input {
            Ok(Input::Control(peer, msg)) => {
                if matches!(msg, Control::Enter { .. }) {
                    driver = Some(peer);
                }
                // Only the current driver's input counts.
                if driver == Some(peer) {
                    receiver.control(now, msg, &mut out);
                }
                false
            }
            Ok(Input::Datagram(msg)) => {
                receiver.datagram(msg, &mut out);
                false
            }
            Ok(Input::LocalActivity) => {
                receiver.local_activity(&mut out);
                false
            }
            Ok(Input::Disconnected(peer)) => {
                if driver == Some(peer) {
                    receiver.disconnected(&mut out);
                    driver = None;
                }
                false
            }
            Ok(Input::InvertWheel(invert)) => {
                injector.set_invert_wheel(invert);
                false
            }
            Ok(Input::Stop) | Err(RecvTimeoutError::Disconnected) => {
                receiver.disconnected(&mut out);
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
        };
        receiver.tick(now, &mut out);
        for action in out.drain(..) {
            if action == Inject::SendYield {
                if let Some(session) =
                    driver.and_then(|d| sessions.read().unwrap().get(&d).cloned())
                {
                    session.send(Control::Yield);
                }
            } else {
                injector.apply(&action);
            }
        }
        if receiver.is_controlled() != controlled {
            controlled = receiver.is_controlled();
            let _ = status.send(Status::ControlledBy(driver.filter(|_| controlled)));
        }
        if stop {
            return;
        }
    }
}
