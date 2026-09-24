//! Controlling other machines with this one's keyboard and mouse: the Windows side for now.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use legato_core::controller::{Action, Controller, ControllerConfig, Event};
use legato_core::{KeyRemap, MachineId};
use legato_net::{EndpointId, Session, SessionEvent};
use legato_proto::{Control, Os, Screens};
use legato_windows::{Capture, CaptureOptions, Command};
use tokio::sync::{mpsc, oneshot};

use crate::arrange::{self, ConnectedPeer};
use crate::config::{Config, Remap};
use crate::{Ctx, Status};

type Sessions = Arc<RwLock<HashMap<MachineId, Arc<Session>>>>;

struct Peer {
    session: Arc<Session>,
    screens: Screens,
}

fn controller_config(config: &Config) -> ControllerConfig {
    ControllerConfig {
        push_distance: config.switching.push_distance,
        ..Default::default()
    }
}

fn remap(config: &Config, os: Os) -> KeyRemap {
    match (config.keys.remap, os) {
        (Remap::Auto, Os::MacOs) => KeyRemap::windows_keyboard_on_mac(),
        _ => KeyRemap::identity(),
    }
}

pub(crate) async fn run(
    mut ctx: Ctx,
    local: Screens,
    mut events: mpsc::UnboundedReceiver<SessionEvent>,
    mut stop: oneshot::Receiver<()>,
) -> Result<()> {
    let sessions: Sessions = Arc::default();
    let initial = ctx.config.borrow_and_update().clone();
    let controller = Controller::new(
        controller_config(&initial),
        legato_core::Layout::new(local.clone()),
    );
    // Which peer the cursor is on, as the capture thread sees it.
    let (active_tx, mut active_rx) = mpsc::unbounded_channel::<Option<MachineId>>();
    let capture = {
        let sessions = sessions.clone();
        Capture::start(controller, CaptureOptions::default(), move |action| {
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
            }
        })
        .context("capturing the keyboard and mouse")?
    };

    let mut ids: HashMap<EndpointId, MachineId> = HashMap::new();
    let mut peers: HashMap<MachineId, Peer> = HashMap::new();
    let mut config = initial;
    let relayout = |peers: &HashMap<MachineId, Peer>, config: &Config, ctx: &Ctx| {
        let connected: Vec<ConnectedPeer<'_>> = peers
            .iter()
            .map(|(&machine, p)| ConnectedPeer {
                machine,
                id: p.session.peer.to_string(),
                name: &p.session.remote.name,
                screens: &p.screens,
            })
            .collect();
        let (layout, problems) = arrange::build(&local, &connected, config);
        for problem in problems {
            ctx.status(Status::Problem(problem));
        }
        capture.send(Command::SetLayout(layout));
    };

    loop {
        tokio::select! {
            _ = &mut stop => break,
            changed = ctx.config.changed() => {
                if changed.is_err() {
                    break;
                }
                config = ctx.config.borrow_and_update().clone();
                capture.send(Command::SetConfig(controller_config(&config)));
                for (&machine, p) in &peers {
                    capture.send(Command::SetRemap(machine, remap(&config, p.session.remote.os)));
                }
                relayout(&peers, &config, &ctx);
            }
            Some(active) = active_rx.recv() => {
                let id = active.and_then(|m| peers.get(&m)).map(|p| p.session.peer);
                ctx.status(Status::Controlling(id));
            }
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    SessionEvent::Connected(session) => {
                        let next = MachineId(ids.len() as u32 + 1);
                        let machine = *ids.entry(session.peer).or_insert(next);
                        capture.send(Command::SetRemap(machine, remap(&config, session.remote.os)));
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
                        sessions.write().unwrap().insert(machine, session.clone());
                        peers.insert(machine, Peer { screens: session.remote.screens.clone(), session });
                        relayout(&peers, &config, &ctx);
                    }
                    SessionEvent::Control { peer, msg } => {
                        let Some(&machine) = ids.get(&peer) else { continue };
                        match msg {
                            Control::Yield => capture.send(Command::Event(Event::PeerYield(machine))),
                            Control::Screens(screens) => {
                                ctx.status(Status::PeerScreens { id: peer, screens: screens.clone() });
                                if let Some(p) = peers.get_mut(&machine) {
                                    p.screens = screens;
                                }
                                relayout(&peers, &config, &ctx);
                            }
                            _ => {}
                        }
                    }
                    SessionEvent::Datagram { .. } => {}
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
                        capture.send(Command::Event(Event::PeerLost(machine)));
                        relayout(&peers, &config, &ctx);
                    }
                }
            }
        }
    }
    drop(capture);
    Ok(())
}
