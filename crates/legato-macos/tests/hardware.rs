//! Tests against the real window server. They move the cursor (and put it back), so they
//! are ignored by default: `cargo test -p legato-macos -- --ignored --test-threads=1`.
//! They need Accessibility access for the app running them.
#![cfg(target_os = "macos")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use legato_core::Inject;
use legato_macos::{ActivityMonitor, Injector, Permissions, cursor_position, screens};
use legato_proto::Point;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
};

fn require_permissions() {
    let p = Permissions::check();
    assert!(p.all_granted(), "grant Accessibility first: {p:?}");
}

#[test]
fn reports_at_least_one_display() {
    let s = screens();
    assert!(!s.displays.is_empty());
    assert_eq!(s.displays.iter().filter(|d| d.primary).count(), 1);
    for d in &s.displays {
        assert!(d.bounds.width > 0.0 && d.bounds.height > 0.0, "{d:?}");
        assert!(d.pixel_scale >= 1.0, "{d:?}");
    }
}

#[test]
#[ignore = "moves the cursor"]
fn injected_motion_lands_exactly_and_is_not_local_activity() {
    require_permissions();
    let original = cursor_position();
    let seen = Arc::new(AtomicUsize::new(0));
    let monitor = {
        let seen = seen.clone();
        ActivityMonitor::start(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap()
    };

    let main = screens()
        .displays
        .into_iter()
        .find(|d| d.primary)
        .unwrap()
        .bounds;
    let target = Point::new(
        main.x + main.width / 2.0 + 37.0,
        main.y + main.height / 2.0 + 11.0,
    );
    let mut injector = Injector::new().unwrap();
    for i in 0..20 {
        injector.apply(&Inject::MoveTo {
            pos: Point::new(target.x + i as f64 * 3.0, target.y),
        });
    }
    std::thread::sleep(Duration::from_millis(100));
    let landed = cursor_position();
    assert!(
        (landed.x - (target.x + 57.0)).abs() < 1.0 && (landed.y - target.y).abs() < 1.0,
        "{landed:?}"
    );
    assert_eq!(
        seen.load(Ordering::SeqCst),
        0,
        "our own events must not count as local input"
    );

    // An untagged event looks like the user's own mouse.
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).unwrap();
    for i in 0..10 {
        let e = CGEvent::new_mouse_event(
            Some(&source),
            CGEventType::MouseMoved,
            CGPoint::new(target.x - i as f64 * 4.0, target.y),
            CGMouseButton::Left,
        )
        .unwrap();
        CGEvent::set_integer_value_field(
            Some(&e),
            objc2_core_graphics::CGEventField::MouseEventDeltaX,
            -4,
        );
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        seen.load(Ordering::SeqCst) >= 1,
        "physical-looking motion was not noticed"
    );

    injector.apply(&Inject::MoveTo { pos: original });
    drop(monitor);
}

