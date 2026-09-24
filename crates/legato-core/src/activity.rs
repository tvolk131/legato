//! Telling deliberate use of a machine's own keyboard and mouse from a brushed trackpad or
//! a bumped mouse, so control is only taken back when the user means it.

use std::time::{Duration, Instant};

/// Physical input seen on this machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LocalInput {
    /// Pointer motion, in native units.
    Motion { dx: f64, dy: f64 },
    /// A key, click or scroll: always deliberate.
    Other,
}

#[derive(Debug, Clone)]
pub struct ActivityFilter {
    /// Pointer travel that counts as deliberate.
    pub threshold: f64,
    /// Motion pauses longer than this start the count over.
    pub window: Duration,
    moved: f64,
    last: Option<Instant>,
}

impl Default for ActivityFilter {
    fn default() -> Self {
        Self::new(12.0, Duration::from_millis(300))
    }
}

impl ActivityFilter {
    pub fn new(threshold: f64, window: Duration) -> Self {
        Self {
            threshold,
            window,
            moved: 0.0,
            last: None,
        }
    }

    /// Returns true if this input, together with recent input, is deliberate.
    pub fn feed(&mut self, now: Instant, input: LocalInput) -> bool {
        match input {
            LocalInput::Other => {
                self.moved = 0.0;
                true
            }
            LocalInput::Motion { dx, dy } => {
                if self
                    .last
                    .is_none_or(|t| now.saturating_duration_since(t) > self.window)
                {
                    self.moved = 0.0;
                }
                self.last = Some(now);
                self.moved += dx.abs() + dy.abs();
                if self.moved >= self.threshold {
                    self.moved = 0.0;
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_jitter_is_ignored_and_real_motion_counts() {
        let t = Instant::now();
        let mut f = ActivityFilter::default();
        assert!(!f.feed(t, LocalInput::Motion { dx: 2.0, dy: 1.0 }));
        // A pause resets the count.
        assert!(!f.feed(
            t + Duration::from_secs(1),
            LocalInput::Motion { dx: 5.0, dy: 0.0 }
        ));
        assert!(!f.feed(
            t + Duration::from_millis(1010),
            LocalInput::Motion { dx: 5.0, dy: 0.0 }
        ));
        assert!(f.feed(
            t + Duration::from_millis(1020),
            LocalInput::Motion { dx: 3.0, dy: 0.0 }
        ));
        assert!(f.feed(t, LocalInput::Other));
    }
}
