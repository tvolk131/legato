//! Being controlled: the Mac side for now.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Result, bail};
use legato_core::{Inject, Receiver};
use legato_macos::{ActivityMonitor, Injector, Permissions};
use legato_net::{Session, SessionEvent};
use legato_proto::{Control, Datagram};
use tokio::sync::{mpsc::UnboundedReceiver, oneshot};

use crate::{Ctx, Status};

enum Input {
    Control(Control),
    Datagram(Datagram),
    LocalActivity,
    Disconnected,
    InvertWheel(bool),
    Stop,
}

pub(crate) async fn run(
    mut ctx: Ctx,
    mut events: UnboundedReceiver<SessionEvent>,
    mut stop: oneshot::Receiver<()>,
) -> Result<()> {
    if !Permissions::check().all_granted() {
        legato_macos::request_accessibility();
        bail!(
            "Legato needs Accessibility access to control this Mac. Allow it in System Settings → \
             Privacy & Security → Accessibility, then start sharing again."
        );
    }
    let (tx, rx) = mpsc::channel();
    let controller: Arc<Mutex<Option<Arc<Session>>>> = Arc::default();
    let injector_thread = {
        let controller = controller.clone();
        let invert = ctx.config.borrow().scrolling.invert_wheel;
        let status = ctx.engine.status.clone();
        std::thread::Builder::new()
            .name("legato-inject".into())
            .spawn(move || inject_loop(rx, controller, invert, status))?
    };
    let _monitor = {
        let tx = tx.clone();
        ActivityMonitor::start(move || {
            let _ = tx.send(Input::LocalActivity);
        })
        .map_err(anyhow::Error::msg)?
    };

    loop {
        tokio::select! {
            _ = &mut stop => break,
            changed = ctx.config.changed() => {
                if changed.is_err() {
                    break;
                }
                let invert = ctx.config.borrow_and_update().scrolling.invert_wheel;
                let _ = tx.send(Input::InvertWheel(invert));
            }
            event = events.recv() => match event {
                None => break,
                Some(SessionEvent::Connected(session)) => {
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
                    *controller.lock().unwrap() = Some(session);
                }
                Some(SessionEvent::Control { peer, msg }) => {
                    if let Control::Screens(screens) = &msg {
                        ctx.status(Status::PeerScreens { id: peer, screens: screens.clone() });
                    }
                    let _ = tx.send(Input::Control(msg));
                }
                Some(SessionEvent::Datagram { msg, .. }) => {
                    let _ = tx.send(Input::Datagram(msg));
                }
                Some(SessionEvent::Disconnected { peer, reason }) => {
                    let mut current = controller.lock().unwrap();
                    if let Some(session) = current.as_ref().filter(|s| s.peer == peer) {
                        ctx.status(Status::PeerDisconnected {
                            id: peer,
                            name: session.remote.name.clone(),
                            reason,
                        });
                        *current = None;
                        let _ = tx.send(Input::Disconnected);
                    }
                }
            }
        }
    }
    let _ = tx.send(Input::Stop);
    let _ = injector_thread.join();
    Ok(())
}

/// Owns the injector and receiver state; runs until told to stop.
fn inject_loop(
    rx: mpsc::Receiver<Input>,
    controller: Arc<Mutex<Option<Arc<Session>>>>,
    invert_wheel: bool,
    status: tokio::sync::broadcast::Sender<Status>,
) {
    let mut receiver = Receiver::new(legato_macos::receiver_config());
    let Some(mut injector) = Injector::new() else {
        tracing::error!("could not create a Quartz event source");
        return;
    };
    injector.invert_wheel = invert_wheel;
    let mut out = Vec::new();
    let mut controlled = false;
    loop {
        let input = match receiver.next_deadline() {
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let now = Instant::now();
        let stop = match input {
            Ok(Input::Control(msg)) => {
                receiver.control(now, msg, &mut out);
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
            Ok(Input::Disconnected) => {
                receiver.disconnected(&mut out);
                false
            }
            Ok(Input::InvertWheel(invert)) => {
                injector.invert_wheel = invert;
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
                if let Some(session) = controller.lock().unwrap().as_ref() {
                    session.send(Control::Yield);
                }
            } else {
                injector.apply(&action);
            }
        }
        if receiver.is_controlled() != controlled {
            controlled = receiver.is_controlled();
            let by = controlled
                .then(|| controller.lock().unwrap().as_ref().map(|s| s.peer))
                .flatten();
            let _ = status.send(Status::ControlledBy(by));
        }
        if stop {
            return;
        }
    }
}
