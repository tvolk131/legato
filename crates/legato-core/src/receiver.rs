//! The controlled side: turns control messages and motion datagrams into injection
//! actions, owns key repeat and click counting, and hands control back when the user
//! touches this machine's own keyboard or mouse.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use legato_proto::{Button, Control, Datagram, Point, Scroll};

use crate::keymap;

#[derive(Debug, Clone, PartialEq)]
pub struct ReceiverConfig {
    /// Delay before a held key starts repeating. Use the OS setting.
    pub repeat_delay: Duration,
    /// Interval between repeats. Use the OS setting.
    pub repeat_interval: Duration,
    /// Maximum time between clicks of a double-click. Use the OS setting.
    pub double_click_interval: Duration,
    /// Maximum pointer travel (native units) between clicks of a double-click.
    pub double_click_distance: f64,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            repeat_delay: Duration::from_millis(500),
            repeat_interval: Duration::from_millis(33),
            double_click_interval: Duration::from_millis(500),
            double_click_distance: 4.0,
        }
    }
}

/// What the injection backend should do.
#[derive(Debug, Clone, PartialEq)]
pub enum Inject {
    /// Move the cursor to `pos` (native coordinates). Button-held motion must be injected
    /// as a drag.
    MoveTo {
        pos: Point,
    },
    Key {
        usage: u16,
        down: bool,
        /// A synthesized auto-repeat.
        repeat: bool,
    },
    Button {
        button: Button,
        down: bool,
        pos: Point,
        /// 1 for a single click, 2 for a double-click, ...
        clicks: u8,
    },
    Scroll(Scroll),
    /// Tell the controller to give this machine its cursor back.
    SendYield,
}

#[derive(Debug, Clone, Copy)]
struct Repeat {
    usage: u16,
    next: Instant,
}

#[derive(Debug, Clone, Copy)]
struct LastClick {
    button: Button,
    at: Instant,
    pos: Point,
    clicks: u8,
}

#[derive(Debug)]
pub struct Receiver {
    config: ReceiverConfig,
    /// The cursor is ours to drive: between `Enter` and `Leave`/yield.
    entered: bool,
    last_seq: u32,
    cursor: Point,
    keys: HashSet<u16>,
    buttons: HashSet<Button>,
    button_clicks: [u8; 5],
    repeat: Option<Repeat>,
    last_click: Option<LastClick>,
}

impl Receiver {
    pub fn new(config: ReceiverConfig) -> Self {
        Self {
            config,
            entered: false,
            last_seq: 0,
            cursor: Point::default(),
            keys: HashSet::new(),
            buttons: HashSet::new(),
            button_clicks: [1; 5],
            repeat: None,
            last_click: None,
        }
    }

    pub fn set_config(&mut self, config: ReceiverConfig) {
        self.config = config;
    }

    /// Whether a remote controller is currently driving this machine.
    pub fn is_controlled(&self) -> bool {
        self.entered
    }

    pub fn control(&mut self, now: Instant, msg: Control, out: &mut Vec<Inject>) {
        match msg {
            Control::Enter { seq, pos } => {
                self.release_all(out);
                self.entered = true;
                self.last_seq = seq;
                self.cursor = pos;
                self.last_click = None;
                out.push(Inject::MoveTo { pos });
            }
            Control::Leave => {
                self.release_all(out);
                self.entered = false;
            }
            Control::Key { usage, down } => self.key(now, usage, down, out),
            Control::Button { button, down, pos } => self.button(now, button, down, pos, out),
            Control::Scroll(scroll) => {
                if self.entered {
                    out.push(Inject::Scroll(scroll));
                }
            }
            Control::Hello(_)
            | Control::Screens(_)
            | Control::Yield
            | Control::Placement { .. }
            | Control::ControlMode(_) => {}
        }
    }

    pub fn datagram(&mut self, msg: Datagram, out: &mut Vec<Inject>) {
        match msg {
            Datagram::Motion { seq, pos } => {
                // Newer wins (with wrap-around); anything at or before the last one we used,
                // including motion from before the latest Enter, is stale.
                let newer = (seq.wrapping_sub(self.last_seq) as i32) > 0;
                if !self.entered || !newer {
                    return;
                }
                self.last_seq = seq;
                if pos != self.cursor {
                    self.cursor = pos;
                    out.push(Inject::MoveTo { pos });
                }
            }
        }
    }

    /// Physical (not injected) input happened on this machine.
    pub fn local_activity(&mut self, out: &mut Vec<Inject>) {
        if self.entered {
            self.release_all(out);
            self.entered = false;
            out.push(Inject::SendYield);
        }
    }

    /// The connection to the controller dropped.
    pub fn disconnected(&mut self, out: &mut Vec<Inject>) {
        self.release_all(out);
        self.entered = false;
    }

