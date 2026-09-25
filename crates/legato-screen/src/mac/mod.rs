//! The Mac side: a virtual display, captured and encoded.

pub mod capture;
pub mod display;
pub mod encode;
pub mod scale;
pub mod stream;

pub use capture::{Frame, ScreenCapture, screen_recording_allowed};
pub use display::{Mode, VirtualDisplay};
pub use encode::{EncodedFrame, Encoder, EncoderConfig};
pub use stream::{DisplayStream, StreamConfig};
