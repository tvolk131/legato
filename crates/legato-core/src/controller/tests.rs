use std::time::{Duration, Instant};

use legato_proto::{Button, Control, Datagram, Point, Scroll};
use proptest::prelude::*;

use super::*;
use crate::keymap::{KeyRemap, usage};
use crate::layout::fixtures::{macbook_16, windows_triple_4k};
use crate::layout::{Align, Layout, MachineId, Side};

const MAC: MachineId = MachineId(1);
const A: u16 = 0x04;

/// The user's desk: three 4K monitors at 150%, MacBook centered below the middle one.
fn desk() -> Layout {
    let mut layout = Layout::new(windows_triple_4k());
    assert!(layout.place_next_to_local(MAC, macbook_16(), 1, Side::Below, Align::Center, 0.0));
    layout
}

struct Harness {
    c: Controller,
    now: Instant,
    out: Vec<Action>,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(ControllerConfig::default())
    }

    fn with_config(config: ControllerConfig) -> Self {
        Self {
            c: Controller::new(config, desk()),
            now: Instant::now(),
            out: vec![],
        }
    }

    fn step(&mut self, ms: u64, event: Event) -> Verdict {
        self.now += Duration::from_millis(ms);
        self.c.handle(self.now, event, &mut self.out)
    }

    /// Pushes down against the bottom of the middle monitor at native x.
    fn push_down(&mut self, x: f64, dy: f64) -> Verdict {
        self.step(
            8,
            Event::LocalMotion {
                pos: Point::new(x, 2159.0),
                attempted: Point::new(0.0, dy),
            },
        )
    }

    /// Crosses onto the Mac at native Windows x (1.5 native px per desk unit).
    fn cross_to_mac(&mut self, x: f64) {
        while self.push_down(x, 15.0) == Verdict::Pass {
            assert!(self.out.is_empty());
        }
        assert_eq!(self.c.active_peer(), Some(MAC));
    }

    fn take(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.out)
    }
}

