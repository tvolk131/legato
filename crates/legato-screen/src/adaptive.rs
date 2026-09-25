//! Adaptive quality: the full-size picture while the screen is still or changing a little
//! (typing, a blinking cursor), a smaller, much quicker one while a lot of it moves
//! (dragging a window, scrolling). When the movement stops, the latest picture is sent at
//! full size again, bringing it back into focus.
//!
//! Each size is its own track (its own stream and H.264 sequence), so switching costs no
//! keyframes: each track's next frame simply follows on from its last one. The viewer
//! shows whichever picture is newest.

use std::time::{Duration, Instant};

/// The track at the stream's full size, and the smaller one used while moving.
pub const SHARP: u8 = 0;
pub const MOVING: u8 = 1;

/// Pictures changing at least this share of the screen count as movement, when two come
/// within [`SUSTAINED`] of each other. A single one (switching tabs, opening a window)
/// stays sharp, rather than flashing soft then sharpening.
pub const MOVEMENT: f64 = 0.03;
pub const SUSTAINED: Duration = Duration::from_millis(100);
/// While moving, pictures changing at least this share keep it moving.
pub const STILL_BELOW: f64 = 0.01;
/// How long things must stay still before the picture is sharpened.
pub const SETTLE: Duration = Duration::from_millis(150);
/// After sharpening, if the screen stays still, the picture is encoded once more to touch
/// up what the encoder's rate control left soft.
pub const TOUCH_UP: Duration = Duration::from_millis(300);

/// What to do with a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Encode the new picture on `track`, as a keyframe if `keyframe`.
    Encode { track: u8, keyframe: bool },
    /// Encode the latest picture on the sharp track: the screen has been still for a
    /// moment. It has been on screen a while, so it's current as of now.
    Sharpen { keyframe: bool },
    /// Not now: the sharp track is at its frame-rate limit (it's slower to encode than
    /// the display runs). The latest picture goes out on a later tick if nothing newer
    /// comes.
    Wait,
}

/// Decides which track each picture goes on.
#[derive(Debug, Clone)]
pub struct Policy {
    adaptive: bool,
    moving: bool,
    /// The last picture with a big change, and the last while moving.
    last_big: Option<Instant>,
    last_movement: Option<Instant>,
    /// The sharp track's frame-rate limit.
    sharp_interval: Duration,
    last_sharp: Option<Instant>,
    /// A picture waiting for the sharp track.
    pending: bool,
    touch_up_at: Option<Instant>,
    /// Tracks whose next frame must be a keyframe.
    keyframe: [bool; 2],
}

impl Policy {
    /// Everything on the sharp track, as fast as pictures come.
    pub fn fixed() -> Self {
        Self::new(false, Duration::ZERO)
    }

    /// Adaptive quality, with the sharp track kept to `sharp_fps`.
    pub fn adaptive(sharp_fps: u32) -> Self {
        Self::new(true, Duration::from_secs_f64(1.0 / sharp_fps.max(1) as f64))
    }

    fn new(adaptive: bool, sharp_interval: Duration) -> Self {
        Self {
            adaptive,
            moving: false,
            last_big: None,
            last_movement: None,
            sharp_interval,
            last_sharp: None,
            pending: false,
            touch_up_at: None,
            keyframe: [true, true],
        }
    }

    pub fn is_moving(&self) -> bool {
        self.moving
    }

    /// The track pictures go on right now.
    pub fn current(&self) -> u8 {
        if self.moving { MOVING } else { SHARP }
    }

    /// A viewer wants a keyframe on `track`. If that's the track in use, the latest
    /// picture should go out as one now (the screen may be still, with nothing new
    /// coming); otherwise the track's next frame will be one.
    pub fn keyframe_now(&mut self, track: u8) -> Option<Decision> {
        *self.keyframe.get_mut(track as usize)? = true;
        (track == self.current()).then(|| self.encode(track))
    }

    /// A new picture, with `changed` (0 to 1) of the screen different from the last.
    pub fn on_frame(&mut self, now: Instant, changed: f64) -> Decision {
        self.touch_up_at = None;
        if self.adaptive {
            if changed >= MOVEMENT {
                let sustained = self
                    .last_big
                    .is_some_and(|t| now.saturating_duration_since(t) <= SUSTAINED);
                self.last_big = Some(now);
                self.moving |= sustained;
            }
            if self.moving && changed >= STILL_BELOW {
                self.last_movement = Some(now);
            }
        }
        if self.moving {
            self.pending = false;
            return self.encode(MOVING);
        }
        if !self.sharp_ready(now) {
            self.pending = true;
            return Decision::Wait;
        }
        self.last_sharp = Some(now);
        self.encode(SHARP)
    }

