//! The real Caps Lock: the state the HID system keeps, which the keyboard's light shows.

use std::ffi::{c_char, c_void};

use objc2_core_graphics::{CGEventFlags, CGEventSource, CGEventSourceStateID};

/// An IOKit object, connection or Mach port.
type IoObject = u32;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: IoObject, matching: *mut c_void) -> IoObject;
    fn IOServiceOpen(
        service: IoObject,
        owning_task: IoObject,
        connect_type: u32,
        connect: *mut IoObject,
    ) -> i32;
    fn IOServiceClose(connect: IoObject) -> i32;
    fn IOObjectRelease(object: IoObject) -> i32;
    fn IOHIDSetModifierLockState(connect: IoObject, selector: i32, state: bool) -> i32;
}

unsafe extern "C" {
    /// What `mach_task_self()` (a C macro) reads.
    static mach_task_self_: IoObject;
}

/// `kIOHIDParamConnectType`: a connection for setting HID parameters.
const PARAM_CONNECT: u32 = 1;
/// `kIOHIDCapsLockState`.
const CAPS_LOCK_STATE: i32 = 1;

/// Whether Caps Lock is on.
pub fn caps_lock() -> bool {
    CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState)
        .contains(CGEventFlags::MaskAlphaShift)
}

/// Turns Caps Lock on or off, light and all, as the keyboard would.
pub fn set_caps_lock(on: bool) -> Result<(), String> {
    // SAFETY: plain IOKit calls; `matching` is consumed by IOServiceGetMatchingService,
    // and the service and connection are released here.
    unsafe {
        let matching = IOServiceMatching(c"IOHIDSystem".as_ptr());
        let service = IOServiceGetMatchingService(0, matching);
        if service == 0 {
            return Err("no IOHIDSystem service".into());
        }
        let mut connect = 0;
        let opened = IOServiceOpen(service, mach_task_self_, PARAM_CONNECT, &mut connect);
        IOObjectRelease(service);
        if opened != 0 {
            return Err(format!("couldn't open IOHIDSystem ({opened:#x})"));
        }
        let set = IOHIDSetModifierLockState(connect, CAPS_LOCK_STATE, on);
        IOServiceClose(connect);
        if set != 0 {
            return Err(format!("couldn't set Caps Lock ({set:#x})"));
        }
    }
    Ok(())
}
