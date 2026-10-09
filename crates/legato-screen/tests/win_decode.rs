//! Decodes a stream the Mac encoder made (see `mac_encoder.rs`), so the two ends are
//! known to agree.
#![cfg(windows)]

use legato_screen::test_pattern::{FRAMES, matches, unpack};
use legato_screen::win::Decoder;

#[test]
fn decodes_what_the_mac_encoder_produces() {
    check_stream(&mut Decoder::new().unwrap());
}

#[test]
fn software_decodes_the_same_stream() {
    let mut decoder = Decoder::new_software().unwrap();
    assert!(!decoder.is_hardware_accelerated());
    check_stream(&mut decoder);
}

#[test]
fn decoder_recovers_in_software_from_a_fresh_keyframe() {
    let mut decoder = Decoder::new().unwrap();
    check_stream(&mut decoder);
    for _ in 0..2 {
        decoder
            .reset_after_error(&anyhow::anyhow!("simulated device loss"))
            .unwrap();
        assert!(!decoder.is_hardware_accelerated());
        check_stream(&mut decoder);
    }
}

#[test]
#[ignore = "requires a real GPU with H.264 decoding"]
fn hardware_decodes_the_mac_stream_without_fallback() {
    let mut decoder = Decoder::new().unwrap();
    assert!(
        decoder.is_hardware_accelerated(),
        "hardware self-check failed"
    );
    check_stream(&mut decoder);
}

fn check_stream(decoder: &mut Decoder) {
    let frames = unpack(include_bytes!("fixtures/pattern.frames"));
    assert_eq!(frames.len(), FRAMES as usize);
    let mut decoded = Vec::new();
    for frame in &frames {
        let pictures = decoder.decode(frame).unwrap();
        // Low-latency mode: nothing is held back.
        assert_eq!(pictures.len(), 1, "one picture per frame");
        decoded.extend(pictures);
    }
    for (n, picture) in decoded.iter().enumerate() {
        matches(picture, n as u32).unwrap();
    }
}
