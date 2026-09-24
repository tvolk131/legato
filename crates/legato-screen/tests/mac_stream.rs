//! Streaming a virtual display. Needs a real Mac session and Screen Recording permission:
//! `cargo test -p legato-screen --test mac_stream -- --ignored`.
//!
//! In its own test binary: a process without an AppKit application can only show one
//! virtual display (see `legato_screen::mac::display`).
#![cfg(target_os = "macos")]

use std::time::Duration;

#[test]
#[ignore = "adds a display to this Mac and needs Screen Recording permission"]
fn a_virtual_display_streams_as_h264() {
    use legato_screen::mac::{DisplayStream, StreamConfig};
    use std::sync::mpsc;

    if !objc2_core_graphics::CGPreflightScreenCaptureAccess() {
        eprintln!("skipped: this process may not record the screen");
        return;
    }
    let (tx, rx) = mpsc::channel();
    let stream = DisplayStream::start(
        "Legato test",
        StreamConfig {
            width: 1920,
            height: 1080,
            hidpi: true,
            fps: 60,
            bitrate: 20_000_000,
        },
        move |frame| {
            let _ = tx.send(frame);
        },
    )
    .unwrap();
    let first = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("a first frame");
    assert!(first.keyframe, "the stream starts with a keyframe");
    // Asking again on a still screen re-sends the last picture as a keyframe.
    while rx.try_recv().is_ok() {}
    stream.request_keyframe();
    let again = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("a keyframe on request");
    assert!(again.keyframe);
    eprintln!("keyframe: {} bytes", again.data.len());
}
