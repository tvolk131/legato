//! An extra display that exists only in software.
//!
//! `CGVirtualDisplay` is private CoreGraphics API (the public route needs an entitlement
//! from Apple). The classes are looked up at run time, so if a macOS release removes them
//! this fails cleanly instead of crashing. The display lasts as long as the object.
//!
//! In a process without an AppKit application (a command-line tool, a test), CoreGraphics
//! stops updating its view of displays created after the first one: it reports no mode or
//! bounds for them, though other processes see them fine. So such a process can show one
//! display in its lifetime; the app, which runs `NSApplication`, can show any number.

use anyhow::{Context, Result, bail, ensure};
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_core_foundation::{CGRect, CGSize};
use std::time::{Duration, Instant};

use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGDisplayModelNumber,
    CGDisplayVendorNumber, CGGetActiveDisplayList,
};
use objc2_foundation::{NSArray, NSObject, NSString};

/// Arbitrary but fixed, so macOS remembers the display's arrangement between runs. The
/// macOS backend leaves displays with these ids out of the shared desk: keep them in sync.
const VENDOR_ID: u32 = 0x4c47; // "LG"
const PRODUCT_ID: u32 = 0x0001;
const SERIAL: u32 = 0x0001;

pub struct VirtualDisplay {
    display: Retained<AnyObject>,
    _queue: DispatchRetained<DispatchQueue>,
    /// The largest mode it can switch to.
    max: (u32, u32),
    id: u32,
}

// SAFETY: CGVirtualDisplay is only kept alive and released; it has no thread affinity.
// `&VirtualDisplay` only offers CoreGraphics queries, which are thread safe.
unsafe impl Send for VirtualDisplay {}
unsafe impl Sync for VirtualDisplay {}

fn class(name: &str) -> Result<&'static AnyClass> {
    let name = std::ffi::CString::new(name)?;
    AnyClass::get(&name).with_context(|| {
        format!(
            "this version of macOS has no {} (virtual displays aren't available)",
            name.to_string_lossy()
        )
    })
}

/// A size and refresh rate for the virtual display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mode {
    /// Size in pixels. With `hidpi` the display runs at 2× (Retina), so it looks like
    /// half this size in points.
    pub width: u32,
    pub height: u32,
    pub hidpi: bool,
    pub refresh: f64,
}

/// The largest mode the display can switch to later; fixed when it's created.
const MAX_PIXELS: (u32, u32) = (7680, 4320);

impl VirtualDisplay {
    pub fn create(name: &str, mode: Mode) -> Result<Self> {
        ensure!(mode.width >= 640 && mode.height >= 480, "display too small");
        // A display that was just removed can linger for a moment, and a new one with the
        // same identity then comes up without a mode.
        wait_until(Duration::from_secs(3), || legato_displays().is_empty());
        let descriptor_class = class("CGVirtualDisplayDescriptor")?;
        let display_class = class("CGVirtualDisplay")?;
        let queue = DispatchQueue::new("io.legato.virtual-display", None);
        let max = (mode.width.max(MAX_PIXELS.0), mode.height.max(MAX_PIXELS.1));
        // SAFETY: messages match the private headers (as used by Chromium's tests and
        // several shipping apps); objc2 checks the encodings in debug builds.
        unsafe {
            let descriptor: Retained<AnyObject> = msg_send![descriptor_class, new];
            let queue_ptr: *const AnyObject = (&*queue as *const DispatchQueue).cast();
            let _: () = msg_send![&*descriptor, setQueue: queue_ptr];
            let _: () = msg_send![&*descriptor, setName: &*NSString::from_str(name)];
            let _: () = msg_send![&*descriptor, setMaxPixelsWide: max.0];
            let _: () = msg_send![&*descriptor, setMaxPixelsHigh: max.1];
            // About a 27" panel; only used for the physical size macOS reports.
            let scale = if mode.hidpi { 0.5 } else { 1.0 };
            let mm = CGSize::new(
                mode.width as f64 * 0.155 * scale,
                mode.height as f64 * 0.155 * scale,
            );
            let _: () = msg_send![&*descriptor, setSizeInMillimeters: mm];
            let _: () = msg_send![&*descriptor, setVendorID: VENDOR_ID];
            let _: () = msg_send![&*descriptor, setProductID: PRODUCT_ID];
            let _: () = msg_send![&*descriptor, setSerialNum: SERIAL];

            let display: Option<Retained<AnyObject>> =
                msg_send![msg_send![display_class, alloc], initWithDescriptor: &*descriptor];
            let display = display.context("macOS refused to create a virtual display")?;
            let id: u32 = msg_send![&*display, displayID];
            ensure!(id != 0, "the virtual display has no id");
            let display = Self {
                display,
                _queue: queue,
                max,
                id,
            };
            display.set_mode(mode)?;
            Ok(display)
        }
    }

