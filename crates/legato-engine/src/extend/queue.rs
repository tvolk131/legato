//! Bounded encoder output. Never block the capture callback; after a dropped encoded
//! picture, discard its dependants until a keyframe fits in the queue.

use std::sync::{Arc, Mutex};

use legato_screen::mac::EncodedFrame;
use tokio::sync::mpsc;

const BACKLOG: usize = 3;

#[derive(Default)]
struct State {
    needs_keyframe: bool,
    missed: u32,
}

pub(super) struct Sender {
    frames: mpsc::Sender<EncodedFrame>,
    state: Arc<Mutex<State>>,
}

pub(super) struct Receiver {
    frames: mpsc::Receiver<EncodedFrame>,
    state: Arc<Mutex<State>>,
}

pub(super) fn channel() -> (Sender, Receiver) {
    let (tx, rx) = mpsc::channel(BACKLOG);
    let state = Arc::new(Mutex::new(State::default()));
    (
        Sender {
            frames: tx,
            state: state.clone(),
        },
        Receiver { frames: rx, state },
    )
}

impl Sender {
    pub(super) fn push(&self, frame: EncodedFrame) {
        let mut state = self.state.lock().unwrap();
        if state.needs_keyframe && !frame.keyframe {
            state.missed = state.missed.saturating_add(1);
            return;
        }
        match self.frames.try_send(frame) {
            Ok(()) => state.needs_keyframe = false,
            Err(mpsc::error::TrySendError::Full(_)) => {
                state.needs_keyframe = true;
                state.missed = state.missed.saturating_add(1);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }
}

impl Receiver {
    pub(super) async fn recv(&mut self) -> Option<EncodedFrame> {
        self.frames.recv().await
    }

    pub(super) fn needs_keyframe(&self) -> bool {
        self.state.lock().unwrap().needs_keyframe
    }

    pub(super) fn take_missed(&self) -> u32 {
        std::mem::take(&mut self.state.lock().unwrap().missed)
    }
}

/// Apply backpressure before polling either stream opening or a network write.
pub(super) async fn while_sending<T>(
    set_backlogged: impl Fn(bool),
    send: impl Future<Output = T>,
) -> T {
    set_backlogged(true);
    let result = send.await;
    set_backlogged(false);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, time::Duration};

    fn frame(n: u64, keyframe: bool) -> EncodedFrame {
        EncodedFrame {
            data: vec![n as u8],
            keyframe,
            pts: Duration::from_millis(n),
            shown_at: None,
            track: 0,
        }
    }

    #[tokio::test]
    async fn stalled_write_is_backlogged_and_cannot_grow_the_queue() {
        let (tx, mut rx) = channel();
        tx.push(frame(0, true));
        let first = rx.recv().await.unwrap();
        let backlogged = Cell::new(false);
        let (release, blocked) = tokio::sync::oneshot::channel();
        let started = tokio::sync::Notify::new();
        let sending = while_sending(|b| backlogged.set(b), async {
            assert!(backlogged.get());
            started.notify_one();
            blocked.await.unwrap();
            first
        });
        tokio::pin!(sending);
        tokio::select! {
            _ = &mut sending => panic!("write should still be blocked"),
            _ = started.notified() => {}
        }
        // Even callbacks already in flight, or keyframes which bypass the encoder's
        // backpressure, have a hard memory bound.
        for n in 1..=10_000 {
            tx.push(frame(n, n % 5 == 0));
        }
        assert!(backlogged.get());
        assert_eq!(rx.frames.len(), BACKLOG);
        assert_eq!(rx.take_missed(), 10_000 - BACKLOG as u32);
        assert_eq!(rx.take_missed(), 0);
        assert!(rx.needs_keyframe());
        release.send(()).unwrap();
        assert_eq!(sending.await.pts, Duration::ZERO);
        assert!(!backlogged.get());
        for n in 1..=3 {
            assert_eq!(rx.recv().await.unwrap(), frame(n, false));
        }
        // Space alone does not make dependent pictures safe. A fresh keyframe does.
        tx.push(frame(10_001, false));
        assert!(rx.frames.is_empty());
        tx.push(frame(10_002, true));
        tx.push(frame(10_003, false));
        assert!(!rx.needs_keyframe());
        assert_eq!(rx.take_missed(), 1);
        assert_eq!(rx.recv().await.unwrap(), frame(10_002, true));
        assert_eq!(rx.recv().await.unwrap(), frame(10_003, false));
        drop(tx);
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn tracks_recover_independently_and_dropped_keyframes_do_not_clear_the_gap() {
        let (sharp, mut sharp_rx) = channel();
        let (moving, mut moving_rx) = channel();
        for n in 0..=3 {
            sharp.push(frame(n, true));
        }
        assert!(sharp_rx.needs_keyframe());
        moving.push(frame(0, true));
        moving.push(frame(1, false));
        assert_eq!(moving_rx.recv().await.unwrap(), frame(0, true));
        assert_eq!(moving_rx.recv().await.unwrap(), frame(1, false));
        assert!(!moving_rx.needs_keyframe());
        sharp_rx.recv().await.unwrap();
        sharp.push(frame(4, true));
        assert!(!sharp_rx.needs_keyframe());
        assert_eq!(sharp_rx.take_missed(), 1);
        drop(sharp_rx);
        // Closing the viewer must never block a capture callback either.
        sharp.push(frame(5, true));
        let backlogged = Cell::new(false);
        let result = while_sending(|b| backlogged.set(b), async { Err::<(), _>("closed") }).await;
        assert_eq!(result, Err("closed"));
        assert!(!backlogged.get());
    }
}