    /// When [`Receiver::tick`] next needs to run, if ever.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.repeat.map(|r| r.next)
    }

    /// Emits due key repeats.
    pub fn tick(&mut self, now: Instant, out: &mut Vec<Inject>) {
        if let Some(r) = &mut self.repeat
            && now >= r.next
        {
            out.push(Inject::Key {
                usage: r.usage,
                down: true,
                repeat: true,
            });
            // Don't try to catch up on missed repeats after a stall.
            r.next = now + self.config.repeat_interval;
        }
    }

    fn key(&mut self, now: Instant, usage: u16, down: bool, out: &mut Vec<Inject>) {
        if down {
            if !self.entered || !self.keys.insert(usage) {
                return;
            }
            out.push(Inject::Key {
                usage,
                down: true,
                repeat: false,
            });
            // Like a real keyboard, only the most recently pressed key repeats.
            if keymap::repeats(usage) {
                self.repeat = Some(Repeat {
                    usage,
                    next: now + self.config.repeat_delay,
                });
            }
        } else {
            if !self.keys.remove(&usage) {
                return;
            }
            if self.repeat.is_some_and(|r| r.usage == usage) {
                self.repeat = None;
            }
            out.push(Inject::Key {
                usage,
                down: false,
                repeat: false,
            });
        }
    }

    fn button(
        &mut self,
        now: Instant,
        button: Button,
        down: bool,
        pos: Point,
        out: &mut Vec<Inject>,
    ) {
        if down {
            if !self.entered || !self.buttons.insert(button) {
                return;
            }
            let clicks = match self.last_click {
                Some(last)
                    if last.button == button
                        && now.saturating_duration_since(last.at)
                            <= self.config.double_click_interval
                        && distance(last.pos, pos) <= self.config.double_click_distance =>
                {
                    last.clicks.saturating_add(1)
                }
                _ => 1,
            };
            self.last_click = Some(LastClick {
                button,
                at: now,
                pos,
                clicks,
            });
            self.button_clicks[button_index(button)] = clicks;
            self.cursor = pos;
            out.push(Inject::Button {
                button,
                down: true,
                pos,
                clicks,
            });
        } else {
            if !self.buttons.remove(&button) {
                return;
            }
            self.cursor = pos;
            out.push(Inject::Button {
                button,
                down: false,
                pos,
                clicks: self.button_clicks[button_index(button)],
            });
        }
    }

    fn release_all(&mut self, out: &mut Vec<Inject>) {
        self.repeat = None;
        let mut keys: Vec<u16> = self.keys.drain().collect();
        keys.sort_unstable();
        for usage in keys {
            out.push(Inject::Key {
                usage,
                down: false,
                repeat: false,
            });
        }
        for button in [
            Button::Left,
            Button::Right,
            Button::Middle,
            Button::Back,
            Button::Forward,
        ] {
            if self.buttons.remove(&button) {
                out.push(Inject::Button {
                    button,
                    down: false,
                    pos: self.cursor,
                    clicks: self.button_clicks[button_index(button)],
                });
            }
        }
    }
}

fn button_index(button: Button) -> usize {
    match button {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
        Button::Back => 3,
        Button::Forward => 4,
    }
}

