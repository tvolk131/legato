//! Privacy permissions: Accessibility (to post events and use an active event tap) and
//! Input Monitoring (to watch input). macOS keeps them separately, and for an app without
//! a stable signature it ties each grant to that exact build, so an update can lose one
//! and not the other.

use objc2_core_graphics::{
    CGPreflightListenEventAccess, CGPreflightPostEventAccess, CGRequestListenEventAccess,
};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const std::ffi::c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryCreate(
        allocator: *const std::ffi::c_void,
        keys: *const *const std::ffi::c_void,
        values: *const *const std::ffi::c_void,
        count: isize,
        key_callbacks: *const std::ffi::c_void,
        value_callbacks: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
    fn CFRelease(cf: *const std::ffi::c_void);
    static kCFBooleanTrue: *const std::ffi::c_void;
    static kCFTypeDictionaryKeyCallBacks: std::ffi::c_void;
    static kCFTypeDictionaryValueCallBacks: std::ffi::c_void;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// Accessibility: required to post events and to use an active event tap.
    pub accessibility: bool,
    /// Posting synthetic events (covered by Accessibility).
    pub post_events: bool,
    /// Listening to all input: Input Monitoring.
    pub listen_events: bool,
    /// Screen Recording: only needed to show this Mac as a display on another machine.
    pub screen_recording: bool,
}

impl Permissions {
    pub fn check() -> Self {
        // SAFETY: no arguments, no preconditions.
        let accessibility = unsafe { AXIsProcessTrusted() };
        Self {
            accessibility,
            post_events: CGPreflightPostEventAccess(),
            listen_events: CGPreflightListenEventAccess(),
            screen_recording: objc2_core_graphics::CGPreflightScreenCaptureAccess(),
        }
    }

    pub fn all_granted(&self) -> bool {
        self.accessibility && self.post_events && self.listen_events
    }

    /// The System Settings names of the permissions sharing still needs.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !(self.accessibility && self.post_events) {
            missing.push("Accessibility");
        }
        if !self.listen_events {
            missing.push("Input Monitoring");
        }
        missing
    }
}

/// Shows the system prompts for whichever of Accessibility and Input Monitoring aren't
/// granted yet. macOS shows nothing for one that was already decided (or is stuck on an
/// earlier build), so the error should also say where to fix it by hand.
pub fn request_missing(permissions: &Permissions) {
    if !(permissions.accessibility && permissions.post_events) {
        request_accessibility();
    }
    if !permissions.listen_events {
        CGRequestListenEventAccess();
    }
}

/// Shows the system prompt asking for Accessibility access (if not already granted) and
/// returns whether it's granted now. The grant belongs to whichever app launched us, e.g.
/// the terminal.
pub fn request_accessibility() -> bool {
    // SAFETY: we build a one-entry CFDictionary from valid CF constants and release it.
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        );
        let trusted = AXIsProcessTrustedWithOptions(options);
        if !options.is_null() {
            CFRelease(options);
        }
        trusted
    }
}
