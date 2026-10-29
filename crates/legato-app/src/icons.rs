//! The native tray uses the same offline Material Symbols catalog as the UI.

use anyhow::{Context, Result, bail};
use iced::advanced::svg::{Data, Handle};
use iced_m3::material_symbols::{IconName, Style, Symbol};

/// Rasterize the official screen-sharing symbol for the native tray API. Black
/// with alpha lets macOS tint it as a template image, including in dark mode.
pub fn tray_rgba() -> Result<(Vec<u8>, u32, u32)> {
    const N: u32 = 32;
    let handle: Handle = Symbol::new(IconName::ScreenShare)
        .style(Style::Rounded)
        .into();
    let Data::Bytes(bytes) = handle.data() else {
        bail!("the bundled Material tray symbol has no SVG bytes");
    };
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
        .context("parse the Material tray symbol")?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(N, N).context("allocate the tray image")?;
    let size = tree.size();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(N as f32 / size.width(), N as f32 / size.height()),
        &mut pixmap.as_mut(),
    );
    let mut rgba = pixmap.take();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        // Preserve antialiasing, independent of the SVG's original foreground.
        // Black also has identical straight and premultiplied RGB values.
        pixel[..3].fill(0);
    }
    Ok((rgba, N, N))
}
