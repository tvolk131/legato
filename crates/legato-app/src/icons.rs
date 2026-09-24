//! Navigation icons (24 × 24, drawn for Legato) and the tray icon.

use iced::widget::svg::Handle;

const DEVICES: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M4 4h16a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2h-6v2h3v2H7v-2h3v-2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zm0 2v9h16V6z"/></svg>"#;
const ARRANGEMENT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M2 5h9v6H2zm2 2v2h5V7zm9-2h9v6h-9zm2 2v2h5V7zM7 14h10v6H7zm2 2v2h6v-2z"/></svg>"#;
const SETTINGS: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M3 5h10v2H3zm14 0h4v2h-4zm-4-2h2v6h-2zM3 11h4v2H3zm8 0h10v2H11zM7 9h2v6H7zm-4 8h12v2H3zm16 0h2v2h-2zm-2-2h2v6h-2z"/></svg>"#;

pub fn devices() -> Handle {
    Handle::from_memory(DEVICES.as_bytes())
}

pub fn arrangement() -> Handle {
    Handle::from_memory(ARRANGEMENT.as_bytes())
}

pub fn settings() -> Handle {
    Handle::from_memory(SETTINGS.as_bytes())
}

/// A 32 × 32 RGBA tray icon: two screens, one handing over to the other. Drawn in black
/// with alpha so macOS can use it as a template image.
pub fn tray_rgba() -> (Vec<u8>, u32, u32) {
    const N: u32 = 32;
    let mut px = vec![0u8; (N * N * 4) as usize];
    let mut set = |x: u32, y: u32| {
        if x < N && y < N {
            let i = ((y * N + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
        }
    };
    // Outline of a screen at (x0, y0) of size w × h, 2 px thick.
    let mut screen = |x0: u32, y0: u32, w: u32, h: u32| {
        for x in x0..x0 + w {
            for t in 0..2 {
                set(x, y0 + t);
                set(x, y0 + h - 1 - t);
            }
        }
        for y in y0..y0 + h {
            for t in 0..2 {
                set(x0 + t, y);
                set(x0 + w - 1 - t, y);
            }
        }
    };
    screen(2, 5, 17, 12);
    screen(13, 15, 17, 12);
    (px, N, N)
}
