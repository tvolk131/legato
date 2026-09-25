//! Streaming a virtual display. Needs a real Mac session and Screen Recording permission:
//! `cargo test -p legato-screen --test mac_stream -- --ignored --nocapture`.
//!
//! In its own test binary: a process without an AppKit application can only show one
//! virtual display (see `legato_screen::mac::display`).
#![cfg(target_os = "macos")]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use legato_screen::adaptive::{MOVING, SHARP};
use legato_screen::mac::{DisplayStream, EncodedFrame, StreamConfig};

/// Frames for `how_long`, or until `until` says stop.
fn collect(
    rx: &mpsc::Receiver<EncodedFrame>,
    how_long: Duration,
    mut until: impl FnMut(&EncodedFrame) -> bool,
) -> Vec<EncodedFrame> {
    let deadline = Instant::now() + how_long;
    let mut frames = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(frame) = rx.recv_timeout(left) else {
            break;
        };
        let done = until(&frame);
        frames.push(frame);
        if done {
            break;
        }
    }
    frames
}

#[test]
#[ignore = "adds a display to this Mac and needs Screen Recording permission"]
fn a_virtual_display_streams_as_h264_on_one_track_or_two() {
    if !objc2_core_graphics::CGPreflightScreenCaptureAccess() {
        eprintln!("skipped: this process may not record the screen");
        return;
    }
    let adaptive = StreamConfig {
        width: 2560,
        height: 1440,
        hidpi: true,
        stream_width: 2560,
        stream_height: 1440,
        moving: Some((1280, 720)),
        fps: 60,
        sharp_fps: 30,
        bitrate: 20_000_000,
    };
    let (tx, rx) = mpsc::channel();
    let stream = DisplayStream::start("Legato test", adaptive, move |frame| {
        let _ = tx.send(frame);
    })
    .unwrap();

    // Whatever's on screen, sent at full size unless it keeps moving.
    let frames = collect(&rx, Duration::from_secs(2), |_| false);
    for f in &frames {
        eprintln!(
            "track {} {} bytes{}",
            f.track,
            f.data.len(),
            if f.keyframe { " keyframe" } else { "" }
        );
    }
    let first_of = |track| frames.iter().find(|f| f.track == track);
    let sharp = first_of(SHARP).expect("a sharp picture");
    assert!(sharp.keyframe, "each track starts with a keyframe");
    if let Some(moving) = first_of(MOVING) {
        assert!(moving.keyframe, "each track starts with a keyframe");
    }
    assert!(
        frames.windows(2).all(|w| w[0].pts < w[1].pts),
        "one clock across tracks"
    );

    // Asking again on a still screen re-sends the last picture as a keyframe.
    while rx.try_recv().is_ok() {}
    stream.request_keyframe(SHARP);
    let again = collect(&rx, Duration::from_secs(5), |f| f.keyframe);
    let again = again.last().expect("a keyframe on request");
    assert!(again.keyframe && again.track == SHARP);
    eprintln!("sharp keyframe: {} bytes", again.data.len());

    // One track only.
    stream
        .reconfigure(StreamConfig {
            moving: None,
            ..adaptive
        })
        .unwrap();
    stream.request_keyframe(SHARP);
    let fixed = collect(&rx, Duration::from_secs(5), |f| f.keyframe);
    assert!(fixed.iter().all(|f| f.track == SHARP));
    assert!(fixed.last().is_some_and(|f| f.keyframe));
}
