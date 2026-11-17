//! Keys for the key mapping settings: what a key press in this window is, the keys a key
//! can act as, and how keys are named on each kind of keyboard.

use iced::keyboard::key::{Code, Physical};
use legato_core::keymap::{self, usage};
use legato_proto::Os;

pub const ESCAPE: u16 = 0x29;
/// F13 to F20: keys most keyboards lack, free for apps to use as shortcuts.
const F13_TO_F20: std::ops::RangeInclusive<u16> = 0x68..=0x6f;

/// The key a physical key press in this window is, if Legato can send it.
pub fn from_physical(key: Physical) -> Option<u16> {
    let Physical::Code(code) = key else {
        return None;
    };
    // iced names keys like the web, except the Windows (Command) keys.
    let name = match code {
        Code::SuperLeft => "MetaLeft".to_owned(),
        Code::SuperRight => "MetaRight".to_owned(),
        code => format!("{code:?}"),
    };
    keymap::from_name(&name)
}

/// The keys offered as what a key acts as on `os`.
pub fn targets(os: Os) -> Vec<u16> {
    let mut keys = Vec::new();
    if os == Os::MacOs {
        keys.push(usage::FN);
    }
    keys.extend([
        usage::LEFT_GUI,
        usage::RIGHT_GUI,
        usage::LEFT_ALT,
        usage::RIGHT_ALT,
        usage::LEFT_CTRL,
        usage::RIGHT_CTRL,
        usage::LEFT_SHIFT,
        usage::RIGHT_SHIFT,
        usage::CAPS_LOCK,
        ESCAPE,
    ]);
    keys.extend(F13_TO_F20);
    keys
}

/// A key's name as `os`'s keyboards print it: "Right Ctrl" on Windows is "Right Control"
/// on a Mac.
pub fn label(key: u16, os: Os) -> String {
    let mac = os == Os::MacOs;
    let modifier = match key {
        usage::LEFT_CTRL | usage::RIGHT_CTRL if mac => Some("Control"),
        usage::LEFT_CTRL | usage::RIGHT_CTRL => Some("Ctrl"),
        usage::LEFT_SHIFT | usage::RIGHT_SHIFT => Some("Shift"),
        usage::LEFT_ALT | usage::RIGHT_ALT if mac => Some("Option"),
        usage::LEFT_ALT | usage::RIGHT_ALT => Some("Alt"),
        usage::LEFT_GUI | usage::RIGHT_GUI if mac => Some("Command"),
        usage::LEFT_GUI | usage::RIGHT_GUI => Some("Windows"),
        _ => None,
    };
    if let Some(modifier) = modifier {
        let side = if key < usage::RIGHT_CTRL {
            "Left"
        } else {
            "Right"
        };
        return format!("{side} {modifier}");
    }
    if key == usage::FN {
        return "fn (Globe)".into();
    }
    if key == ESCAPE {
        return if mac { "Escape" } else { "Esc" }.into();
    }
    let Some(name) = keymap::name(key) else {
        return format!("Key {key:#06x}");
    };
    let name = name
        .strip_prefix("Key")
        .or_else(|| name.strip_prefix("Digit"))
        .unwrap_or(&name);
    // "CapsLock" → "Caps Lock".
    let mut spaced = String::new();
    let mut previous = None;
    for c in name.chars() {
        if c.is_ascii_uppercase() && previous.is_some_and(|p: char| p.is_ascii_lowercase()) {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    spaced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_presses_are_the_keys_legato_sends() {
        for (code, key) in [
            (Code::ControlRight, usage::RIGHT_CTRL),
            (Code::SuperLeft, usage::LEFT_GUI),
            (Code::SuperRight, usage::RIGHT_GUI),
            (Code::AltRight, usage::RIGHT_ALT),
            (Code::CapsLock, usage::CAPS_LOCK),
            (Code::KeyA, 0x04),
            (Code::F13, 0x68),
            (Code::Fn, usage::FN),
        ] {
            assert_eq!(from_physical(Physical::Code(code)), Some(key), "{code:?}");
        }
        assert_eq!(
            from_physical(Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified
            )),
            None
        );
    }

    #[test]
    fn keys_are_named_as_each_keyboard_prints_them() {
        for (key, windows, mac) in [
            (usage::RIGHT_CTRL, "Right Ctrl", "Right Control"),
            (usage::LEFT_GUI, "Left Windows", "Left Command"),
            (usage::RIGHT_ALT, "Right Alt", "Right Option"),
            (usage::LEFT_SHIFT, "Left Shift", "Left Shift"),
            (usage::FN, "fn (Globe)", "fn (Globe)"),
            (usage::CAPS_LOCK, "Caps Lock", "Caps Lock"),
            (ESCAPE, "Esc", "Escape"),
            (0x04, "A", "A"),
            (0x1e, "1", "1"),
            (0x6f, "F20", "F20"),
            (0x50, "Arrow Left", "Arrow Left"),
        ] {
            assert_eq!(label(key, Os::Windows), windows);
            assert_eq!(label(key, Os::MacOs), mac);
        }
    }

    #[test]
    fn only_macs_offer_fn() {
        assert_eq!(targets(Os::MacOs)[0], usage::FN);
        assert!(!targets(Os::Windows).contains(&usage::FN));
        for os in [Os::MacOs, Os::Windows] {
            assert!(
                targets(os).iter().all(|&key| keymap::name(key).is_some()),
                "every offered key can be saved"
            );
        }
    }
}