#[test]
#[ignore = "moves and briefly hides the cursor"]
fn capture_crosses_an_edge_and_holds_the_cursor() {
    use legato_core::controller::{Action, CaptureCommand, Controller, ControllerConfig, Event};
    use legato_core::{Align, Layout, MachineId, Side};
    use legato_proto::{Control, Display, Rect, Screens};

    require_permissions();
    let original = cursor_position();
    let local = screens();
    let rightmost = (0..local.displays.len())
        .max_by(|&a, &b| {
            local.displays[a]
                .bounds
                .right()
                .total_cmp(&local.displays[b].bounds.right())
        })
        .unwrap();
    let edge = local.displays[rightmost].bounds;
    let peer = MachineId(1);
    let mut layout = Layout::new(local.clone());
    let peer_screens = Screens {
        displays: vec![Display {
            id: 1,
            bounds: Rect::new(0.0, 0.0, 1000.0, 800.0),
            pixel_scale: 1.0,
            ui_scale: 1.0,
            primary: true,
            name: "peer".into(),
        }],
        native_per_desk: 1.0,
    };
    assert!(layout.place_next_to_local(
        peer,
        peer_screens,
        rightmost,
        Side::Right,
        Align::Center,
        0.0
    ));
    let (tx, rx) = std::sync::mpsc::channel();
    let capture = legato_macos::Capture::start(
        Controller::new(
            ControllerConfig {
                push_distance: 0.0,
                ..Default::default()
            },
            layout,
        ),
        move |a| {
            let _ = tx.send(a);
        },
        |_| {},
    )
    .unwrap();

    // Untagged motion looks like the user's own mouse, pushing into the right edge.
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).unwrap();
    let post = |x: f64, y: f64, dx: i64| {
        let e = CGEvent::new_mouse_event(
            Some(&source),
            CGEventType::MouseMoved,
            CGPoint::new(x, y),
            CGMouseButton::Left,
        )
        .unwrap();
        CGEvent::set_integer_value_field(
            Some(&e),
            objc2_core_graphics::CGEventField::MouseEventDeltaX,
            dx,
        );
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
        std::thread::sleep(Duration::from_millis(20));
    };
    let y = edge.y + edge.height / 2.0;
    let mut actions: Vec<Action> = Vec::new();
    for _ in 0..5 {
        post(edge.right() - 1.0, y, 20);
        actions.extend(rx.try_iter());
        if actions.iter().any(|a| {
            matches!(
                a,
                Action::Send {
                    msg: Control::Enter { .. },
                    ..
                }
            )
        }) {
            break;
        }
    }
    let entered = actions.iter().any(|a| {
        matches!(
            a,
            Action::Send {
                msg: Control::Enter { .. },
                ..
            }
        )
    });
    // Taking over warped the hidden cursor to the middle of the main display. The first
    // event after a warp reports the warp's jump in its delta (rdar://11757097): here a big
    // move back towards this Mac, though it moved 2 points. It mustn't hand the pointer
    // back.
    let main = objc2_core_graphics::CGDisplayBounds(objc2_core_graphics::CGMainDisplayID());
    let pin = (
        main.origin.x + main.size.width / 2.0,
        main.origin.y + main.size.height / 2.0,
    );
    post(pin.0 + 2.0, pin.1, -600);
    // After that, each event moves the peer's cursor by its own motion, however far the
    // hidden cursor has drifted from the pin (a trackpad moves it, dropped or not).
    for i in 1..=3 {
        post(pin.0 + 2.0 + 10.0 * f64::from(i), pin.1, 10);
    }
    let later: Vec<Action> = rx.try_iter().collect();
    let entered_at = actions.iter().find_map(|a| match a {
        Action::Send {
            msg: Control::Enter { pos, .. },
            ..
        } => Some(pos.x),
        _ => None,
    });
    let last_x = later.iter().rev().find_map(|a| match a {
        Action::Datagram {
            msg: legato_proto::Datagram::Motion { pos, .. },
            ..
        } => Some(pos.x),
        _ => None,
    });
    // (That the held cursor doesn't move can't be checked here: a posted event moves the
    // cursor to its location even when the tap drops it, unlike the hardware's.)
    let bounced = later.iter().any(|a| {
        matches!(
            a,
            Action::Release { .. }
                | Action::Send {
                    msg: Control::Leave,
                    ..
                }
        )
    });

    // Back on this Mac, with a push needed to cross again: the first event after the warp
    // back (to where it left), spiking towards the peer, mustn't cross straight over.
    capture.send(CaptureCommand::SetConfig(ControllerConfig {
        push_distance: 30.0,
        ..Default::default()
    }));
    capture.send(CaptureCommand::Event(Event::PeerLost(peer)));
    std::thread::sleep(Duration::from_millis(150));
    let _ = rx.try_iter().count();
    post(edge.right() - 1.0, y, 600);
    let recrossed = rx.try_iter().any(|a| {
        matches!(
            a,
            Action::Send {
                msg: Control::Enter { .. },
                ..
            }
        )
    });

    std::thread::sleep(Duration::from_millis(100));
    drop(capture);
    Injector::new()
        .unwrap()
        .apply(&Inject::MoveTo { pos: original });

    assert!(entered, "never entered the peer: {actions:?}");
    assert!(
        later.iter().any(|a| matches!(a, Action::Datagram { .. })),
        "{later:?}"
    );
    assert!(
        !bounced,
        "a warp's delta spike handed the pointer back: {later:?}"
    );
    // 2 points, then 3 × 10: the peer's cursor is 32 in, not a sum of drifts.
    let (entered_at, last_x) = (entered_at.unwrap(), last_x.unwrap());
    assert!(
        (last_x - entered_at - 32.0).abs() < 1.0,
        "moved {} rather than 32: {later:?}",
        last_x - entered_at
    );
    assert!(
        !recrossed,
        "a warp's delta spike crossed straight back over"
    );
}

