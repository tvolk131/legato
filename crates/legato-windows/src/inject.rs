//! Injecting input with `SendInput`, for when another machine drives this one.
//!
//! Every injected event carries [`INJECTED_TAG`] in `dwExtraInfo`, so our own low-level
//! hooks can tell it apart from the user's input. Windows won't deliver injected input to
//! windows of elevated (administrator) apps unless Legato itself runs elevated.

use legato_core::Inject;
use legato_core::keymap;
use legato_proto::{Button, Point, Scroll};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    XBUTTON1, XBUTTON2,
};

/// Marks events we inject ("LEGA").
pub const INJECTED_TAG: usize = 0x4c45_4741;

/// Wheel units per pixel when turning trackpad scrolling into wheel movement.
const WHEEL_PER_PIXEL: f64 = 2.4;

#[derive(Debug, Default)]
pub struct Injector {
    /// Flip wheel direction.
    pub invert_wheel: bool,
}

impl Injector {
    pub fn new() -> Self {
        crate::init_dpi_awareness();
        Self::default()
    }

    pub fn apply(&mut self, action: &Inject) {
        match *action {
            Inject::MoveTo { pos } => send(&[mouse(move_to(pos), 0, MOUSEEVENTF_MOVE)]),
            Inject::Button {
                button, down, pos, ..
            } => {
                // Windows counts double-clicks itself from timing and position.
                let (flags, data) = button_flags(button, down);
                send(&[
                    mouse(move_to(pos), 0, MOUSEEVENTF_MOVE),
                    mouse((0, 0), data, flags),
                ]);
            }
            Inject::Key { usage, down, .. } => {
                let Some(scancode) = keymap::windows_scancode_from_hid(usage) else {
                    tracing::debug!("no Windows key for HID usage {usage:#04x}");
                    return;
                };
                let mut flags = KEYEVENTF_SCANCODE;
                if scancode & 0xff00 == 0xe000 {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if !down {
                    flags |= KEYEVENTF_KEYUP;
                }
                send(&[key(scancode & 0xff, flags)]);
            }
            Inject::Scroll(scroll) => {
                let sign = if self.invert_wheel { -1.0 } else { 1.0 };
                let (x, y) = match scroll {
                    Scroll::Wheel { x, y } => (x, y),
                    Scroll::Pixels { x, y } => (x * WHEEL_PER_PIXEL, y * WHEEL_PER_PIXEL),
                };
                let mut events = Vec::new();
                if y != 0.0 {
                    events.push(mouse(
                        (0, 0),
                        (sign * y).round() as i32 as u32,
                        MOUSEEVENTF_WHEEL,
                    ));
                }
                if x != 0.0 {
                    events.push(mouse(
                        (0, 0),
                        (sign * x).round() as i32 as u32,
                        MOUSEEVENTF_HWHEEL,
                    ));
                }
                send(&events);
            }
            Inject::SendYield => {}
        }
    }
}

/// Absolute coordinates over the whole virtual desktop, normalised to 0..=65535.
fn move_to(p: Point) -> (i32, i32) {
    // SAFETY: plain metric queries.
    let (x, y, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN) as f64,
            GetSystemMetrics(SM_YVIRTUALSCREEN) as f64,
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2) as f64,
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2) as f64,
        )
    };
    let nx = ((p.x - x) * 65535.0 / (w - 1.0))
        .round()
        .clamp(0.0, 65535.0) as i32;
    let ny = ((p.y - y) * 65535.0 / (h - 1.0))
        .round()
        .clamp(0.0, 65535.0) as i32;
    (nx, ny)
}

fn button_flags(button: Button, down: bool) -> (MOUSE_EVENT_FLAGS, u32) {
    match (button, down) {
        (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (Button::Back, true) => (MOUSEEVENTF_XDOWN, u32::from(XBUTTON1)),
        (Button::Back, false) => (MOUSEEVENTF_XUP, u32::from(XBUTTON1)),
        (Button::Forward, true) => (MOUSEEVENTF_XDOWN, u32::from(XBUTTON2)),
        (Button::Forward, false) => (MOUSEEVENTF_XUP, u32::from(XBUTTON2)),
    }
}

fn mouse((dx, dy): (i32, i32), data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    let flags = if flags == MOUSEEVENTF_MOVE {
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK
    } else {
        flags
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_TAG,
            },
        },
    }
}

fn key(scancode: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scancode,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED_TAG,
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    if inputs.is_empty() {
        return;
    }
    // SAFETY: the slice holds fully initialised INPUT structs.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        tracing::debug!(
            "SendInput delivered {sent} of {} events (blocked by UIPI?)",
            inputs.len()
        );
    }
}
