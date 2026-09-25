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
    nv12_buffer(WIDTH as usize, HEIGHT as usize, nv12)
}

/// Adaptive quality's smaller picture: scaled on the GPU, then encoded.
#[test]
fn pictures_scale_down_for_the_moving_track() {
    use legato_screen::mac::scale::Scaler;
    use legato_screen::mac::{Encoder, EncoderConfig};
    use legato_screen::test_pattern::colors;
    use objc2_core_video::*;
    use std::sync::{Arc, Mutex};

    let (w, h) = (1280, 720);
    let [left, right] = colors(3);
    let mut nv12 = vec![0u8; w * h * 3 / 2];
    for y in 0..h {
        for x in 0..w {
            nv12[y * w + x] = if x < w / 2 { left.0 } else { right.0 };
        }
    }
    for i in (w * h..nv12.len()).step_by(2) {
        let x = (i - w * h) % w;
        let (u, v) = if x < w / 2 {
            (left.1, left.2)
        } else {
            (right.1, right.2)
        };
        nv12[i] = u;
        nv12[i + 1] = v;
    }
    let big = nv12_buffer(w, h, &nv12);
    let scaler = Scaler::new(640, 360).unwrap();
    let small = scaler.scale(&big).unwrap();
    assert_eq!(
        (
            CVPixelBufferGetWidth(&small),
            CVPixelBufferGetHeight(&small)
        ),
        (640, 360)
    );
    assert_eq!(
        CVPixelBufferGetPixelFormatType(&small),
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
    );
    // SAFETY: reading the luma plane of a locked 640x360 buffer.
    let (l, r) = unsafe {
        CVPixelBufferLockBaseAddress(&small, CVPixelBufferLockFlags::ReadOnly);
        let base = CVPixelBufferGetBaseAddressOfPlane(&small, 0) as *const u8;
        let stride = CVPixelBufferGetBytesPerRowOfPlane(&small, 0);
        let at = |x: usize, y: usize| *base.add(y * stride + x);
        let sample = (at(160, 180), at(480, 180));
        CVPixelBufferUnlockBaseAddress(&small, CVPixelBufferLockFlags::ReadOnly);
        sample
    };
    assert!(
        l.abs_diff(left.0) <= 2 && r.abs_diff(right.0) <= 2,
        "{l} {r}"
    );

    let frames = Arc::new(Mutex::new(Vec::new()));
    let encoder = {
        let frames = frames.clone();
        Encoder::new(
            EncoderConfig {
                width: 640,
                height: 360,
                fps: 60,
                bitrate: 4_000_000,
            },
            move |f| frames.lock().unwrap().push(f.unwrap()),
        )
        .unwrap()
    };
    encoder.encode_now(&small, Duration::ZERO, true).unwrap();
    let again = scaler.scale(&big).unwrap();
    encoder
        .encode_now(&again, Duration::from_millis(16), false)
        .unwrap();
    let frames = frames.lock().unwrap();
    assert_eq!(frames.len(), 2);
    assert!(frames[0].keyframe && !frames[1].keyframe);
}

fn nv12_buffer(
    w: usize,
    h: usize,
    nv12: &[u8],
) -> objc2_core_foundation::CFRetained<objc2_core_video::CVPixelBuffer> {
    use objc2_core_video::*;
    use std::ptr::NonNull;
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

/// How long scaling a 4K picture down for adaptive quality's moving track takes.
#[test]
#[ignore = "benchmark"]
fn scaling_latency() {
    use legato_screen::mac::scale::Scaler;
    for (w, h) in [(1920u32, 1080u32), (2560, 1440)] {
        let scaler = Scaler::new(w, h).unwrap();
        let buffers: Vec<_> = (0..4).map(|n| sized_buffer(3840, 2160, n)).collect();
        let mut ms: Vec<f64> = (0..60)
            .map(|i| {
                let started = Instant::now();
                let _scaled = scaler.scale(&buffers[i % 4]).unwrap();
                started.elapsed().as_secs_f64() * 1000.0
            })
            .skip(5)
            .collect();
        ms.sort_by(f64::total_cmp);
        eprintln!(
            "3840x2160 -> {w}x{h}: p50 {:.2} ms, p95 {:.2} ms",
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