    /// Switches to another size or refresh rate. Windows on the display are rearranged
    /// by macOS as for any display change.
    pub fn set_mode(&self, mode: Mode) -> Result<()> {
        ensure!(
            mode.width >= 640
                && mode.height >= 480
                && mode.width <= self.max.0
                && mode.height <= self.max.1,
            "unsupported display size {}x{}",
            mode.width,
            mode.height
        );
        let wanted = (mode.width as usize, mode.height as usize);
        // macOS applies modes asynchronously, and occasionally drops one: ask again once.
        for _ in 0..2 {
            apply(&self.display, mode)?;
            if wait_until(Duration::from_secs(2), || self.pixel_size() == wanted) {
                return Ok(());
            }
        }
        bail!(
            "the display didn't switch to {}x{} (it's {:?})",
            mode.width,
            mode.height,
            self.pixel_size()
        )
    }

    /// The display's CoreGraphics id.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Where the display sits in the global coordinate space, in points.
    pub fn bounds(&self) -> CGRect {
        CGDisplayBounds(self.id)
    }

    /// The current refresh rate, in Hz.
    pub fn refresh_rate(&self) -> f64 {
        CGDisplayMode::refresh_rate(CGDisplayCopyDisplayMode(self.id).as_deref())
    }

    /// The size of the display's backing store, in pixels.
    pub fn pixel_size(&self) -> (usize, usize) {
        let mode = CGDisplayCopyDisplayMode(self.id);
        (
            CGDisplayMode::pixel_width(mode.as_deref()),
            CGDisplayMode::pixel_height(mode.as_deref()),
        )
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        // Releasing `display` removes it.
        tracing::debug!(id = self.id, "removing the virtual display");
    }
}

fn apply(display: &AnyObject, mode: Mode) -> Result<()> {
    let settings_class = class("CGVirtualDisplaySettings")?;
    let mode_class = class("CGVirtualDisplayMode")?;
    // In HiDPI mode, mode sizes are in points.
    let (w, h) = if mode.hidpi {
        (mode.width / 2, mode.height / 2)
    } else {
        (mode.width, mode.height)
    };
    // SAFETY: as in `create`.
    unsafe {
        let descriptor: Option<Retained<NSObject>> = msg_send![
            msg_send![mode_class, alloc],
            initWithWidth: w,
            height: h,
            refreshRate: mode.refresh
        ];
        let descriptor = descriptor.context("couldn't describe the display mode")?;
        let settings: Retained<AnyObject> = msg_send![settings_class, new];
        let _: () = msg_send![&*settings, setHiDPI: u32::from(mode.hidpi)];
        let modes = NSArray::from_retained_slice(&[descriptor]);
        let _: () = msg_send![&*settings, setModes: &*modes];
        let applied: Bool = msg_send![display, applySettings: &*settings];
        if !applied.as_bool() {
            bail!("macOS refused the display mode {mode:?}");
        }
    }
    Ok(())
}

/// Polls `done` until it's true or `timeout` passes; returns whether it became true.
fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Active displays with Legato's identity.
fn legato_displays() -> Vec<u32> {
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    // SAFETY: the buffer holds 32 ids.
    unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    ids[..count as usize]
        .iter()
        .copied()
        .filter(|&id| {
            CGDisplayVendorNumber(id) == VENDOR_ID && CGDisplayModelNumber(id) == PRODUCT_ID
        })
        .collect()
}
