//! The virtual display. Needs a real Mac session:
//! `cargo test -p legato-screen --test mac_display -- --ignored --nocapture`.
//!
//! One test making one display: a process without an AppKit application can only show one
//! virtual display (see `legato_screen::mac::display`).
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use legato_screen::mac::{Mode, VirtualDisplay};
use objc2_core_graphics::CGGetActiveDisplayList;

fn active_displays() -> Vec<u32> {
    let mut ids = [0u32; 32];
    let mut count = 0;
    // SAFETY: the buffer holds 32 ids.
    unsafe { CGGetActiveDisplayList(32, ids.as_mut_ptr(), &mut count) };
    ids[..count as usize].to_vec()
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn mode(width: u32, height: u32, refresh: f64) -> Mode {
    Mode {
        width,
        height,
        hidpi: true,
        refresh,
    }
}

#[test]
#[ignore = "adds a display to this Mac for a few seconds"]
fn virtual_display_appears_changes_mode_live_and_goes_away() {
    let display = VirtualDisplay::create("Legato test", mode(2560, 1440, 60.0)).unwrap();
    let id = display.id();
    wait_for("the display to appear", || active_displays().contains(&id));
    let bounds = display.bounds();
    eprintln!(
        "virtual display {id}: {bounds:?} points, {:?} pixels",
        display.pixel_size()
    );
    assert_eq!((bounds.size.width, bounds.size.height), (1280.0, 720.0));
    assert_eq!(display.pixel_size(), (2560, 1440));
    // The ids the macOS backend uses to leave it out of the shared desk.
    assert_eq!(objc2_core_graphics::CGDisplayVendorNumber(id), 0x4c47);
    assert_eq!(objc2_core_graphics::CGDisplayModelNumber(id), 0x0001);
    assert!(
        !legato_macos::screens().displays.iter().any(|d| d.id == id),
        "the virtual display isn't part of the shared desk"
    );

    // Fast refresh rates, and size changes while it's shown (for fitting a window).
    for (w, h, hz) in [(3840, 2160, 144.0), (3840, 2160, 120.0), (3440, 1440, 60.0)] {
        let started = Instant::now();
        display.set_mode(mode(w, h, hz)).unwrap();
        eprintln!(
            "switched to {:?} px at {} Hz in {:?}",
            display.pixel_size(),
            display.refresh_rate(),
            started.elapsed()
        );
        assert_eq!(display.pixel_size(), (w as usize, h as usize));
        assert_eq!(display.refresh_rate(), hz);
    }

    // Moving it in the arrangement: above the main display, then back to the left.
    let main = objc2_core_graphics::CGDisplayBounds(objc2_core_graphics::CGMainDisplayID());
    let size = display.bounds().size;
    for (x, y) in [
        (
            (main.origin.x + (main.size.width - size.width) / 2.0) as i32,
            (main.origin.y - size.height) as i32,
        ),
        ((main.origin.x - size.width) as i32, main.origin.y as i32),
    ] {
        display.set_origin(x, y).unwrap();
        // macOS snaps nearly aligned edges together, so allow a small nudge.
        wait_for("the display to move", || {
            let o = display.bounds().origin;
            (o.x - x as f64).abs() <= 32.0 && (o.y - y as f64).abs() <= 32.0
        });
        eprintln!("moved to {:?}", display.bounds().origin);
    }

    // Removing it works, but after a rearrangement this process's view of the displays
    // is stale (as for any process without an AppKit application; see
    // `legato_screen::mac::display`), so there's nothing reliable left to check here.
    drop(display);
}
