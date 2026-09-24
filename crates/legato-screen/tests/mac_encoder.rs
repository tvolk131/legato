//! VideoToolbox encoding. `encoder_produces_a_decodable_stream` runs everywhere, even in
//! CI's macOS VM; `cargo test -p legato-screen --release --test mac_encoder -- --ignored
//! --nocapture` also prints encode latency.
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

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

/// How long VideoToolbox takes per frame, from handing it a picture to getting the
/// encoded frame back, at a few sizes and rates. Set `LEGATO_VT_PIPELINED=1` to compare
/// with letting the encoder keep frames in flight (about 5 frames of extra delay).
#[test]
#[ignore = "benchmark"]
fn encoder_latency() {
    use legato_screen::mac::{Encoder, EncoderConfig};
    use std::sync::{Arc, Mutex};
    for (width, height, fps) in [
        (3840u32, 2160u32, 60u32),
        (3840, 2160, 144),
        (2560, 1440, 144),
    ] {
        let done: Arc<Mutex<Vec<(Duration, Instant)>>> = Arc::default();
        let encoder = {
            let done = done.clone();
            Encoder::new(
                EncoderConfig {
                    width,
                    height,
                    fps,
                    bitrate: 40_000_000,
                },
                move |f| {
                    let f = f.unwrap();
                    done.lock().unwrap().push((f.pts, Instant::now()));
                },
            )
            .unwrap()
        };
        let frames = 90;
        let buffers: Vec<_> = (0..4)
            .map(|n| sized_buffer(width as usize, height as usize, n))
            .collect();
        let mut sent = Vec::new();
        let interval = Duration::from_secs_f64(1.0 / fps as f64);
        let start = Instant::now();
        for i in 0..frames {
            let due = start + interval * i;
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            sent.push(Instant::now());
            let buffer = &buffers[i as usize % 4];
            if std::env::var_os("LEGATO_VT_PIPELINED").is_some() {
                encoder.encode(buffer, interval * i, i == 0).unwrap();
            } else {
                encoder.encode_now(buffer, interval * i, i == 0).unwrap();
            }
        }
        encoder.flush();
        let done = done.lock().unwrap().clone();
        // Match each output to its input by timestamp: the encoder may drop frames.
        let mut ms: Vec<f64> = done
            .iter()
            .filter_map(|(pts, at)| {
                let i = (pts.as_secs_f64() / interval.as_secs_f64()).round() as usize;
                (i >= 5).then(|| at.duration_since(sent[i]).as_secs_f64() * 1000.0)
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        eprintln!(
            "{width}x{height}@{fps}: {} of {frames} frames, encode latency p50 {:.1} ms, p95 {:.1} ms",
            done.len(),
            ms[ms.len() / 2],
            ms[ms.len() * 95 / 100]
        );
    }
}

/// An NV12 picture with a moving gradient, so every frame differs.
fn sized_buffer(
    w: usize,
    h: usize,
    n: usize,
) -> objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer> {
    use objc2_core_video::*;
    use std::ptr::NonNull;
    // SAFETY: creates and fills an NV12 buffer of the right size.
    unsafe {
        let mut out = std::ptr::null_mut();
        // Backed by an IOSurface, like ScreenCaptureKit's buffers.
        let empty = objc2_core_foundation::CFDictionary::<
            objc2_core_foundation::CFString,
            objc2_core_foundation::CFType,
        >::from_slices(&[], &[]);
        let attributes = objc2_core_foundation::CFDictionary::<
            objc2_core_foundation::CFString,
            objc2_core_foundation::CFType,
        >::from_slices(
            &[kCVPixelBufferIOSurfacePropertiesKey],
            &[&**empty.as_opaque()],
        );
        let status = CVPixelBufferCreate(
            None,
            w,
            h,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            Some(attributes.as_opaque()),
            NonNull::from(&mut out),
        );
        assert_eq!(status, 0);
        let buffer = objc2_core_foundation::CFRetained::from_raw(NonNull::new(out).unwrap());
        CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        for (plane, rows) in [(0, h), (1, h / 2)] {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane) as *mut u8;
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            for row in 0..rows {
                let line = std::slice::from_raw_parts_mut(base.add(row * stride), w);
                for (x, px) in line.iter_mut().enumerate() {
                    *px = ((x + row + n * 37) % 220 + 16) as u8;
                }
            }
        }
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        buffer
    }
}
