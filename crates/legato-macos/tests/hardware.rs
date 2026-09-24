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
    for _ in 0..5 {
        post(edge.right() - 1.0, y, 20);
    }
    let actions: Vec<Action> = rx.try_iter().collect();
    let entered = actions.iter().any(|a| {
        matches!(
            a,
            Action::Send {
                msg: Control::Enter { .. },
                ..
            }
        )
    });
    // While captured, motion goes to the peer and the cursor stays put.
    let held = cursor_position();
    post(held.x, held.y, 30);
    post(held.x, held.y, 30);
    let later: Vec<Action> = rx.try_iter().collect();
    let still = cursor_position();

    capture.send(CaptureCommand::Event(Event::PeerYield(peer)));
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
        (still.x - held.x).abs() < 1.0 && (still.y - held.y).abs() < 1.0,
        "cursor moved: {held:?} → {still:?}"
    );
}
