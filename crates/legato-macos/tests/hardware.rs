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
