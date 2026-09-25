//! Windows backend.
//!
//! Only compiled on Windows; on other platforms this crate is empty.

#![cfg(windows)]

mod capture;
mod displays;
mod drop;
mod inject;

pub use capture::{Capture, CaptureOptions, Command};

/// The window in front, which gets typing (for the log).
pub fn foreground_app() -> Option<String> {
    // SAFETY: no preconditions.
    let window = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    (!window.is_invalid()).then(|| capture::describe(window))
}
pub use displays::{init_dpi_awareness, screens};
pub use inject::{INJECTED_TAG, Injector};

/// The user's key repeat and double-click settings.
pub fn receiver_config() -> legato_core::ReceiverConfig {
    use std::time::Duration;
    use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETKEYBOARDDELAY, SPI_GETKEYBOARDSPEED, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        SystemParametersInfoW,
    };
    let get = |action| {
        let mut value = 0u32;
        // SAFETY: both actions write a u32 through the pointer.
        unsafe {
            SystemParametersInfoW(
                action,
                0,
                Some((&raw mut value).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .ok()
        .map(|()| value)
    };
    let mut config = legato_core::ReceiverConfig::default();
    // Delay 0..=3 is 250 ms steps; speed 0..=31 is about 2.5..=30 repeats per second.
    if let Some(delay) = get(SPI_GETKEYBOARDDELAY) {
        config.repeat_delay = Duration::from_millis(250 * (u64::from(delay.min(3)) + 1));
    }
    if let Some(speed) = get(SPI_GETKEYBOARDSPEED) {
        let rate = 2.5 + f64::from(speed.min(31)) * 27.5 / 31.0;
        config.repeat_interval = Duration::from_secs_f64(1.0 / rate);
    }
    // SAFETY: no arguments.
    let double = unsafe { GetDoubleClickTime() };
    if double > 0 {
        config.double_click_interval = Duration::from_millis(u64::from(double));
    }
    config
}

/// Changes whenever anything is copied.
pub fn clipboard_change_count() -> u64 {
    // SAFETY: no arguments.
    u64::from(unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() })
}
