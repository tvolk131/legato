//! Decodes a stream the Mac encoder made (see `mac_hardware.rs`), so the two ends are
//! known to agree.
#![cfg(windows)]

use legato_screen::test_pattern::{FRAMES, matches, unpack};
use legato_screen::win::Decoder;

#[test]
fn decodes_what_the_mac_encoder_produces() {
    let frames = unpack(include_bytes!("fixtures/pattern.frames"));
    assert_eq!(frames.len(), FRAMES as usize);
    let mut decoder = Decoder::new().unwrap();
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
