//! Selects the peer whose input the receiver may inject. Motion and reliable input
//! must share the same owner, even when packets from an earlier driver arrive late.

use std::time::Instant;

use legato_core::{Inject, Receiver, ReceiverConfig};
use legato_net::EndpointId;
use legato_proto::{Control, Datagram};

pub(super) struct InputReceiver {
    pub(super) receiver: Receiver,
    // Retained after local activity so SendYield goes back to the correct peer.
    pub(super) driver: Option<EndpointId>,
}

impl InputReceiver {
    pub(super) fn new(config: ReceiverConfig) -> Self {
        Self {
            receiver: Receiver::new(config),
            driver: None,
        }
    }

    pub(super) fn control(
        &mut self,
        peer: EndpointId,
        now: Instant,
        msg: Control,
        out: &mut Vec<Inject>,
    ) {
        if matches!(msg, Control::Enter { .. }) {
            self.driver = Some(peer);
        }
        if self.driver == Some(peer) {
            self.receiver.control(now, msg, out);
        }
    }

    /// Returns whether the packet belongs to the driver (for motion diagnostics).
    pub(super) fn datagram(
        &mut self,
        peer: EndpointId,
        msg: Datagram,
        out: &mut Vec<Inject>,
    ) -> bool {
        if self.driver != Some(peer) {
            return false;
        }
        self.receiver.datagram(msg, out);
        true
    }

    pub(super) fn disconnected(&mut self, peer: EndpointId, out: &mut Vec<Inject>) {
        if self.driver == Some(peer) {
            self.receiver.disconnected(out);
            self.driver = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use legato_proto::{Button, Point};

    fn peer(n: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[n; 32]).public()
    }

    fn enter(input: &mut InputReceiver, peer: EndpointId, seq: u32, out: &mut Vec<Inject>) {
        input.control(
            peer,
            Instant::now(),
            Control::Enter {
                seq,
                pos: Point::default(),
            },
            out,
        );
    }

    fn motion(seq: u32) -> Datagram {
        Datagram::Motion {
            seq,
            pos: Point::new(10.0, 20.0),
        }
    }

    #[test]
    fn late_motion_from_previous_driver_cannot_move_or_poison_sequence() {
        let mut input = InputReceiver::new(Default::default());
        let mut out = Vec::new();
        enter(&mut input, peer(1), 100, &mut out);
        enter(&mut input, peer(2), 10, &mut out);
        out.clear();
        assert!(!input.datagram(peer(1), motion(101), &mut out));
        assert!(out.is_empty());
        assert!(input.datagram(peer(2), motion(11), &mut out));
        assert_eq!(
            out,
            [Inject::MoveTo {
                pos: Point::new(10.0, 20.0)
            }]
        );
    }

    #[test]
    fn motion_requires_an_active_visit_and_a_fresh_sequence() {
        let mut input = InputReceiver::new(Default::default());
        let mut out = Vec::new();
        assert!(!input.datagram(peer(1), motion(1), &mut out));
        assert!(out.is_empty());
        enter(&mut input, peer(1), u32::MAX, &mut out);
        out.clear();
        input.datagram(peer(1), motion(u32::MAX - 1), &mut out);
        assert!(out.is_empty());
        input.datagram(peer(1), motion(0), &mut out);
        assert_eq!(out.len(), 1, "sequence wraps normally");
        out.clear();
        input.datagram(peer(1), motion(0), &mut out);
        input.datagram(
            peer(1),
            Datagram::Pointer {
                seq: 100,
                pos: Point::default(),
            },
            &mut out,
        );
        assert!(
            out.is_empty(),
            "duplicates and pointer reports aren't injected"
        );
        for end in [
            Control::Leave,
            Control::Enter {
                seq: 0,
                pos: Point::default(),
            },
        ] {
            input.control(peer(1), Instant::now(), end.clone(), &mut out);
            if matches!(end, Control::Enter { .. }) {
                input.receiver.local_activity(&mut out);
                assert!(out.contains(&Inject::SendYield));
                assert_eq!(input.driver, Some(peer(1)));
            }
            out.clear();
            input.datagram(peer(1), motion(200), &mut out);
            assert!(out.is_empty());
        }
    }

    #[test]
    fn handover_releases_held_input_and_ignores_the_old_peers_controls_and_disconnect() {
        let mut input = InputReceiver::new(Default::default());
        let mut out = Vec::new();
        let now = Instant::now();
        enter(&mut input, peer(1), 0, &mut out);
        input.control(
            peer(1),
            now,
            Control::Key {
                usage: 4,
                down: true,
            },
            &mut out,
        );
        input.control(
            peer(1),
            now,
            Control::Button {
                button: Button::Left,
                down: true,
                pos: Point::default(),
            },
            &mut out,
        );
        out.clear();
        enter(&mut input, peer(2), 0, &mut out);
        assert!(out.contains(&Inject::Key {
            usage: 4,
            down: false,
            repeat: false
        }));
        assert!(out.iter().any(|action| matches!(
            action,
            Inject::Button {
                button: Button::Left,
                down: false,
                ..
            }
        )));
        input.control(
            peer(2),
            now,
            Control::Key {
                usage: 5,
                down: true,
            },
            &mut out,
        );
        out.clear();
        input.control(peer(1), now, Control::Leave, &mut out);
        input.control(
            peer(1),
            now,
            Control::Key {
                usage: 5,
                down: false,
            },
            &mut out,
        );
        input.disconnected(peer(1), &mut out);
        assert!(out.is_empty());
        assert!(input.receiver.is_controlled());
        input.disconnected(peer(2), &mut out);
        assert_eq!(
            out,
            [Inject::Key {
                usage: 5,
                down: false,
                repeat: false
            }]
        );
        assert_eq!(input.driver, None);
        assert!(!input.receiver.is_controlled());
        out.clear();
        input.datagram(peer(2), motion(1), &mut out);
        assert!(out.is_empty());
    }
}
