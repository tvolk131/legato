use legato_proto::{Display, Point, Rect, Screens};
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayIsBuiltin, CGDisplayMode,
    CGDisplayModelNumber, CGDisplayVendorNumber, CGEvent, CGGetActiveDisplayList, CGMainDisplayID,
};

/// The vendor and product ids of Legato's own virtual display (see `legato-screen`). It's
/// shown in a window on another machine, so it isn't part of the shared desk.
const VIRTUAL_VENDOR: u32 = 0x4c47;
const VIRTUAL_PRODUCT: u32 = 0x0001;

/// Whether `id` is Legato's own virtual display.
pub fn is_legato_virtual_display(id: u32) -> bool {
    CGDisplayVendorNumber(id) == VIRTUAL_VENDOR && CGDisplayModelNumber(id) == VIRTUAL_PRODUCT
}

/// The active displays (except Legato's virtual one), in global display coordinates (points, origin at the top-left of
/// the main display).
pub fn screens() -> Screens {
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    // SAFETY: the buffer holds `ids.len()` entries and both pointers are valid.
    let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if err.0 != 0 {
        tracing::warn!("CGGetActiveDisplayList failed: {}", err.0);
        count = 0;
    }
    let main = CGMainDisplayID();
    let displays = ids[..count as usize]
        .iter()
        .filter(|&&id| !is_legato_virtual_display(id))
        .map(|&id| {
            let b = CGDisplayBounds(id);
            let pixel_scale = CGDisplayCopyDisplayMode(id)
                .map(|mode| {
                    let points = CGDisplayMode::width(Some(&mode)) as f64;
                    let pixels = CGDisplayMode::pixel_width(Some(&mode)) as f64;
                    if points > 0.0 { pixels / points } else { 1.0 }
                })
                .unwrap_or(1.0);
            Display {
                id,
                bounds: Rect::new(b.origin.x, b.origin.y, b.size.width, b.size.height),
                pixel_scale,
                ui_scale: 1.0,
                primary: id == main,
                name: if CGDisplayIsBuiltin(id) {
                    "Built-in display".into()
                } else {
                    format!("Display {id}")
                },
            }
        })
        .collect();
    Screens {
        displays,
        native_per_desk: 1.0,
    }
}

/// Where the cursor is, in global display coordinates.
pub fn cursor_position() -> Point {
    let event = CGEvent::new(None);
    let p = CGEvent::location(event.as_deref());
    Point::new(p.x, p.y)
}