    /// Time passing. Says what to do with the latest picture, if anything: sharpen it
    /// once things have been still long enough, touch it up, or send one that had to
    /// wait for the sharp track.
    pub fn on_tick(&mut self, now: Instant) -> Option<Decision> {
        if self.moving {
            let settled = self
                .last_movement
                .is_none_or(|t| now.saturating_duration_since(t) >= SETTLE);
            if !settled {
                return None;
            }
            self.moving = false;
            self.pending = false;
            self.last_sharp = Some(now);
            self.touch_up_at = Some(now + TOUCH_UP);
            return Some(self.sharpen());
        }
        if self.pending && self.sharp_ready(now) {
            self.pending = false;
            self.last_sharp = Some(now);
            return Some(self.encode(SHARP));
        }
        if self.touch_up_at.is_some_and(|at| now >= at) {
            self.touch_up_at = None;
            self.last_sharp = Some(now);
            return Some(self.sharpen());
        }
        None
    }

    /// The picture a decision was for couldn't be sent (the network is backed up). A
    /// sharp one is retried on a later tick, so the viewer isn't left with a stale
    /// picture when the screen goes still; a moving one is sharpened later anyway.
    pub fn deferred(&mut self, decision: Decision) {
        match decision {
            Decision::Encode { track, keyframe } => {
                if keyframe {
                    self.keyframe[track as usize] = true;
                }
                if track == SHARP {
                    self.pending = true;
                    self.last_sharp = None;
                }
            }
            Decision::Sharpen { keyframe } => {
                self.keyframe[SHARP as usize] |= keyframe;
                self.pending = true;
                self.last_sharp = None;
            }
            Decision::Wait => {}
        }
    }

    fn sharp_ready(&self, now: Instant) -> bool {
        self.last_sharp
            .is_none_or(|t| now.saturating_duration_since(t) >= self.sharp_interval)
    }

    fn encode(&mut self, track: u8) -> Decision {
        let keyframe = std::mem::take(&mut self.keyframe[track as usize]);
        Decision::Encode { track, keyframe }
    }

