//! Needs a real Mac session. Run with `cargo test -p legato-screen -- --ignored`.
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use legato_screen::mac::VirtualDisplay;
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

#[test]
#[ignore = "adds a display to this Mac for a moment"]
fn virtual_display_appears_with_the_requested_size_and_goes_away() {
    let display = VirtualDisplay::create("Legato test", 2560, 1440, true).unwrap();
    let id = display.id();
    wait_for("the display to appear", || active_displays().contains(&id));
    let bounds = display.bounds();
    eprintln!(
        "virtual display {id}: {:?} points, {:?} pixels",
        bounds,
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
    drop(display);
    wait_for("the display to go away", || {
        !active_displays().contains(&id)
    });
}

/// Encodes the test pattern. With `LEGATO_WRITE_FIXTURES=1` it also rewrites the stream
/// the Windows decoder test reads.
#[test]
fn encoder_produces_a_decodable_stream() {
    use legato_screen::h264::{self, nal};
    use legato_screen::mac::{Encoder, EncoderConfig};
    use legato_screen::test_pattern::{FRAMES, HEIGHT, WIDTH, nv12};
    use std::sync::{Arc, Mutex};

    let frames = Arc::new(Mutex::new(Vec::new()));
    let encoder = {
        let frames = frames.clone();
        Encoder::new(
            EncoderConfig {
                width: WIDTH,
                height: HEIGHT,
                fps: 30,
                bitrate: 4_000_000,
            },
            move |f| frames.lock().unwrap().push(f.unwrap()),
        )
        .unwrap()
    };
    for n in 0..FRAMES {
        let buffer = pixel_buffer(&nv12(n));
        encoder
            .encode(&buffer, Duration::from_millis(n as u64 * 33), n == 0)
            .unwrap();
    }
    encoder.flush();
    let frames = frames.lock().unwrap().clone();
    assert_eq!(frames.len(), FRAMES as usize);
    let types = |f: &legato_screen::mac::EncodedFrame| {
        h264::units(&f.data)
            .iter()
            .filter_map(|u| h264::unit_type(u))
            .collect::<Vec<_>>()
    };
    assert!(frames[0].keyframe);
    assert_eq!(&types(&frames[0])[..2], [nal::SPS, nal::PPS]);
    assert!(types(&frames[0]).contains(&nal::IDR));
    assert!(
        frames[1..]
            .iter()
            .all(|f| !f.keyframe && types(f).contains(&nal::SLICE))
    );

    if std::env::var_os("LEGATO_WRITE_FIXTURES").is_some() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pattern.frames");
        std::fs::write(
            path,
            legato_screen::test_pattern::pack(
                &frames.iter().map(|f| f.data.clone()).collect::<Vec<_>>(),
            ),
        )
        .unwrap();
    }
}

fn pixel_buffer(nv12: &[u8]) -> objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer> {
    use legato_screen::test_pattern::{HEIGHT, WIDTH};
    use objc2_core_video::*;
    use std::ptr::NonNull;
    let (w, h) = (WIDTH as usize, HEIGHT as usize);
    // SAFETY: creates and fills an NV12 buffer of the right size.
    unsafe {
        let mut out = std::ptr::null_mut();
        let status = CVPixelBufferCreate(
            None,
            w,
            h,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            None,
            NonNull::from(&mut out),
        );
        assert_eq!(status, 0);
        let buffer = objc2_core_foundation::CFRetained::from_raw(NonNull::new(out).unwrap());
        CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        for (plane, rows, offset) in [(0, h, 0), (1, h / 2, w * h)] {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane) as *mut u8;
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            for row in 0..rows {
                std::ptr::copy_nonoverlapping(
                    nv12[offset + row * w..].as_ptr(),
                    base.add(row * stride),
                    w,
                );
            }
        }
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        buffer
    }
}

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
