//! Being controlled: the Mac side for now.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Result, bail};
use legato_core::{Inject, Receiver};
use legato_macos::{ActivityMonitor, Injector, Permissions};
use legato_net::{Net, Session, SessionEvent};
use legato_proto::{Control, Datagram};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::config::Config;

enum Input {
    Control(Control),
    Datagram(Datagram),
    LocalActivity,
    Disconnected,
    Stop,
}

pub async fn run(
    _net: &Net,
    config: &Config,
    mut events: UnboundedReceiver<SessionEvent>,
) -> Result<()> {
    if !Permissions::check().all_granted() {
        legato_macos::request_accessibility();
        bail!(
            "Legato needs Accessibility access to control this Mac. Allow it in System Settings → \
             Privacy & Security → Accessibility (for the app you're running `legato` from), then \
             run it again."
        );
    }
    let (tx, rx) = mpsc::channel();
    let controller: Arc<Mutex<Option<Arc<Session>>>> = Arc::default();
    let injector_thread = {
        let controller = controller.clone();
        let invert = config.scrolling.invert_wheel;
        std::thread::Builder::new()
            .name("legato-inject".into())
            .spawn(move || inject_loop(rx, controller, invert))?
    };
    let _monitor = {
        let tx = tx.clone();
        ActivityMonitor::start(move || {
            let _ = tx.send(Input::LocalActivity);
        })
        .map_err(anyhow::Error::msg)?
    };

    let result = loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break Ok(()),
            event = events.recv() => match event {
                None => break Ok(()),
                Some(SessionEvent::Connected(session)) => {
                    tracing::info!(
                        "Connected to \"{}\"{}. Move its cursor onto this Mac to take control.",
                        session.remote.name,
                        crate::describe_path(&session)
                    );
                    *controller.lock().unwrap() = Some(session);
                }
                Some(SessionEvent::Control { msg, .. }) => {
                    let _ = tx.send(Input::Control(msg));
                }
                Some(SessionEvent::Datagram { msg, .. }) => {
                    let _ = tx.send(Input::Datagram(msg));
                }
                Some(SessionEvent::Disconnected { peer, reason }) => {
                    let mut current = controller.lock().unwrap();
                    if current.as_ref().is_some_and(|s| s.peer == peer) {
                        tracing::info!("Disconnected from \"{}\": {reason}", current.as_ref().unwrap().remote.name);
                        *current = None;
                        let _ = tx.send(Input::Disconnected);
                    }
                }
            }
        }
    };
    let _ = tx.send(Input::Stop);
    let _ = injector_thread.join();
    result
}

/// Owns the injector and receiver state; runs until told to stop.
fn inject_loop(
    rx: mpsc::Receiver<Input>,
    controller: Arc<Mutex<Option<Arc<Session>>>>,
    invert_wheel: bool,
) {
    let mut receiver = Receiver::new(legato_macos::receiver_config());
    let Some(mut injector) = Injector::new() else {
        tracing::error!("could not create a Quartz event source");
        return;
    };
    injector.invert_wheel = invert_wheel;
    let mut out = Vec::new();
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
        if stop {
            return;
        }
    }
}