    fn sharpen(&mut self) -> Decision {
        let keyframe = std::mem::take(&mut self.keyframe[SHARP as usize]);
        Decision::Sharpen { keyframe }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn encode(track: u8, keyframe: bool) -> Decision {
        Decision::Encode { track, keyframe }
    }

    #[test]
    fn small_changes_stay_sharp_and_big_ones_switch_straight_away() {
        let t0 = Instant::now();
        let mut p = Policy::adaptive(60);
        assert_eq!(
            p.on_frame(t0, 0.002),
            encode(SHARP, true),
            "starts with a keyframe"
        );
        assert_eq!(
            p.on_frame(t0 + ms(20), 0.004),
            encode(SHARP, false),
            "typing"
        );
        // A window starts moving: from the second big change on, the quick track.
        assert_eq!(p.on_frame(t0 + ms(40), 0.12), encode(SHARP, false));
        assert_eq!(p.on_frame(t0 + ms(57), 0.12), encode(MOVING, true));
        assert!(p.is_moving());
        assert_eq!(p.on_frame(t0 + ms(64), 0.10), encode(MOVING, false));
    }

    #[test]
    fn a_single_big_change_stays_sharp() {
        let t0 = Instant::now();
        let mut p = Policy::adaptive(60);
        p.on_frame(t0, 0.001);
        // Switching tabs, then a keystroke, then another tab a while later.
        assert_eq!(p.on_frame(t0 + ms(500), 0.6), encode(SHARP, false));
        assert_eq!(p.on_frame(t0 + ms(700), 0.002), encode(SHARP, false));
        assert_eq!(p.on_frame(t0 + ms(900), 0.6), encode(SHARP, false));
        assert!(!p.is_moving());
    }

    #[test]
    fn it_sharpens_once_things_have_been_still_for_a_moment_then_touches_up() {
        let t0 = Instant::now();
        let mut p = Policy::adaptive(60);
        p.on_frame(t0, 0.2);
        p.on_frame(t0 + ms(16), 0.2);
        assert!(p.is_moving());
        // Small changes while moving keep it moving (the end of a drag)...
        assert_eq!(p.on_frame(t0 + ms(32), 0.015), encode(MOVING, false));
        assert_eq!(p.on_tick(t0 + ms(100)), None, "not settled yet");
        // ...and tiny ones don't.
        assert_eq!(p.on_frame(t0 + ms(120), 0.001), encode(MOVING, false));
        let settled = t0 + ms(32) + SETTLE;
        assert_eq!(
            p.on_tick(settled),
            Some(Decision::Sharpen { keyframe: false }),
            "carries on from the first picture, which was sharp"
        );
        assert!(!p.is_moving());
        assert_eq!(p.on_tick(settled + ms(10)), None);
        assert_eq!(
            p.on_tick(settled + TOUCH_UP),
            Some(Decision::Sharpen { keyframe: false })
        );
        assert_eq!(p.on_tick(settled + TOUCH_UP * 3), None, "only once");
    }

    #[test]
    fn switching_back_and_forth_needs_no_keyframes() {
        let t0 = Instant::now();
        let mut p = Policy::adaptive(60);
        p.on_frame(t0, 0.001);
        p.on_frame(t0 + ms(20), 0.3);
        p.on_frame(t0 + ms(30), 0.3);
        p.on_tick(t0 + ms(200));
        p.on_frame(t0 + ms(300), 0.3);
        assert_eq!(p.on_frame(t0 + ms(310), 0.3), encode(MOVING, false));
        assert_eq!(
            p.on_tick(t0 + ms(500)),
            Some(Decision::Sharpen { keyframe: false })
        );
    }

    #[test]
    fn a_new_picture_cancels_the_touch_up() {
        let t0 = Instant::now();
        let mut p = Policy::adaptive(60);
        p.on_frame(t0, 0.5);
        p.on_frame(t0 + ms(10), 0.5);
        p.on_tick(t0 + ms(10) + SETTLE);
        assert_eq!(p.on_frame(t0 + ms(200), 0.001), encode(SHARP, false));
        assert_eq!(p.on_tick(t0 + ms(10) + SETTLE + TOUCH_UP), None);
    }

    #[test]
    fn the_sharp_track_keeps_to_its_frame_rate_and_sends_the_last_picture_late() {
        let t0 = Instant::now();
        // The display runs at 120 Hz but 4K encodes at 60.
        let mut p = Policy::adaptive(60);
        assert_eq!(p.on_frame(t0, 0.001), encode(SHARP, true));
        assert_eq!(p.on_frame(t0 + ms(8), 0.001), Decision::Wait);
        assert_eq!(p.on_tick(t0 + ms(12)), None, "not its turn yet");
        assert_eq!(
            p.on_tick(t0 + ms(17)),
            Some(encode(SHARP, false)),
            "the waiting picture isn't lost"
        );
        assert_eq!(p.on_tick(t0 + ms(40)), None);
    }

    #[test]
    fn keyframes_go_on_the_track_asked_for() {
        let t0 = Instant::now();
        let mut p = Policy::adaptive(60);
        p.on_frame(t0, 0.001);
        assert_eq!(
            p.keyframe_now(SHARP),
            Some(encode(SHARP, true)),
            "re-sent now"
        );
        // The moving track isn't in use: its next frame will be a keyframe.
        assert_eq!(p.keyframe_now(MOVING), None);
        assert_eq!(p.on_frame(t0 + ms(40), 0.001), encode(SHARP, false));
        p.on_frame(t0 + ms(60), 0.5);
        assert_eq!(p.on_frame(t0 + ms(70), 0.5), encode(MOVING, true));
        assert_eq!(p.keyframe_now(9), None, "no such track");
    }

    #[test]
    fn a_picture_held_up_by_the_network_is_sent_once_it_clears() {
        let t0 = Instant::now();
        let mut p = Policy::adaptive(60);
        let first = p.on_frame(t0, 0.001);
        p.deferred(first);
        assert_eq!(p.on_tick(t0 + ms(5)), Some(encode(SHARP, true)));
        // A deferred sharpen comes back too.
        p.on_frame(t0 + ms(20), 0.5);
        p.on_frame(t0 + ms(25), 0.5);
        let sharpen = p.on_tick(t0 + ms(25) + SETTLE).unwrap();
        assert_eq!(sharpen, Decision::Sharpen { keyframe: false });
        p.deferred(sharpen);
        assert_eq!(p.on_tick(t0 + ms(200)), Some(encode(SHARP, false)));
    }

    #[test]
    fn fixed_quality_never_switches_or_waits() {
        let t0 = Instant::now();
        let mut p = Policy::fixed();
        assert_eq!(p.on_frame(t0, 1.0), encode(SHARP, true));
        assert_eq!(p.on_frame(t0 + ms(1), 1.0), encode(SHARP, false));
        assert_eq!(p.on_tick(t0 + ms(500)), None);
        assert_eq!(p.keyframe_now(SHARP), Some(encode(SHARP, true)));
    }
}