fn distance(a: Point, b: Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u16 = 0x04;
    const SHIFT: u16 = keymap::usage::LEFT_SHIFT;

    fn entered(now: Instant) -> Receiver {
        let mut r = Receiver::new(ReceiverConfig::default());
        let mut out = vec![];
        r.control(
            now,
            Control::Enter {
                seq: 10,
                pos: Point::new(5.0, 0.5),
            },
            &mut out,
        );
        assert_eq!(
            out,
            [Inject::MoveTo {
                pos: Point::new(5.0, 0.5)
            }]
        );
        r
    }

    fn motion(seq: u32, x: f64) -> Datagram {
        Datagram::Motion {
            seq,
            pos: Point::new(x, 1.0),
        }
    }

    #[test]
    fn stale_and_reordered_motion_is_dropped() {
        let t = Instant::now();
        let mut r = entered(t);
        let mut out = vec![];
        r.datagram(motion(9, 1.0), &mut out); // from a previous visit
        r.datagram(motion(12, 2.0), &mut out);
        r.datagram(motion(11, 3.0), &mut out); // arrived late
        r.datagram(motion(13, 4.0), &mut out);
        assert_eq!(
            out,
            [
                Inject::MoveTo {
                    pos: Point::new(2.0, 1.0)
                },
                Inject::MoveTo {
                    pos: Point::new(4.0, 1.0)
                },
            ]
        );
    }

    #[test]
    fn motion_sequence_wraps_around() {
        let t = Instant::now();
        let mut r = Receiver::new(ReceiverConfig::default());
        let mut out = vec![];
        r.control(
            t,
            Control::Enter {
                seq: u32::MAX,
                pos: Point::default(),
            },
            &mut out,
        );
        out.clear();
        r.datagram(motion(0, 7.0), &mut out);
        assert_eq!(
            out,
            [Inject::MoveTo {
                pos: Point::new(7.0, 1.0)
            }]
        );
    }

    #[test]
    fn nothing_is_injected_before_enter() {
        let t = Instant::now();
        let mut r = Receiver::new(ReceiverConfig::default());
        let mut out = vec![];
        r.datagram(motion(1, 1.0), &mut out);
        r.control(
            t,
            Control::Key {
                usage: A,
                down: true,
            },
            &mut out,
        );
        r.control(
            t,
            Control::Button {
                button: Button::Left,
                down: true,
                pos: Point::default(),
            },
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn held_key_repeats_until_released() {
        let t = Instant::now();
        let mut r = entered(t);
        let mut out = vec![];
        r.control(
            t,
            Control::Key {
                usage: A,
                down: true,
            },
            &mut out,
        );
        assert_eq!(r.next_deadline(), Some(t + Duration::from_millis(500)));
        r.tick(t + Duration::from_millis(499), &mut out);
        assert_eq!(out.len(), 1, "no repeat before the delay");
        r.tick(t + Duration::from_millis(500), &mut out);
        r.tick(t + Duration::from_millis(533), &mut out);
        assert_eq!(
            out[1..],
            [
                Inject::Key {
                    usage: A,
                    down: true,
                    repeat: true
                },
                Inject::Key {
                    usage: A,
                    down: true,
                    repeat: true
                },
            ]
        );
        out.clear();
        r.control(
            t + Duration::from_millis(540),
            Control::Key {
                usage: A,
                down: false,
            },
            &mut out,
        );
        assert_eq!(
            out,
            [Inject::Key {
                usage: A,
                down: false,
                repeat: false
            }]
        );
        assert_eq!(r.next_deadline(), None);
    }

    #[test]
    fn modifiers_do_not_repeat() {
        let t = Instant::now();
        let mut r = entered(t);
        let mut out = vec![];
        r.control(
            t,
            Control::Key {
                usage: SHIFT,
                down: true,
            },
            &mut out,
        );
        assert_eq!(r.next_deadline(), None);
    }

    #[test]
    fn leave_releases_everything_that_was_pressed() {
        let t = Instant::now();
        let mut r = entered(t);
        let mut out = vec![];
        r.control(
            t,
            Control::Key {
                usage: SHIFT,
                down: true,
            },
            &mut out,
        );
        r.control(
            t,
            Control::Key {
                usage: A,
                down: true,
            },
            &mut out,
        );
        r.control(
            t,
            Control::Button {
                button: Button::Left,
                down: true,
                pos: Point::new(3.0, 3.0),
            },
            &mut out,
        );
        out.clear();
        r.control(t, Control::Leave, &mut out);
        assert_eq!(
            out,
            [
                Inject::Key {
                    usage: A,
                    down: false,
                    repeat: false
                },
                Inject::Key {
                    usage: SHIFT,
                    down: false,
                    repeat: false
                },
                Inject::Button {
                    button: Button::Left,
                    down: false,
                    pos: Point::new(3.0, 3.0),
                    clicks: 1
                },
            ]
        );
        assert!(!r.is_controlled());
        // A late release for a key already released is ignored.
        out.clear();
        r.control(
            t,
            Control::Key {
                usage: A,
                down: false,
            },
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn double_and_triple_clicks_are_counted() {
        let t = Instant::now();
        let mut r = entered(t);
        let mut out = vec![];
        let p = Point::new(100.0, 100.0);
        for i in 0..3 {
            let at = t + Duration::from_millis(150 * i);
            r.control(
                at,
                Control::Button {
                    button: Button::Left,
                    down: true,
                    pos: p,
                },
                &mut out,
            );
            r.control(
                at,
                Control::Button {
                    button: Button::Left,
                    down: false,
                    pos: p,
                },
                &mut out,
            );
        }
        let clicks: Vec<u8> = out
            .iter()
            .filter_map(|i| match i {
                Inject::Button { clicks, .. } => Some(*clicks),
                _ => None,
            })
            .collect();
        assert_eq!(clicks, [1, 1, 2, 2, 3, 3]);

        // Too slow, or moved too far: counting restarts.
        out.clear();
        let later = t + Duration::from_secs(5);
        r.control(
            later,
            Control::Button {
                button: Button::Left,
                down: true,
                pos: p,
            },
            &mut out,
        );
        r.control(
            later,
            Control::Button {
                button: Button::Left,
                down: false,
                pos: p,
            },
            &mut out,
        );
        let far = Point::new(200.0, 100.0);
        r.control(
            later,
            Control::Button {
                button: Button::Left,
                down: true,
                pos: far,
            },
            &mut out,
        );
        assert!(matches!(out[0], Inject::Button { clicks: 1, .. }));
        assert!(matches!(out[2], Inject::Button { clicks: 1, .. }));
    }

    #[test]
    fn local_activity_yields_once_and_releases() {
        let t = Instant::now();
        let mut r = entered(t);
        let mut out = vec![];
        r.control(
            t,
            Control::Key {
                usage: A,
                down: true,
            },
            &mut out,
        );
        out.clear();
        r.local_activity(&mut out);
        assert_eq!(
            out,
            [
                Inject::Key {
                    usage: A,
                    down: false,
                    repeat: false
                },
                Inject::SendYield,
            ]
        );
        out.clear();
        r.local_activity(&mut out);
        assert!(out.is_empty(), "only yield once");
        r.datagram(motion(99, 1.0), &mut out);
        assert!(out.is_empty(), "motion is ignored after yielding");
    }
}
