//! Keep display enumeration off the sharing loop, including while sharing is stopped.

use std::sync::Arc;
use std::time::Duration;

use legato_proto::Screens;
use tokio::sync::watch;

pub(crate) fn watch(
    initial: Screens,
    sample: impl Fn() -> Screens + Send + Sync + 'static,
    period: Duration,
) -> watch::Receiver<Screens> {
    let (tx, rx) = watch::channel(initial);
    let sample = Arc::new(sample);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tx.closed() => break,
                _ = tick.tick() => {}
            }
            let sample = sample.clone();
            let Ok(next) = tokio::task::spawn_blocking(move || sample()).await else {
                continue;
            };
            // Enumeration can briefly fail during sleep or reconfiguration. Keep the
            // last usable desk until the OS reports displays again.
            if !next.displays.is_empty() {
                tx.send_if_modified(|current| {
                    if *current == next {
                        return false;
                    }
                    *current = next;
                    true
                });
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use legato_proto::{Display, Rect};
    use std::sync::Mutex;

    #[tokio::test]
    async fn refreshes_bounds_scale_and_hotplug_but_keeps_the_last_usable_desk() {
        let initial = Screens {
            native_per_desk: 1.0,
            displays: vec![Display {
                id: 1,
                bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
                pixel_scale: 1.0,
                ui_scale: 1.0,
                primary: true,
                name: "Monitor".into(),
            }],
        };
        let source = Arc::new(Mutex::new(initial.clone()));
        let read = source.clone();
        let mut updates = watch(
            initial.clone(),
            move || read.lock().unwrap().clone(),
            Duration::from_millis(5),
        );
        let mut resized = initial.clone();
        resized.displays[0].bounds = Rect::new(-2560.0, 0.0, 2560.0, 1440.0);
        resized.displays[0].ui_scale = 1.5;
        resized.native_per_desk = 1.5;
        let mut plugged = resized.clone();
        plugged.displays.push(initial.displays[0].clone());
        for next in [resized, plugged, initial.clone()] {
            *source.lock().unwrap() = next.clone();
            tokio::time::timeout(Duration::from_secs(2), updates.changed())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(*updates.borrow_and_update(), next);
        }
        source.lock().unwrap().displays.clear();
        assert!(
            tokio::time::timeout(Duration::from_millis(40), updates.changed())
                .await
                .is_err()
        );
        assert_eq!(*updates.borrow(), initial);
        *source.lock().unwrap() = initial;
        assert!(
            tokio::time::timeout(Duration::from_millis(40), updates.changed())
                .await
                .is_err()
        );
    }
}
