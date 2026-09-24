//! Windows backend.
//!
//! Only compiled on Windows; on other platforms this crate is empty.

#![cfg(windows)]

mod capture;
mod displays;

pub use capture::{Capture, CaptureOptions, Command};
pub use displays::{init_dpi_awareness, screens};
