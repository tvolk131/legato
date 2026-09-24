//! Controlling other machines with this one's keyboard and mouse: the Windows side for now.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use legato_core::controller::{Action, Controller, ControllerConfig, Event};
use legato_core::{KeyRemap, MachineId};
use legato_net::{EndpointId, Net, Session, SessionEvent};
use legato_proto::{Control, Os, Screens};
use legato_windows::{Capture, CaptureOptions, Command};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::arrange::{self, ConnectedPeer};
use crate::config::{Config, Remap};

type Sessions = Arc<RwLock<HashMap<MachineId, Arc<Session>>>>;

struct Peer {
    session: Arc<Session>,
    screens: Screens,
}

pub async fn run(
    _net: &Net,
    config: &Config,
    local: Screens,
    mut events: UnboundedReceiver<SessionEvent>,
) -> Result<()> {
    let sessions: Sessions = Arc::default();
    let controller = Controller::new(
        ControllerConfig {
            push_distance: config.switching.push_distance,
            ..Default::default()
        },
        legato_core::Layout::new(local.clone()),
    );
    let capture = {
        let sessions = sessions.clone();
        Capture::start(controller, CaptureOptions::default(), move |action| {
            let sessions = sessions.read().unwrap();
            match action {
                Action::Send { to, msg } => {
                    if let Some(s) = sessions.get(&to) {
                        s.send(msg);
                    }
                }
                Action::Datagram { to, msg } => {
                    if let Some(s) = sessions.get(&to) {
                        s.send_datagram(&msg);
                    }
                }
                Action::Capture | Action::Release { .. } => {}
            }
        })
        .context("capturing the keyboard and mouse")?
    };

    let mut ids: HashMap<EndpointId, MachineId> = HashMap::new();
    let mut peers: HashMap<MachineId, Peer> = HashMap::new();
    let relayout = |peers: &HashMap<MachineId, Peer>| {
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
            tracing::warn!("{problem}");
        }
        capture.send(Command::SetLayout(layout));
    };

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    SessionEvent::Connected(session) => {
                        let next = MachineId(ids.len() as u32 + 1);
                        let machine = *ids.entry(session.peer).or_insert(next);
                        let remap = match (config.keys.remap, session.remote.os) {
                            (Remap::Auto, Os::MacOs) => KeyRemap::windows_keyboard_on_mac(),
                            _ => KeyRemap::identity(),
                        };
                        capture.send(Command::SetRemap(machine, remap));
                        tracing::info!(
                            "Connected to \"{}\"{}.",
                            session.remote.name,
                            crate::describe_path(&session)
                        );
                        sessions.write().unwrap().insert(machine, session.clone());
                        peers.insert(machine, Peer { screens: session.remote.screens.clone(), session });
                        relayout(&peers);
                    }
                    SessionEvent::Control { peer, msg } => {
                        let Some(&machine) = ids.get(&peer) else { continue };
                        match msg {
                            Control::Yield => capture.send(Command::Event(Event::PeerYield(machine))),
                            Control::Screens(screens) => {
                                if let Some(p) = peers.get_mut(&machine) {
                                    p.screens = screens;
                                }
                                relayout(&peers);
                            }
                            _ => {}
                        }
                    }
                    SessionEvent::Datagram { .. } => {}
                    SessionEvent::Disconnected { peer, reason } => {
                        let Some(&machine) = ids.get(&peer) else { continue };
                        if let Some(p) = peers.remove(&machine) {
                            tracing::info!("Disconnected from \"{}\": {reason}", p.session.remote.name);
                        }
                        sessions.write().unwrap().remove(&machine);
                        capture.send(Command::Event(Event::PeerLost(machine)));
                        relayout(&peers);
                    }
                }
            }
        }
    }
    drop(capture);
    Ok(())
}
