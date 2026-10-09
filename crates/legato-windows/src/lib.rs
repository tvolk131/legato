//! Windows backend.
//!
//! Only compiled on Windows; on other platforms this crate is empty.

#![cfg(windows)]

mod capture;
mod displays;
mod drop;
mod inject;

pub use capture::{Capture, CaptureOptions, Command};

/// Windows' view of the cursor, for the log: where it is, whether it's showing, whether
/// Windows is suppressing it (as it does after touch or pen input), and whether it has an
/// image (none while a window has set none).
pub fn cursor_state() -> String {
    use windows::Win32::UI::WindowsAndMessaging::{
        CURSOR_SHOWING, CURSOR_SUPPRESSED, CURSORINFO, GetCursorInfo,
    };
    let mut info = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: fills in a correctly sized structure.
    if unsafe { GetCursorInfo(&mut info) }.is_err() {
        return "unknown".into();
    }
    format!(
        "at {},{}, showing {}, suppressed {}, has an image {}",
        info.ptScreenPos.x,
        info.ptScreenPos.y,
        info.flags.0 & CURSOR_SHOWING.0 != 0,
        info.flags.0 & CURSOR_SUPPRESSED.0 != 0,
        !info.hCursor.is_invalid()
    )
}

/// A window's client area on the screen (physical pixels), or `None` for a stale handle.
pub fn client_area(window: u64) -> Option<legato_proto::Rect> {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
    let window = HWND(window as usize as *mut core::ffi::c_void);
    // SAFETY: window queries; a stale handle just fails them.
    unsafe {
        let mut client = RECT::default();
        GetClientRect(window, &mut client).ok()?;
        let mut origin = POINT::default();
        if !ClientToScreen(window, &mut origin).as_bool() {
            return None;
        }
        Some(legato_proto::Rect::new(
            origin.x as f64,
            origin.y as f64,
            (client.right - client.left) as f64,
            (client.bottom - client.top) as f64,
        ))
    }
}

/// Whether Caps Lock is on.
pub fn caps_lock() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL};
    // SAFETY: no preconditions.
    let state = unsafe { GetKeyState(i32::from(VK_CAPITAL.0)) };
    state & 1 != 0
}

/// Turns Caps Lock on or off, lights and all, by pressing it if it isn't already.
pub fn set_caps_lock(on: bool) {
    if caps_lock() != on {
        inject::press_caps_lock();
    }
}

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
