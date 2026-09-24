//! An extra display that exists only in software.
//!
//! `CGVirtualDisplay` is private CoreGraphics API (the public route needs an entitlement
//! from Apple). The classes are looked up at run time, so if a macOS release removes them
//! this fails cleanly instead of crashing. The display lasts as long as the object.

use anyhow::{Context, Result, bail, ensure};
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_core_foundation::{CGRect, CGSize};
use objc2_core_graphics::{CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode};
use objc2_foundation::{NSArray, NSObject, NSString};

/// Arbitrary but fixed, so macOS remembers the display's arrangement between runs. The
/// macOS backend leaves displays with these ids out of the shared desk: keep them in sync.
const VENDOR_ID: u32 = 0x4c47; // "LG"
const PRODUCT_ID: u32 = 0x0001;
const SERIAL: u32 = 0x0001;

pub struct VirtualDisplay {
    _display: Retained<AnyObject>,
    _queue: DispatchRetained<DispatchQueue>,
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

impl VirtualDisplay {
    /// Creates a display of `width`×`height` pixels. With `hidpi` it runs at 2× (Retina),
    /// so it looks like half that size in points.
    pub fn create(name: &str, width: u32, height: u32, hidpi: bool) -> Result<Self> {
        ensure!(width >= 640 && height >= 480, "display too small");
        let descriptor_class = class("CGVirtualDisplayDescriptor")?;
        let display_class = class("CGVirtualDisplay")?;
        let settings_class = class("CGVirtualDisplaySettings")?;
        let mode_class = class("CGVirtualDisplayMode")?;
        let queue = DispatchQueue::new("io.legato.virtual-display", None);
        // SAFETY: messages match the private headers (as used by Chromium's tests and
        // several shipping apps); objc2 checks the encodings in debug builds.
        unsafe {
            let descriptor: Retained<AnyObject> = msg_send![descriptor_class, new];
            let queue_ptr: *const AnyObject = (&*queue as *const DispatchQueue).cast();
            let _: () = msg_send![&*descriptor, setQueue: queue_ptr];
            let _: () = msg_send![&*descriptor, setName: &*NSString::from_str(name)];
            let _: () = msg_send![&*descriptor, setMaxPixelsWide: width];
            let _: () = msg_send![&*descriptor, setMaxPixelsHigh: height];
            // About a 27" panel; only used for the physical size macOS reports.
            let scale = if hidpi { 0.5 } else { 1.0 };
            let mm = CGSize::new(width as f64 * 0.155 * scale, height as f64 * 0.155 * scale);
            let _: () = msg_send![&*descriptor, setSizeInMillimeters: mm];
            let _: () = msg_send![&*descriptor, setVendorID: VENDOR_ID];
            let _: () = msg_send![&*descriptor, setProductID: PRODUCT_ID];
            let _: () = msg_send![&*descriptor, setSerialNum: SERIAL];

            let display: Option<Retained<AnyObject>> =
                msg_send![msg_send![display_class, alloc], initWithDescriptor: &*descriptor];
            let display = display.context("macOS refused to create a virtual display")?;

            // In HiDPI mode, mode sizes are in points.
            let (mode_w, mode_h) = if hidpi {
                (width / 2, height / 2)
            } else {
                (width, height)
            };
            let mode: Option<Retained<NSObject>> = msg_send![
                msg_send![mode_class, alloc],
                initWithWidth: mode_w,
                height: mode_h,
                refreshRate: 60.0f64
            ];
            let mode = mode.context("couldn't describe the display mode")?;
            let settings: Retained<AnyObject> = msg_send![settings_class, new];
            let _: () = msg_send![&*settings, setHiDPI: u32::from(hidpi)];
            let modes = NSArray::from_retained_slice(&[mode]);
            let _: () = msg_send![&*settings, setModes: &*modes];
            let applied: Bool = msg_send![&*display, applySettings: &*settings];
            if !applied.as_bool() {
                bail!("macOS refused the virtual display's mode");
            }
            let id: u32 = msg_send![&*display, displayID];
            ensure!(id != 0, "the virtual display has no id");
            Ok(Self {
                _display: display,
                _queue: queue,
                id,
            })
        }
    }

    /// The display's CoreGraphics id.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Where the display sits in the global coordinate space, in points.
    pub fn bounds(&self) -> CGRect {
        CGDisplayBounds(self.id)
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
