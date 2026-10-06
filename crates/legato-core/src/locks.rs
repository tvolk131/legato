//! Caps Lock, shared by connected machines: one state, whichever keyboard changes it.
//!
//! Each machine watches its own Caps Lock and tells its peers when it changes; a peer
//! that hears of a newer change sets its own to match, light and all. Every change
//! carries a counter (a Lamport clock), so when two changes cross on the way, the
//! newest wins on both machines, and a tie goes to the machine with the lower id.

use legato_proto::Control;

/// How many reads of this machine's Caps Lock to wait for a peer's change to take
/// effect here (setting it can lag: on Windows it's a key press) before taking what's
/// read as a change of its own.
const SETTLE_READS: u8 = 5;

/// This machine's Caps Lock, as shared with its peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapsLock {
    on: bool,
    /// The newest change seen, here or from a peer.
    clock: u64,
    /// Reads left to wait for a peer's change to show here.
    settling: u8,
}

impl CapsLock {
    /// Starts from this machine's Caps Lock as it is.
    pub fn new(on: bool) -> Self {
        Self {
            on,
            clock: 0,
            settling: 0,
        }
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// This machine's Caps Lock reads `on` now. If it changed, returns what to tell
    /// the peers.
    pub fn local(&mut self, on: bool) -> Option<Control> {
        if on == self.on {
            self.settling = 0;
            return None;
        }
        if self.settling > 0 {
            // A peer's change, still being set here.
            self.settling -= 1;
            return None;
        }
        self.on = on;
        self.clock += 1;
        Some(self.announce())
    }

    /// What to tell a peer that just connected.
    pub fn announce(&self) -> Control {
        Control::CapsLock {
            on: self.on,
            clock: self.clock,
        }
    }

    /// A peer's Caps Lock is `on` as of its change `clock`; `peer_wins_ties` says
    /// whose change wins when both are as new. Returns what to set this machine's
    /// Caps Lock to, if the peer's is newer and different.
    pub fn remote(&mut self, on: bool, clock: u64, peer_wins_ties: bool) -> Option<bool> {
        let newer = clock > self.clock || (clock == self.clock && peer_wins_ties);
        self.clock = self.clock.max(clock);
        if !newer || on == self.on {
            return None;
        }
        self.on = on;
        self.settling = SETTLE_READS;
        Some(on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Delivers `msg` (from a machine whose id is lower when `from_lower`) to `to`.
    fn deliver(to: &mut CapsLock, msg: &Control, from_lower: bool) -> Option<bool> {
        match *msg {
            Control::CapsLock { on, clock } => to.remote(on, clock, from_lower),
            _ => None,
        }
    }

    #[test]
    fn a_change_here_is_told_to_peers_and_set_there() {
        let (mut pc, mut mac) = (CapsLock::new(false), CapsLock::new(false));
        assert_eq!(pc.local(false), None, "no change, nothing to say");
        let msg = pc.local(true).unwrap();
        assert_eq!(deliver(&mut mac, &msg, true), Some(true));
        // Setting it there reads back as no change: nothing echoes.
        assert_eq!(mac.local(true), None);
        assert_eq!(deliver(&mut mac, &msg, true), None, "already on");
    }

    #[test]
    fn a_peers_change_that_takes_a_moment_to_show_here_isnt_sent_back() {
        let mut mac = CapsLock::new(false);
        let msg = CapsLock::new(false).local(true).unwrap();
        assert_eq!(deliver(&mut mac, &msg, true), Some(true));
        // Still reading off for a little while: not a change of its own.
        for _ in 0..3 {
            assert_eq!(mac.local(false), None);
        }
        assert_eq!(mac.local(true), None, "now it shows");
        // If it never shows (it couldn't be set), what's read wins in the end.
        let _ = deliver(&mut mac, &CapsLock::new(true).local(false).unwrap(), true);
        let sent = (0..10).find_map(|_| mac.local(true));
        assert!(matches!(sent, Some(Control::CapsLock { on: true, .. })));
    }

    #[test]
    fn on_connecting_both_settle_on_the_same_state() {
        // Neither has changed it since starting: the lower id's state wins.
        let (mut low, mut high) = (CapsLock::new(true), CapsLock::new(false));
        let (from_low, from_high) = (low.announce(), high.announce());
        assert_eq!(deliver(&mut high, &from_low, true), Some(true));
        assert_eq!(deliver(&mut low, &from_high, false), None);
        assert_eq!((low.is_on(), high.is_on()), (true, true));
        // A change made since wins over a connecting peer's older state.
        let mut changed = CapsLock::new(false);
        let _ = changed.local(true);
        assert_eq!(
            deliver(&mut changed, &CapsLock::new(false).announce(), true),
            None
        );
        assert!(changed.is_on());
    }

    #[test]
    fn crossing_changes_end_up_the_same_on_both() {
        let (mut low, mut high) = (CapsLock::new(false), CapsLock::new(true));
        // Each changes before hearing of the other's change.
        let from_low = low.local(true).unwrap();
        let from_high = high.local(false).unwrap();
        let _ = deliver(&mut high, &from_low, true);
        let _ = deliver(&mut low, &from_high, false);
        assert_eq!(low.is_on(), high.is_on(), "both agree");
        assert!(low.is_on(), "the lower id's change wins the tie");
        // Once that shows on both, the next change, on either, is newer than both.
        assert_eq!(high.local(true), None);
        let msg = high.local(false).unwrap();
        assert_eq!(deliver(&mut low, &msg, false), Some(false));
    }
}
