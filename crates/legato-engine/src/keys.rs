//! Key labels and offered targets for key-mapping settings, shared by the Windows and
//! macOS UIs.

pub use legato_core::keymap::{from_name, hid_from_mac_keycode, name, usage};
use legato_proto::Os;

pub const ESCAPE: u16 = 0x29;
/// F13 to F20: keys most keyboards lack, free for apps to use as shortcuts.
const F13_TO_F20: std::ops::RangeInclusive<u16> = 0x68..=0x6f;

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
    let Some(name) = name(key) else {
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
                targets(os).iter().all(|&key| name(key).is_some()),
                "every offered key can be saved"
            );
        }
    }
}