fn sent(actions: &[Action]) -> Vec<Control> {
    actions
        .iter()
        .filter_map(|a| match a {
            Action::Send { msg, .. } => Some(msg.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn pushing_through_the_bottom_edge_enters_the_mac() {
    let mut h = Harness::new();
    // Desk x 1280 is the middle of the middle monitor → Mac x 864.
    // 30 desk units of push = 45 native px; 15 px per event → the third event crosses.
    assert_eq!(h.push_down(1920.0, 15.0), Verdict::Pass);
    assert_eq!(h.push_down(1920.0, 15.0), Verdict::Pass);
    assert_eq!(h.push_down(1920.0, 15.0), Verdict::Swallow);
    let out = h.take();
    assert_eq!(
        out,
        [
            Action::Send {
                to: MAC,
                msg: Control::Enter {
                    seq: 1,
                    pos: Point::new(864.0, 0.5),
                },
            },
            Action::Capture,
        ]
    );
}

#[test]
fn moving_along_the_taskbar_never_crosses() {
    let mut h = Harness::new();
    for i in 0..100 {
        // At the bottom edge but moving sideways, with a tiny downward wobble below the
        // threshold each time, separated by long pauses.
        let v = h.step(
            300,
            Event::LocalMotion {
                pos: Point::new(1000.0 + i as f64, 2159.0),
                attempted: Point::new(20.0, 2.0),
            },
        );
        assert_eq!(v, Verdict::Pass);
    }
    assert_eq!(h.c.active_peer(), None);
}

#[test]
fn slow_separated_pushes_reset() {
    let mut h = Harness::new();
    for _ in 0..10 {
        h.step(
            400,
            Event::LocalMotion {
                pos: Point::new(1920.0, 2159.0),
                attempted: Point::new(0.0, 20.0),
            },
        );
    }
    assert_eq!(h.c.active_peer(), None);
}

#[test]
fn bottom_edges_without_a_neighbour_are_walls() {
    let mut h = Harness::with_config(ControllerConfig {
        push_distance: 0.0,
        ..Default::default()
    });
    // Left monitor, and the parts of the middle monitor beyond the Mac's width.
    for x in [-2000.0, 100.0, 3700.0] {
        let v = h.step(
            8,
            Event::LocalMotion {
                pos: Point::new(x, 2159.0),
                attempted: Point::new(0.0, 50.0),
            },
        );
        assert_eq!(v, Verdict::Pass, "x = {x}");
    }
    assert_eq!(h.c.active_peer(), None);
}

/// Backends report positions on their own displays (Windows clamps its hook's). A position
/// on none of them is on a display this machine doesn't share: the Mac's extra display.
#[test]
fn a_mac_cursor_on_its_extra_display_is_not_at_an_edge() {
    // The Mac's view: the PC's monitors above the MacBook. Its extra display (shown on
    // the PC) also sits above the MacBook, but isn't one of the Mac's shared screens.
    const PC: MachineId = MachineId(2);
    let mut layout = Layout::new(macbook_16());
    assert!(layout.place_next_to_local(
        PC,
        windows_triple_4k(),
        0,
        Side::Above,
        Align::Center,
        0.0
    ));
    let mut c = Controller::new(
        ControllerConfig {
            push_distance: 0.0,
            ..Default::default()
        },
        layout,
    );
    let (mut now, mut out) = (Instant::now(), vec![]);
    // A palm on the trackpad while the cursor is on the extra display, well above the
    // MacBook: the Mac keeps its cursor rather than taking over the PC.
    for (y, dy) in [
        (-400.0, -3.0),
        (-400.0, 2.0),
        (-12.0, -4.0),
        (-1080.0, -6.0),
    ] {
        now += Duration::from_millis(8);
        let v = c.handle(
            now,
            Event::LocalMotion {
                pos: Point::new(700.0, y),
                attempted: Point::new(1.0, dy),
            },
            &mut out,
        );
        assert_eq!(v, Verdict::Pass, "at y {y}");
    }
    assert_eq!(c.active_peer(), None);
    assert!(out.is_empty(), "{out:?}");
    // Pushing up from the MacBook's own top edge still crosses.
    now += Duration::from_millis(8);
    let v = c.handle(
        now,
        Event::LocalMotion {
            pos: Point::new(700.0, 0.0),
            attempted: Point::new(0.0, -6.0),
        },
        &mut out,
    );
    assert_eq!(v, Verdict::Swallow);
    assert_eq!(c.active_peer(), Some(PC));
}

#[test]
fn captured_motion_moves_the_mac_cursor_in_its_units() {
    let mut h = Harness::new();
    h.cross_to_mac(1920.0);
    h.take();
    // 30 native Windows px right, 15 down = 20 × 10 desk units = 20 × 10 Mac points.
    h.step(
        8,
        Event::CapturedMotion {
            delta: Point::new(30.0, 15.0),
        },
    );
    assert_eq!(
        h.take(),
        [Action::Datagram {
            to: MAC,
            msg: Datagram::Motion {
                seq: 2,
                pos: Point::new(884.0, 10.5),
            },
        }]
    );
}

#[test]
fn cursor_is_clamped_to_the_mac_screen() {
    let mut h = Harness::new();
    h.cross_to_mac(1920.0);
    h.step(
        8,
        Event::CapturedMotion {
            delta: Point::new(99999.0, 99999.0),
        },
    );
    let Some(Action::Datagram {
        msg: Datagram::Motion { pos, .. },
        ..
    }) = h.take().pop()
    else {
        panic!("expected motion");
    };
    assert_eq!(pos, Point::new(1727.0, 1116.0));
}

#[test]
fn pushing_up_from_the_mac_returns_to_windows() {
    let mut h = Harness::new();
    h.cross_to_mac(1920.0);
    h.step(
        8,
        Event::CapturedMotion {
            delta: Point::new(0.0, 300.0),
        },
    ); // down into the Mac
    h.take();
    // Up to the top edge, then keep pushing until the resistance is overcome.
    let mut actions = vec![];
    for _ in 0..10 {
        h.step(
            8,
            Event::CapturedMotion {
                delta: Point::new(0.0, -150.0),
            },
        );
        actions.extend(h.take());
        if h.c.active_peer().is_none() {
            break;
        }
    }
    assert_eq!(h.c.active_peer(), None);
    let release = actions.iter().rev().find_map(|a| match a {
        Action::Release { warp } => Some(*warp),
        _ => None,
    });
    let warp = release.expect("released capture");
    assert!(
        (warp.x - 1920.0).abs() < 1e-9,
        "same horizontal position: {warp:?}"
    );
    assert!(
        warp.y > 2150.0 && warp.y < 2160.0,
        "just above the edge: {warp:?}"
    );
    assert!(sent(&actions).contains(&Control::Leave));
}

#[test]
fn touching_the_mac_menu_bar_does_not_cross_back() {
    let mut h = Harness::new();
    h.cross_to_mac(1920.0);
    // Reach the top edge and nudge a little: less than the push distance.
    h.step(
        8,
        Event::CapturedMotion {
            delta: Point::new(0.0, -10.0),
        },
    );
    h.step(
        8,
        Event::CapturedMotion {
            delta: Point::new(0.0, -15.0),
        },
    );
    assert_eq!(h.c.active_peer(), Some(MAC));
}

#[test]
fn keys_follow_the_cursor_and_releases_follow_their_press() {
    let mut h = Harness::new();
    // Shift pressed on Windows, then the cursor moves to the Mac while it's held.
    assert_eq!(
        h.step(
            0,
            Event::Key {
                usage: usage::LEFT_SHIFT,
                down: true
            }
        ),
        Verdict::Pass
    );
    h.cross_to_mac(1920.0);
    h.take();
    assert_eq!(
        h.step(
            0,
            Event::Key {
                usage: A,
                down: true
            }
        ),
        Verdict::Swallow
    );
    // Windows' auto-repeat is swallowed and not forwarded; the Mac repeats by itself.
    assert_eq!(
        h.step(
            30,
            Event::Key {
                usage: A,
                down: true
            }
        ),
        Verdict::Swallow
    );
    assert_eq!(
        h.step(
            0,
            Event::Key {
                usage: A,
                down: false
            }
        ),
        Verdict::Swallow
    );
    // Shift was pressed locally, so its release goes to Windows.
    assert_eq!(
        h.step(
            0,
            Event::Key {
                usage: usage::LEFT_SHIFT,
                down: false
            }
        ),
        Verdict::Pass
    );
    assert_eq!(
        sent(&h.take()),
        [
            Control::Key {
                usage: A,
                down: true
            },
            Control::Key {
                usage: A,
                down: false
            },
        ]
    );
}

#[test]
fn remap_applies_to_keys_sent_to_the_mac() {
    let mut h = Harness::new();
    h.c.set_remap(MAC, KeyRemap::windows_keyboard_on_mac());
    h.cross_to_mac(1920.0);
    h.take();
    h.step(
        0,
        Event::Key {
            usage: usage::LEFT_ALT,
            down: true,
        },
    );
    h.step(
        0,
        Event::Key {
            usage: usage::LEFT_ALT,
            down: false,
        },
    );
    assert_eq!(
        sent(&h.take()),
        [
            Control::Key {
                usage: usage::LEFT_GUI,
                down: true
            },
            Control::Key {
                usage: usage::LEFT_GUI,
                down: false
            },
        ]
    );
}

#[test]
fn clicks_carry_the_shared_cursor_position() {
    let mut h = Harness::new();
    h.cross_to_mac(1920.0);
    h.take();
    assert_eq!(
        h.step(
            0,
            Event::Button {
                button: Button::Left,
                down: true
            }
        ),
        Verdict::Swallow
    );
    assert_eq!(
        sent(&h.take()),
        [Control::Button {
            button: Button::Left,
            down: true,
            pos: Point::new(864.0, 0.5),
        }]
    );
}

#[test]
fn no_switching_while_a_button_is_held() {
    let mut h = Harness::with_config(ControllerConfig {
        push_distance: 0.0,
        ..Default::default()
    });
    h.step(
        0,
        Event::Button {
            button: Button::Left,
            down: true,
        },
    );
    assert_eq!(h.push_down(1920.0, 50.0), Verdict::Pass);
    assert_eq!(h.c.active_peer(), None);
    h.step(
        0,
        Event::Button {
            button: Button::Left,
            down: false,
        },
    );
    assert_eq!(h.push_down(1920.0, 50.0), Verdict::Swallow);
}

#[test]
fn carried_files_cross_with_the_button_held_and_drop_on_release() {
    let mut h = Harness::with_config(ControllerConfig {
        push_distance: 0.0,
        ..Default::default()
    });
    let files = vec![std::path::PathBuf::from("C:/Users/me/report.pdf")];
    assert_eq!(h.c.neighbor_at_edge(Point::new(1920.0, 2159.0)), Some(MAC));
    assert_eq!(h.c.neighbor_at_edge(Point::new(100.0, 2159.0)), None);
    // Left button down on a file, drag starts, the backend spots the files.
    assert_eq!(
        h.step(
            0,
            Event::Button {
                button: Button::Left,
                down: true
            }
        ),
        Verdict::Pass
    );
    h.step(0, Event::Carrying(Some(files.clone())));
    assert_eq!(
        h.push_down(1920.0, 10.0),
        Verdict::Swallow,
        "crosses despite the held button"
    );
    h.step(
        8,
        Event::CapturedMotion {
            delta: Point::new(30.0, 300.0),
        },
    );
    h.take();
    // Releasing on the Mac drops the files there; Windows still sees its own release.
    assert_eq!(
        h.step(
            0,
            Event::Button {
                button: Button::Left,
                down: false
            }
        ),
        Verdict::Pass
    );
    assert_eq!(h.take(), [Action::Drop { to: MAC, files }]);
    assert!(!h.c.is_carrying());
}

#[test]
fn releasing_carried_files_back_home_drops_nothing() {
    let mut h = Harness::new();
    h.step(
        0,
        Event::Button {
            button: Button::Left,
            down: true,
        },
    );
    h.step(0, Event::Carrying(Some(vec!["a".into()])));
    h.step(
        0,
        Event::Button {
            button: Button::Left,
            down: false,
        },
    );
    assert!(h.take().is_empty());
    assert!(!h.c.is_carrying());
}

#[test]
fn yield_returns_the_cursor_where_it_left() {
    let mut h = Harness::new();
    h.cross_to_mac(1920.0);
    h.step(
        0,
        Event::Key {
            usage: A,
            down: true,
        },
    );
    h.step(
        8,
        Event::CapturedMotion {
            delta: Point::new(300.0, 300.0),
        },
    );
    h.take();
    h.step(0, Event::PeerYield(MAC));
    let out = h.take();
    assert_eq!(h.c.active_peer(), None);
    assert!(out.contains(&Action::Release {
        warp: Point::new(1920.0, 2159.0)
    }));
    assert!(sent(&out).contains(&Control::Leave));
    // The held key's release is swallowed and goes nowhere: the Mac already let go.
    assert_eq!(
        h.step(
            0,
            Event::Key {
                usage: A,
                down: false
            }
        ),
        Verdict::Swallow
    );
    assert!(h.take().is_empty());
}

#[test]
fn losing_the_peer_releases_capture_without_messages() {
    let mut h = Harness::new();
    h.cross_to_mac(1920.0);
    h.take();
    h.step(0, Event::PeerLost(MAC));
    assert_eq!(
        h.take(),
        [Action::Release {
            warp: Point::new(1920.0, 2159.0)
        }]
    );
}

#[test]
fn scroll_is_forwarded_only_while_remote() {
    let mut h = Harness::new();
    let wheel = Scroll::Wheel { x: 0.0, y: -120.0 };
    assert_eq!(h.step(0, Event::Scroll(wheel)), Verdict::Pass);
    h.cross_to_mac(1920.0);
    h.take();
    assert_eq!(h.step(0, Event::Scroll(wheel)), Verdict::Swallow);
    assert_eq!(sent(&h.take()), [Control::Scroll(wheel)]);
}

#[test]
fn motion_seq_increases_across_visits() {
    let mut h = Harness::new();
    let mut seqs = vec![];
    for _ in 0..2 {
        h.cross_to_mac(1920.0);
        h.step(
            8,
            Event::CapturedMotion {
                delta: Point::new(3.0, 3.0),
            },
        );
        h.step(0, Event::PeerYield(MAC));
        for a in h.take() {
            match a {
                Action::Send {
                    msg: Control::Enter { seq, .. },
                    ..
                } => seqs.push(seq),
                Action::Datagram {
                    msg: Datagram::Motion { seq, .. },
                    ..
                } => seqs.push(seq),
                _ => {}
            }
        }
    }
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "{seqs:?}");
}

#[derive(Debug, Clone)]
enum Op {
    PushDown(f64),
    Move(f64, f64),
    Key(u16, bool),
    Button(bool),
    Yield,
    Lost,
    Wait(u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0.0..60.0).prop_map(Op::PushDown),
        (-400.0..400.0, -400.0..400.0).prop_map(|(x, y)| Op::Move(x, y)),
        (
            prop::sample::select(vec![A, 0x05, usage::LEFT_SHIFT, usage::LEFT_ALT]),
            any::<bool>()
        )
            .prop_map(|(k, d)| Op::Key(k, d)),
        any::<bool>().prop_map(Op::Button),
        Just(Op::Yield),
        Just(Op::Lost),
        (0u64..500).prop_map(Op::Wait),
    ]
}

proptest! {
    /// Whatever happens, every key or button the Mac was told to press is released by the
    /// time control is back on Windows, and Windows never sees a release it didn't see the
    /// press for.
    #[test]
    fn no_stuck_keys_or_buttons(ops in proptest::collection::vec(op(), 0..200)) {
        let mut h = Harness::new();
        let mut local_down: std::collections::HashSet<u16> = Default::default();
        let mut remote_down: std::collections::HashSet<u16> = Default::default();
        let mut remote_button = false;
        // What the user is physically holding; impossible sequences are skipped.
        let mut physical_keys: std::collections::HashSet<u16> = Default::default();
        let mut physical_button = false;
        for op in ops {
            match op {
                Op::Key(k, false) if !physical_keys.contains(&k) => continue,
                Op::Key(k, true) => { physical_keys.insert(k); }
                Op::Key(k, false) => { physical_keys.remove(&k); }
                Op::Button(down) if down == physical_button => continue,
                Op::Button(down) => physical_button = down,
                _ => {}
            }
            let verdict = match op {
                Op::PushDown(dy) => h.push_down(1920.0, dy),
                Op::Move(x, y) if h.c.active_peer().is_some() => {
                    h.step(8, Event::CapturedMotion { delta: Point::new(x, y) })
                }
                Op::Move(..) => Verdict::Pass,
                Op::Key(k, down) => {
                    let v = h.step(1, Event::Key { usage: k, down });
                    if v == Verdict::Pass {
                        if down {
                            local_down.insert(k);
                        } else {
                            prop_assert!(local_down.remove(&k), "local release without press: {k:#x}");
                        }
                    }
                    v
                }
                Op::Button(down) => h.step(1, Event::Button { button: Button::Left, down }),
                Op::Yield => h.step(1, Event::PeerYield(MAC)),
                Op::Lost => h.step(1, Event::PeerLost(MAC)),
                Op::Wait(ms) => {
                    h.now += Duration::from_millis(ms);
                    Verdict::Pass
                }
            };
            let _ = verdict;
            for a in h.take() {
                if let Action::Send { msg, .. } = a {
                    match msg {
                        Control::Key { usage, down: true } => { remote_down.insert(usage); }
                        Control::Key { usage, down: false } => { remote_down.remove(&usage); }
                        Control::Button { down, .. } => remote_button = down,
                        // The Mac releases everything on Leave (and on disconnect).
                        Control::Leave => { remote_down.clear(); remote_button = false; }
                        _ => {}
                    }
                }
            }
            if matches!(op, Op::Lost) {
                remote_down.clear();
                remote_button = false;
            }
            if h.c.active_peer().is_none() {
                prop_assert!(remote_down.is_empty(), "stuck on Mac: {remote_down:?}");
                prop_assert!(!remote_button, "button stuck on Mac");
            }
        }
    }

    /// The shared cursor never leaves the Mac's screen while it's there.
    #[test]
    fn remote_cursor_stays_on_the_mac(moves in proptest::collection::vec((-3000.0..3000.0f64, -3000.0..3000.0f64), 1..100)) {
        let mut h = Harness::new();
        h.cross_to_mac(1920.0);
        for (x, y) in moves {
            h.step(8, Event::CapturedMotion { delta: Point::new(x, y) });
            for a in h.take() {
                if let Action::Datagram { msg: Datagram::Motion { pos, .. }, .. } = a {
                    prop_assert!((0.0..1728.0).contains(&pos.x) && (0.0..1117.0).contains(&pos.y), "{pos:?}");
                }
            }
            if h.c.active_peer().is_none() {
                break;
            }
        }
    }
}

/// The Mac's extra display, 1920×1080 points to the left of its built-in one.
const EXTRA: Rect = Rect::new(-1920.0, 0.0, 1920.0, 1080.0);

/// The pointer over the portal's picture at `at`, the window being on the left monitor.
fn portal_at(at: Point) -> Event {
    Event::PortalMotion {
        at,
        pos: Point::new(-1920.0, 1080.0),
        attempted: Point::new(0.0, 0.0),
    }
}

fn with_portal() -> Harness {
    let mut h = Harness::new();
    let mut out = vec![];
    h.c.set_portal(
        Some(Portal {
            peer: MAC,
            remote: EXTRA,
            window: 42,
        }),
        &mut out,
    );
    assert!(out.is_empty());
    h
}

#[test]
fn the_portal_places_the_mac_cursor_absolutely_and_leaves_the_local_one_alone() {
    let mut h = with_portal();
    let v = h.step(8, portal_at(Point::new(0.5, 0.25)));
    assert_eq!(v, Verdict::Pass, "the local cursor keeps moving");
    let out = h.take();
    assert!(
        matches!(&out[..], [Action::Send { to: MAC, msg: Control::Enter { pos, .. } }]
            if *pos == Point::new(-960.0, 270.0)),
        "{out:?}"
    );
    assert!(!out.contains(&Action::Capture));
    assert_eq!(h.c.active_peer(), Some(MAC));

    h.step(8, portal_at(Point::new(1.0, 1.0)));
    let out = h.take();
    assert!(
        matches!(&out[..], [Action::Datagram { to: MAC, msg: Datagram::Motion { pos, .. } }]
            if *pos == Point::new(-1.0, 1079.0)),
        "clamped inside the display: {out:?}"
    );
}

#[test]
fn input_over_the_portal_goes_to_the_mac_and_leaving_ends_it() {
    let mut h = with_portal();
    h.step(8, portal_at(Point::new(0.1, 0.1)));
    h.take();
    let v = h.step(
        0,
        Event::Button {
            button: Button::Left,
            down: true,
        },
    );
    assert_eq!(v, Verdict::Swallow);
    assert!(matches!(
        &sent(&h.take())[..],
        [Control::Button { button: Button::Left, down: true, pos }] if *pos == Point::new(-1728.0, 108.0)
    ));
    h.step(
        0,
        Event::Button {
            button: Button::Left,
            down: false,
        },
    );
    let v = h.step(
        0,
        Event::Key {
            usage: A,
            down: true,
        },
    );
    assert_eq!(v, Verdict::Swallow);
    h.take();

    // Off the picture (not dragging): the Mac is told, and this machine gets its input
    // back.
    let v = h.step(
        8,
        Event::LocalMotion {
            pos: Point::new(100.0, 100.0),
            attempted: Point::new(-5.0, 0.0),
        },
    );
    assert_eq!(v, Verdict::Pass);
    assert_eq!(sent(&h.take()), [Control::Leave]);
    assert_eq!(h.c.active_peer(), None);
    // What was pressed over the portal was released by the Mac; the releases go nowhere.
    let v = h.step(
        0,
        Event::Key {
            usage: A,
            down: false,
        },
    );
    assert_eq!(v, Verdict::Swallow);
    assert!(h.take().is_empty());
    let v = h.step(
        0,
        Event::Key {
            usage: A,
            down: true,
        },
    );
    assert_eq!(v, Verdict::Pass, "new keys are local");
}

#[test]
fn closing_the_portal_or_losing_the_mac_ends_it() {
    let mut h = with_portal();
    h.step(8, portal_at(Point::new(0.5, 0.5)));
    h.take();
    let mut out = vec![];
    h.c.set_portal(None, &mut out);
    assert_eq!(sent(&out), [Control::Leave]);
    assert_eq!(h.c.active_peer(), None);
    assert_eq!(h.step(8, portal_at(Point::new(0.5, 0.5))), Verdict::Pass);
    assert!(h.take().is_empty(), "no portal, nothing sent");

    let mut h = with_portal();
    h.step(8, portal_at(Point::new(0.5, 0.5)));
    h.take();
    h.step(0, Event::PeerLost(MAC));
    assert!(h.take().is_empty(), "no Release: nothing was captured");
    assert_eq!(h.c.active_peer(), None);
}

#[test]
fn touching_the_mac_hands_it_back_until_the_pointer_moves_over_the_portal_again() {
    let mut h = with_portal();
    h.step(8, portal_at(Point::new(0.5, 0.5)));
    h.take();
    h.step(0, Event::PeerYield(MAC));
    assert_eq!(sent(&h.take()), [Control::Leave]);
    assert_eq!(h.c.active_peer(), None);
    h.step(8, portal_at(Point::new(0.5, 0.6)));
    assert!(matches!(&sent(&h.take())[..], [Control::Enter { .. }]));
}

#[test]
fn the_portal_is_ignored_when_this_machine_may_not_drive_the_mac() {
    let mut h = with_portal();
    let mut out = vec![];
    h.c.set_layout(Layout::new(windows_triple_4k()), &mut out);
    assert_eq!(h.step(8, portal_at(Point::new(0.5, 0.5))), Verdict::Pass);
    assert!(h.take().is_empty());
    assert_eq!(h.c.active_peer(), None);
}

#[test]
fn pictures_are_letterboxed_and_centred() {
    // A 16:9 picture in a square window: bars above and below.
    assert_eq!(
        fit_picture((1920.0, 1080.0), Rect::new(100.0, 0.0, 1600.0, 1600.0)),
        Rect::new(100.0, 350.0, 1600.0, 900.0)
    );
    // In a wide window: bars at the sides.
    assert_eq!(
        fit_picture((1600.0, 900.0), Rect::new(0.0, 0.0, 3200.0, 900.0)),
        Rect::new(800.0, 0.0, 1600.0, 900.0)
    );
}

#[test]
fn dragging_off_the_portal_carries_on_on_the_mac() {
    let mut h = with_portal();
    // Near the right edge of the extra display, which is left of the MacBook's screen.
    h.step(8, portal_at(Point::new(0.99, 0.5)));
    h.step(
        0,
        Event::Button {
            button: Button::Left,
            down: true,
        },
    );
    h.take();
    // The pointer leaves the portal window while the button is held.
    let v = h.step(
        8,
        Event::LocalMotion {
            pos: Point::new(-1000.0, 1080.0),
            attempted: Point::new(20.0, 0.0),
        },
    );
    assert_eq!(v, Verdict::Swallow);
    let out = h.take();
    assert!(out.contains(&Action::Capture), "{out:?}");
    assert!(
        !sent(&out).contains(&Control::Leave),
        "the Mac keeps the drag"
    );
    assert!(
        out.iter()
            .any(|a| matches!(a, Action::Datagram { to: MAC, msg: Datagram::Motion { pos, .. } } if pos.x == 0.0)),
        "the Mac cursor continues at the MacBook's left edge: {out:?}"
    );
    assert_eq!(h.c.active_peer(), Some(MAC));
    // Letting go drops it on the MacBook's screen.
    let v = h.step(
        0,
        Event::Button {
            button: Button::Left,
            down: false,
        },
    );
    assert_eq!(v, Verdict::Swallow);
    assert!(matches!(
        &sent(&h.take())[..],
        [Control::Button {
            button: Button::Left,
            down: false,
            ..
        }]
    ));
}

#[test]
fn moving_onto_the_extra_display_from_the_macbook_enters_the_portal() {
    let mut h = with_portal();
    h.cross_to_mac(1920.0);
    h.step(
        0,
        Event::Button {
            button: Button::Left,
            down: true,
        },
    );
    h.take();
    // Far enough left to leave the MacBook's screen for the extra display beside it.
    h.step(
        8,
        Event::CapturedMotion {
            delta: Point::new(-1500.0, 0.0),
        },
    );
    let out = h.take();
    let at = out.iter().find_map(|a| match a {
        Action::EnterPortal { at } => Some(*at),
        _ => None,
    });
    let at = at.unwrap_or_else(|| panic!("no EnterPortal in {out:?}"));
    assert!((0.9..1.0).contains(&at.x), "near its right edge: {at:?}");
    assert!(!sent(&out).contains(&Control::Leave));
    assert_eq!(h.c.active_peer(), Some(MAC));
    // The drag is still the Mac's.
    assert_eq!(
        h.step(
            0,
            Event::Button {
                button: Button::Left,
                down: false
            }
        ),
        Verdict::Swallow
    );
}

#[test]
fn pushing_past_a_full_screen_portals_edge_towards_the_mac_crosses_to_it() {
    let mut h = with_portal();
    // Full screen on the middle monitor, which the MacBook sits below.
    let at_bottom = |dy| Event::PortalMotion {
        at: Point::new(0.5, 1.0),
        pos: Point::new(1920.0, 2159.0),
        attempted: Point::new(0.0, dy),
    };
    h.step(8, at_bottom(0.0));
    h.take();
    let mut crossed = false;
    for _ in 0..20 {
        if h.step(8, at_bottom(15.0)) == Verdict::Swallow {
            crossed = true;
            break;
        }
    }
    assert!(crossed, "pushing on crosses");
    let out = h.take();
    assert!(out.contains(&Action::Capture));
    assert!(!sent(&out).contains(&Control::Leave), "still the same Mac");
    assert_eq!(h.c.active_peer(), Some(MAC));
}
