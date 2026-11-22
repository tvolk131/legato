//! Keys for the key mapping settings: what a key press in this window is, the keys a key
//! can act as, and how keys are named on each kind of keyboard.

use iced::keyboard::key::{Code, Physical};
use legato_core::keymap;
pub use legato_engine::keys::{ESCAPE, label, targets};

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

#[cfg(test)]
mod tests {
    use super::*;
    use legato_core::keymap::usage;
    use legato_proto::Os;

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
