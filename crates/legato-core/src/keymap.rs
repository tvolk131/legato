//! Key codes. On the wire keys are USB HID usages on the keyboard page (0x07), i.e.
//! physical key positions; each machine converts to and from its native codes here.
//!
//! Tables come from Chromium's key-code mapping via the `keycode` crate.

use std::collections::HashMap;
use std::sync::OnceLock;

use keycode::KeyMap;

/// USB HID usage page for keyboards.
const KEYBOARD_PAGE: u16 = 0x07;
/// Chromium's "no native key" markers.
const WIN_NONE: u16 = 0x0000;
const MAC_NONE: u16 = 0xffff;

pub mod usage {
    //! A few HID usages the rest of the code refers to by name.
    pub const CAPS_LOCK: u16 = 0x39;
    pub const LEFT_CTRL: u16 = 0xe0;
    pub const LEFT_SHIFT: u16 = 0xe1;
    pub const LEFT_ALT: u16 = 0xe2;
    pub const LEFT_GUI: u16 = 0xe3;
    pub const RIGHT_CTRL: u16 = 0xe4;
    pub const RIGHT_SHIFT: u16 = 0xe5;
    pub const RIGHT_ALT: u16 = 0xe6;
    pub const RIGHT_GUI: u16 = 0xe7;
}

struct Tables {
    win_to_hid: HashMap<u16, u16>,
    hid_to_win: HashMap<u16, u16>,
    mac_to_hid: HashMap<u16, u16>,
    hid_to_mac: HashMap<u16, u16>,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut t = Tables {
            win_to_hid: HashMap::new(),
            hid_to_win: HashMap::new(),
            mac_to_hid: HashMap::new(),
            hid_to_mac: HashMap::new(),
        };
        for code in 0..=0xff {
            let Ok(k) = KeyMap::from_usb_code(KEYBOARD_PAGE, code) else {
                continue;
            };
            if k.win != WIN_NONE {
                t.win_to_hid.entry(k.win).or_insert(code);
                t.hid_to_win.insert(code, k.win);
            }
            if k.mac != MAC_NONE {
                t.mac_to_hid.entry(k.mac).or_insert(code);
                t.hid_to_mac.insert(code, k.mac);
            }
        }
        t
    })
}

/// Converts a Windows set-1 scancode to a HID usage. Extended keys carry the `0xE0` prefix
/// in the high byte (e.g. Right Ctrl is `0xE01D`).
pub fn hid_from_windows_scancode(scancode: u16) -> Option<u16> {
    tables().win_to_hid.get(&scancode).copied()
}

pub fn windows_scancode_from_hid(usage: u16) -> Option<u16> {
    tables().hid_to_win.get(&usage).copied()
}

/// Converts a macOS virtual key code (`kVK_*`) to a HID usage.
pub fn hid_from_mac_keycode(keycode: u16) -> Option<u16> {
    tables().mac_to_hid.get(&keycode).copied()
}

pub fn mac_keycode_from_hid(usage: u16) -> Option<u16> {
    tables().hid_to_mac.get(&usage).copied()
}

/// Ctrl, Shift, Alt/Option and GUI (Windows/Command) keys.
pub fn is_modifier(usage: u16) -> bool {
    (usage::LEFT_CTRL..=usage::RIGHT_GUI).contains(&usage)
}

/// Keys that the receiving side should auto-repeat while held.
pub fn repeats(usage: u16) -> bool {
    !is_modifier(usage) && usage != usage::CAPS_LOCK
}

/// A per-peer rewrite of HID usages, applied by the controller before sending.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyRemap {
    map: HashMap<u16, u16>,
}

impl std::fmt::Debug for KeyRemap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut pairs: Vec<_> = self.map.iter().collect();
        pairs.sort();
        f.debug_map().entries(pairs).finish()
    }
}

impl Default for KeyRemap {
    fn default() -> Self {
        Self::identity()
    }
}