/// Records this process's own posted key-downs (keycode and flags) as a listen-only tap at
/// the session level sees them, after the window server has had them.
mod key_tap {
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::{Arc, Mutex, mpsc};

    use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, kCFRunLoopCommonModes};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
        CGEventTapProxy, CGEventType,
    };

    pub type Seen = Arc<Mutex<Vec<(i64, u64)>>>;

    pub struct KeyTap {
        pub seen: Seen,
        run_loop: SendRunLoop,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    struct SendRunLoop(CFRetained<CFRunLoop>);
    // SAFETY: CFRunLoopStop may be called from any thread.
    unsafe impl Send for SendRunLoop {}

    impl KeyTap {
        pub fn start() -> Self {
            let seen = Seen::default();
            let (ready_tx, ready_rx) = mpsc::channel();
            let thread = {
                let seen = seen.clone();
                std::thread::spawn(move || run(seen, ready_tx))
            };
            let run_loop = ready_rx.recv().unwrap().expect("couldn't create the tap");
            Self {
                seen,
                run_loop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for KeyTap {
        fn drop(&mut self) {
            self.run_loop.0.stop();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn run(seen: Seen, ready: mpsc::Sender<Option<SendRunLoop>>) {
        let state = Box::into_raw(Box::new(seen));
        // SAFETY: the callback matches the required signature, and `state` outlives the
        // run loop, which is the only caller of the callback.
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                1 << CGEventType::KeyDown.0,
                Some(callback),
                state.cast(),
            )
        };
        let Some(port) = port else {
            // SAFETY: no tap, so nothing else references `state`.
            drop(unsafe { Box::from_raw(state) });
            let _ = ready.send(None);
            return;
        };
        let source = CFMachPort::new_run_loop_source(None, Some(&port), 0).unwrap();
        let run_loop = CFRunLoop::current().unwrap();
        // SAFETY: the mode constant is a valid static CFString.
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        CGEvent::tap_enable(&port, true);
        let _ = ready.send(Some(SendRunLoop(run_loop)));
        CFRunLoop::run();
        CGEvent::tap_enable(&port, false);
        // SAFETY: the run loop has exited, so the callback can't run any more.
        drop(unsafe { Box::from_raw(state) });
    }

    unsafe extern "C-unwind" fn callback(
        _proxy: CGEventTapProxy,
        ty: CGEventType,
        event: NonNull<CGEvent>,
        user_info: *mut c_void,
    ) -> *mut CGEvent {
        // SAFETY: `user_info` is the tap thread's `Seen`, and this runs on that thread.
        let seen = unsafe { &*user_info.cast::<Seen>() };
        // SAFETY: Quartz passes a valid event for the duration of the callback.
        let ev = unsafe { event.as_ref() };
        let tag = CGEvent::integer_value_field(Some(ev), CGEventField::EventSourceUserData);
        if ty == CGEventType::KeyDown && tag == legato_macos::INJECTED_TAG {
            let code = CGEvent::integer_value_field(Some(ev), CGEventField::KeyboardEventKeycode);
            seen.lock()
                .unwrap()
                .push((code, CGEvent::flags(Some(ev)).0));
        }
        event.as_ptr()
    }
}

/// A Mac keyboard marks arrows (and F keys, Home, End…) as function keys, and arrows and
/// the keypad as keypad keys. macOS's Control-← and Control-→ (moving between spaces) are
/// defined with the function-key mark, so typed from a PC without it they didn't switch
/// spaces (0.3.0-alpha.18 and earlier).
#[test]
#[ignore = "types keys, and may switch spaces"]
fn typed_keys_carry_the_marks_a_mac_keyboard_puts_on_them() {
    use legato_core::keymap::usage;
    use objc2_core_graphics::{CGEventFlags, CGEventSourceStateID};

    require_permissions();
    let tap = key_tap::KeyTap::start();
    let mut injector = Injector::new().unwrap();
    let key = |injector: &mut Injector, usage: u16, down: bool| {
        injector.apply(&Inject::Key {
            usage,
            down,
            repeat: false,
        });
        std::thread::sleep(Duration::from_millis(30));
    };
    let press = |injector: &mut Injector, usage: u16| {
        key(injector, usage, true);
        key(injector, usage, false);
    };
    const LEFT: u16 = 0x50;
    const RIGHT: u16 = 0x4f;
    const A: u16 = 0x04;
    const KEYPAD_1: u16 = 0x59;
    const F13: u16 = 0x68;
    for u in [LEFT, A, KEYPAD_1, F13] {
        press(&mut injector, u);
    }
    // Control-←, then Control-→ to come back: macOS's shortcuts for the spaces either side.
    key(&mut injector, usage::LEFT_CTRL, true);
    press(&mut injector, LEFT);
    std::thread::sleep(Duration::from_millis(500));
    press(&mut injector, RIGHT);
    key(&mut injector, usage::LEFT_CTRL, false);
    // For comparison, Control-← the way earlier versions typed it: Control alone.
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).unwrap();
    CGEventSource::set_user_data(Some(&source), legato_macos::INJECTED_TAG);
    for down in [true, false] {
        let e = CGEvent::new_keyboard_event(Some(&source), 0x7b, down).unwrap();
        CGEvent::set_flags(Some(&e), CGEventFlags::MaskControl);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
        std::thread::sleep(Duration::from_millis(30));
    }
    std::thread::sleep(Duration::from_millis(300));
    let seen = tap.seen.lock().unwrap().clone();
    drop(tap);
    for (code, flags) in &seen {
        eprintln!("key {code:#04x}, flags {flags:#08x}");
    }

    let (func, pad, ctrl) = (
        CGEventFlags::MaskSecondaryFn.0,
        CGEventFlags::MaskNumericPad.0,
        CGEventFlags::MaskControl.0,
    );
    let marks = |code: i64, with_ctrl: bool| {
        seen.iter()
            .find(|&&(c, f)| c == code && (f & ctrl != 0) == with_ctrl)
            .map(|&(_, f)| f & (func | pad))
    };
    assert_eq!(marks(0x7b, false), Some(func | pad), "←");
    assert_eq!(marks(0x00, false), Some(0), "A");
    assert_eq!(marks(0x53, false), Some(pad), "keypad 1");
    assert_eq!(marks(0x69, false), Some(func), "F13");
    // Control-← with the function-key mark is macOS's shortcut, which may take it before
    // the session sees it. Only the comparison may lack the mark.
    let control_left: Vec<u64> = seen
        .iter()
        .filter(|&&(c, f)| c == 0x7b && f & ctrl != 0)
        .map(|&(_, f)| f)
        .collect();
    let unmarked = control_left.iter().filter(|&&f| f & func == 0).count();
    eprintln!(
        "Control-←: {} reached the session marked as a function key, {unmarked} unmarked (the \
         comparison)",
        control_left.len() - unmarked
    );
    assert!(
        unmarked <= 1,
        "Control-← was typed without the function-key mark"
    );
}

/// Connected machines share Caps Lock: Legato reads this Mac's about ten times a second
/// and sets it (light and all) to match a peer's.
#[test]
#[ignore = "toggles Caps Lock"]
fn caps_lock_is_read_and_set() {
    use legato_macos::{caps_lock, set_caps_lock};
    let before = caps_lock();
    let wait_for = |want: bool| {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        // Read on another thread, as the engine does.
        while std::time::Instant::now() < deadline {
            if std::thread::spawn(caps_lock).join().unwrap() == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    };
    let set = set_caps_lock(!before);
    let changed = set.is_ok() && wait_for(!before);
    let _ = set_caps_lock(before);
    let restored = wait_for(before);
    eprintln!("Caps Lock was {before}; setting it: {set:?}, seen: {changed}, back: {restored}");
    assert!(set.is_ok(), "{set:?}");
    assert!(changed, "Caps Lock didn't turn {}", !before);
    assert!(restored, "Caps Lock didn't turn back");
}
