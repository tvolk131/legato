//! Virtual monitor mode: the Mac gets an extra display whose picture is streamed to a
//! viewer on Windows.
//!
//! On the Mac: a virtual display (a private CoreGraphics API, kept to [`mac::display`]),
//! captured with ScreenCaptureKit and encoded as H.264 with VideoToolbox. On Windows:
//! decoded with Media Foundation into NV12 frames for the viewer to draw.

pub mod adaptive;
pub mod h264;
#[doc(hidden)]
pub mod test_pattern;

#[cfg(target_os = "macos")]
pub mod mac;

#[cfg(windows)]
pub mod win;

/// A decoded picture: 8-bit NV12 (a full-size luma plane, then interleaved half-size
/// chroma), with rows `stride` bytes apart.
#[derive(Clone, PartialEq, Eq)]
pub struct Nv12 {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// `stride * height` luma bytes followed by `stride * height / 2` chroma bytes.
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Nv12 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nv12")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .finish_non_exhaustive()
    }
}

impl Nv12 {
    pub fn y(&self) -> &[u8] {
        &self.data[..(self.stride * self.height) as usize]
    }

    pub fn uv(&self) -> &[u8] {
        &self.data[(self.stride * self.height) as usize..]
    }
}