impl KeyRemap {
    pub fn identity() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Default for a Windows keyboard driving a Mac: swaps Alt and the Windows key so that
    /// the key next to the space bar acts as Command, like on a Mac keyboard.
    pub fn windows_keyboard_on_mac() -> Self {
        let mut remap = Self::identity();
        remap.swap(usage::LEFT_ALT, usage::LEFT_GUI);
        remap.swap(usage::RIGHT_ALT, usage::RIGHT_GUI);
        remap
    }

    pub fn set(&mut self, from: u16, to: u16) {
        if from == to {
            self.map.remove(&from);
        } else {
            self.map.insert(from, to);
        }
    }

    pub fn swap(&mut self, a: u16, b: u16) {
        self.set(a, b);
        self.set(b, a);
    }

    pub fn apply(&self, usage: u16) -> u16 {
        self.map.get(&usage).copied().unwrap_or(usage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u16 = 0x04;
    const RETURN: u16 = 0x28;

    #[test]
    fn common_keys_map_between_windows_and_mac() {
        // (HID usage, Windows scancode, macOS kVK code)
        let cases = [
            (A, 0x001e, 0x00),               // kVK_ANSI_A
            (RETURN, 0x001c, 0x24),          // kVK_Return
            (0x2c, 0x0039, 0x31),            // Space
            (0x4f, 0xe04d, 0x7c),            // Right arrow (extended on Windows)
            (usage::LEFT_GUI, 0xe05b, 0x37), // Windows key / Command
            (usage::LEFT_ALT, 0x0038, 0x3a), // Alt / Option
            (usage::RIGHT_CTRL, 0xe01d, 0x3e),
            (0x4c, 0xe053, 0x75), // Delete (forward)
        ];
        for (hid, win, mac) in cases {
            assert_eq!(hid_from_windows_scancode(win), Some(hid), "win {win:#06x}");
            assert_eq!(windows_scancode_from_hid(hid), Some(win), "hid {hid:#04x}");
            assert_eq!(mac_keycode_from_hid(hid), Some(mac), "hid {hid:#04x}");
            assert_eq!(hid_from_mac_keycode(mac), Some(hid), "mac {mac:#04x}");
        }
    }

    #[test]
    fn keys_missing_on_mac_have_no_mapping() {
        assert_eq!(mac_keycode_from_hid(0x46), None); // PrintScreen
        assert_eq!(hid_from_windows_scancode(0x0000), None);
    }

    /// Every key a Windows keyboard can send, and what it becomes on a Mac. Reviewed as a
    /// snapshot so any change to the tables is visible.
    #[test]
    fn windows_to_mac_table_snapshot() {
        let t = tables();
        let mut rows: Vec<String> = t
            .win_to_hid
            .iter()
            .map(|(&win, &hid)| {
                let name = KeyMap::from_usb_code(KEYBOARD_PAGE, hid)
                    .ok()
                    .and_then(|k| k.code)
                    .map(|c| format!("{c:?}"))
                    .unwrap_or_default();
                let mac = mac_keycode_from_hid(hid)
                    .map(|m| format!("{m:#04x}"))
                    .unwrap_or_else(|| "-".into());
                format!("win {win:#06x}  hid {hid:#04x}  mac {mac:<5} {name}")
            })
            .collect();
        rows.sort();
        insta::assert_snapshot!(rows.join("\n"));
    }

    #[test]
    fn windows_keyboard_on_mac_swaps_alt_and_gui() {
        let remap = KeyRemap::windows_keyboard_on_mac();
        assert_eq!(remap.apply(usage::LEFT_ALT), usage::LEFT_GUI);
        assert_eq!(remap.apply(usage::LEFT_GUI), usage::LEFT_ALT);
        assert_eq!(remap.apply(usage::RIGHT_ALT), usage::RIGHT_GUI);
        assert_eq!(remap.apply(usage::LEFT_CTRL), usage::LEFT_CTRL);
        assert_eq!(remap.apply(A), A);
    }

    #[test]
    fn modifiers_do_not_repeat() {
        assert!(repeats(A));
        assert!(!repeats(usage::LEFT_SHIFT));
        assert!(!repeats(usage::CAPS_LOCK));
    }
}
