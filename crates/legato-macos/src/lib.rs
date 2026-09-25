//! macOS backend.
//!
//! Only compiled on macOS; on other platforms this crate is empty.

#![cfg(target_os = "macos")]

mod activity;
mod capture;
mod displays;
mod inject;
mod permissions;

pub use activity::ActivityMonitor;
pub use capture::Capture;
pub use displays::{cursor_position, screens};
pub use inject::Injector;
pub use permissions::{Permissions, request_accessibility, request_missing};

/// Written into the source-user-data field of every event we post, so our own event taps
/// can tell injected input from the user's.
pub const INJECTED_TAG: i64 = 0x4c45_4741_544f; // "LEGATO"

/// The user's key repeat and double-click settings.
pub fn receiver_config() -> legato_core::ReceiverConfig {
    use objc2_app_kit::NSEvent;
    use std::time::Duration;
    let secs = |s: f64, fallback: u64| {
        if s.is_finite() && s > 0.0 {
            Duration::from_secs_f64(s)
        } else {
            Duration::from_millis(fallback)
        }
    };
    legato_core::ReceiverConfig {
        repeat_delay: secs(NSEvent::keyRepeatDelay(), 500),
        repeat_interval: secs(NSEvent::keyRepeatInterval(), 33),
        double_click_interval: secs(NSEvent::doubleClickInterval(), 500),
        ..Default::default()
    }
}

/// Changes whenever anything is copied.
pub fn clipboard_change_count() -> u64 {
    objc2_app_kit::NSPasteboard::generalPasteboard().changeCount() as u64
}
