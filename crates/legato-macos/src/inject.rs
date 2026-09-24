//! Posting synthetic input with Quartz events.

use legato_core::Inject;
use legato_core::keymap::{self, usage};
use legato_proto::{Button, Point, Scroll};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventType, CGMouseButton, CGScrollEventUnit,
};

use crate::INJECTED_TAG;

/// Posts events on behalf of a remote keyboard and mouse.
///
/// macOS doesn't derive modifier state from posted modifier key presses reliably, so the
/// injector tracks held modifiers itself and stamps the flags onto every event.
pub struct Injector {
    source: CFRetained<CGEventSource>,
    cursor: Point,
    buttons: [bool; 5],
    modifiers: Vec<u16>,
    caps_lock: bool,
    /// Sub-line wheel movement carried over between events (x, y).
    wheel_remainder: (f64, f64),
    /// Flip wheel direction (for users who want "natural" scrolling from a Windows mouse).
    pub invert_wheel: bool,
}

impl Injector {
    pub fn new() -> Option<Self> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)?;
        CGEventSource::set_user_data(Some(&source), INJECTED_TAG);
        // Posting shouldn't briefly lock out the user's own trackpad and keyboard, or they
        // couldn't take control back.
        CGEventSource::set_local_events_suppression_interval(Some(&source), 0.0);
        Some(Self {
            source,
            cursor: crate::cursor_position(),
            buttons: [false; 5],
            modifiers: Vec::new(),
            caps_lock: false,
            wheel_remainder: (0.0, 0.0),
            invert_wheel: false,
        })
    }

    pub fn apply(&mut self, action: &Inject) {
        match *action {
            Inject::MoveTo { pos } => self.move_to(pos),
            Inject::Button {
                button,
                down,
                pos,
                clicks,
            } => self.button(button, down, pos, clicks),
            Inject::Key {
                usage,
                down,
                repeat,
            } => self.key(usage, down, repeat),
            Inject::Scroll(scroll) => self.scroll(scroll),
            Inject::SendYield => {}
        }
    }

    fn move_to(&mut self, pos: Point) {
        // With a button held the motion must be a drag, or apps see the button released.
        let (ty, button) = if self.buttons[0] {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if self.buttons[1] {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else if self.buttons[2..].iter().any(|b| *b) {
            (CGEventType::OtherMouseDragged, CGMouseButton::Center)
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        };
        let Some(event) = self.mouse_event(ty, pos, button) else {
            return;
        };
        // Relative motion for apps that read deltas (games, 3D views).
        let dx = (pos.x - self.cursor.x).round() as i64;
        let dy = (pos.y - self.cursor.y).round() as i64;
        CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventDeltaX, dx);
        CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventDeltaY, dy);
        self.post(&event);
        self.cursor = pos;
    }

    fn button(&mut self, button: Button, down: bool, pos: Point, clicks: u8) {
        let (ty, cg_button, number) = match (button, down) {
            (Button::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left, 0),
            (Button::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left, 0),
            (Button::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right, 1),
            (Button::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right, 1),
            (other, true) => (
                CGEventType::OtherMouseDown,
                CGMouseButton::Center,
                other_number(other),
            ),
            (other, false) => (
                CGEventType::OtherMouseUp,
                CGMouseButton::Center,
                other_number(other),
            ),
        };
        let Some(event) = self.mouse_event(ty, pos, cg_button) else {
            return;
        };
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::MouseEventClickState,
            i64::from(clicks),
        );
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::MouseEventButtonNumber,
            number,
        );
        self.post(&event);
        self.buttons[button_index(button)] = down;
        self.cursor = pos;
    }

    fn key(&mut self, hid: u16, down: bool, repeat: bool) {
        let Some(keycode) = keymap::mac_keycode_from_hid(hid) else {
            tracing::debug!("no macOS key for HID usage {hid:#04x}");
            return;
        };
        let Some(event) = CGEvent::new_keyboard_event(Some(&self.source), keycode, down) else {
            return;
        };
        if hid == usage::CAPS_LOCK {
            if !down {
                return;
            }
            self.caps_lock = !self.caps_lock;
            CGEvent::set_type(Some(&event), CGEventType::FlagsChanged);
        } else if keymap::is_modifier(hid) {
            if down {
                if !self.modifiers.contains(&hid) {
                    self.modifiers.push(hid);
                }
            } else {
                self.modifiers.retain(|m| *m != hid);
            }
            CGEvent::set_type(Some(&event), CGEventType::FlagsChanged);
        } else if repeat {
            CGEvent::set_integer_value_field(
                Some(&event),
                CGEventField::KeyboardEventAutorepeat,
                1,
            );
        }
        self.post(&event);
    }

    fn scroll(&mut self, scroll: Scroll) {
        let sign = if self.invert_wheel { -1.0 } else { 1.0 };
        let (units, x, y) = match scroll {
            // 120 = one notch = one line. Carry fractions from high-resolution wheels.
            Scroll::Wheel { x, y } => {
                let fx = self.wheel_remainder.0 + sign * x / 120.0;
                let fy = self.wheel_remainder.1 + sign * y / 120.0;
                let (lx, ly) = (fx.trunc(), fy.trunc());
                self.wheel_remainder = (fx - lx, fy - ly);
                if lx == 0.0 && ly == 0.0 {
                    return;
                }
                (CGScrollEventUnit::Line, lx as i32, ly as i32)
            }
            Scroll::Pixels { x, y } => {
                (CGScrollEventUnit::Pixel, x.round() as i32, y.round() as i32)
            }
        };
        // Quartz: wheel 1 is vertical (positive = up), wheel 2 horizontal (positive = left).
        let Some(event) = CGEvent::new_scroll_wheel_event2(Some(&self.source), units, 2, y, -x, 0)
        else {
            return;
        };
        self.post(&event);
    }

    fn mouse_event(
        &self,
        ty: CGEventType,
        pos: Point,
        button: CGMouseButton,
    ) -> Option<CFRetained<CGEvent>> {
        CGEvent::new_mouse_event(Some(&self.source), ty, CGPoint::new(pos.x, pos.y), button)
    }

    fn flags(&self) -> CGEventFlags {
        let mut flags = CGEventFlags::empty();
        for m in &self.modifiers {
            flags |= match *m {
                usage::LEFT_SHIFT | usage::RIGHT_SHIFT => CGEventFlags::MaskShift,
                usage::LEFT_CTRL | usage::RIGHT_CTRL => CGEventFlags::MaskControl,
                usage::LEFT_ALT | usage::RIGHT_ALT => CGEventFlags::MaskAlternate,
                usage::LEFT_GUI | usage::RIGHT_GUI => CGEventFlags::MaskCommand,
                _ => CGEventFlags::empty(),
            };
        }
        if self.caps_lock {
            flags |= CGEventFlags::MaskAlphaShift;
        }
        flags
    }

    fn post(&self, event: &CGEvent) {
        CGEvent::set_flags(Some(event), self.flags());
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
    }
}

fn button_index(button: Button) -> usize {
    match button {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
        Button::Back => 3,
        Button::Forward => 4,
    }
}

/// Quartz button numbers for "other" buttons.
fn other_number(button: Button) -> i64 {
    match button {
        Button::Middle => 2,
        Button::Back => 3,
        Button::Forward => 4,
        Button::Left => 0,
        Button::Right => 1,
    }
}
