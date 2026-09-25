//! Virtual monitor mode: how big and fast the Mac's extra display can be, and where to
//! put it in the Mac's arrangement so it matches the desk.

use legato_proto::{Point, Rect};

use crate::layout::{Layout, MachineId};

/// Frame rates offered for the extra display.
pub const FRAME_RATES: [u32; 5] = [30, 60, 90, 120, 144];

/// The Mac's hardware encoder needs about this long per million pixels when each frame
/// is sent as soon as it's encoded (measured on Apple silicon: 16.2 ms at 3840×2160,
/// 7.7 ms at 2560×1440).
const ENCODE_MS_PER_MEGAPIXEL: f64 = 1.95;

/// The fastest frame rate from [`FRAME_RATES`] the Mac can keep up with at this size.
pub fn max_fps(width: u32, height: u32) -> u32 {
    let megapixels = width as f64 * height as f64 / 1e6;
    let budget = 1000.0 / (megapixels * ENCODE_MS_PER_MEGAPIXEL).max(f64::EPSILON);
    FRAME_RATES
        .iter()
        .copied()
        .filter(|&fps| fps as f64 <= budget)
        .max()
        .unwrap_or(FRAME_RATES[0])
}

/// Whether a display of this size should run in Retina mode (looking like half its
/// pixel size). Smaller ones would look tiny, so they run at 1×.
pub fn prefers_hidpi(width: u32, height: u32) -> bool {
    width >= 2560 && height >= 1440
}

/// `size` scaled down (keeping its shape) to fit within `cap`, if it doesn't already.
pub fn fit_within(size: (u32, u32), cap: (u32, u32)) -> (u32, u32) {
    let (w, h) = (size.0.max(1) as f64, size.1.max(1) as f64);
    let scale = (cap.0 as f64 / w).min(cap.1 as f64 / h).min(1.0);
    (
        ((w * scale).round() as u32) & !1,
        ((h * scale).round() as u32) & !1,
    )
}

/// A size the virtual display and the video pipeline accept: even, and not tiny.
pub fn usable_size(width: u32, height: u32) -> (u32, u32) {
    (width.clamp(640, 7680) & !1, height.clamp(480, 4320) & !1)
}

/// Where to put the extra display in the peer's coordinates so it sits relative to the
/// peer's main display the way `shown` (where it's shown, in this machine's native
/// coordinates) sits relative to that main display on the shared desk. `size` is the extra
/// display's size in the peer's coordinates. `None` if the peer isn't on the desk.
pub fn place_extra_display(
    layout: &Layout,
    peer: MachineId,
    shown: Rect,
    size: (f64, f64),
) -> Option<Point> {
    let local = layout.local();
    let machine = layout.machine(peer)?;
    let main = machine
        .screens
        .displays
        .iter()
        .find(|d| d.primary)
        .or_else(|| machine.screens.displays.first())?
        .bounds;
    let desk = |m: &crate::layout::Machine, r: Rect| {
        let a = m.to_desk(Point::new(r.left(), r.top()));
        let b = m.to_desk(Point::new(r.right(), r.bottom()));
        Rect::new(a.x, a.y, b.x - a.x, b.y - a.y)
    };
    let shown = desk(local, shown);
    let main_desk = desk(machine, main);
    // Which side of the main display, judged by centres relative to the sizes involved.
    let (sc, mc) = (shown.center(), main_desk.center());
    let dx = (sc.x - mc.x) / (shown.width + main_desk.width);
    let dy = (sc.y - mc.y) / (shown.height + main_desk.height);
    // Peer units per desk unit, to carry the offset along the shared edge across.
    let scale = main.width / main_desk.width.max(f64::EPSILON);
    let (w, h) = size;
    // Keep a good stretch of the shared edge in common, so the cursor can cross.
    let overlap = |start: f64, len: f64, edge_start: f64, edge_len: f64| {
        let min_overlap = (len.min(edge_len) / 4.0).max(1.0);
        start.clamp(
            edge_start - len + min_overlap,
            edge_start + edge_len - min_overlap,
        )
    };
    let origin = if dy.abs() >= dx.abs() {
        let x = main.center().x + (sc.x - mc.x) * scale - w / 2.0;
        let x = overlap(x, w, main.left(), main.width);
        let y = if dy < 0.0 {
            main.top() - h
        } else {
            main.bottom()
        };
        Point::new(x, y)
    } else {
        let y = main.center().y + (sc.y - mc.y) * scale - h / 2.0;
        let y = overlap(y, h, main.top(), main.height);
        let x = if dx < 0.0 {
            main.left() - w
        } else {
            main.right()
        };
        Point::new(x, y)
    };
    Some(Point::new(origin.x.round(), origin.y.round()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::fixtures::{macbook_16, windows_triple_4k};
    use crate::layout::{Align, Side};

    const MAC: MachineId = MachineId(1);

    /// Three 4K monitors at 150%, the MacBook centred below the middle one.
    fn desk() -> Layout {
        let mut layout = Layout::new(windows_triple_4k());
        assert!(layout.place_next_to_local(MAC, macbook_16(), 1, Side::Below, Align::Center, 0.0));
        layout
    }

    #[test]
    fn frame_rates_follow_what_the_encoder_can_do() {
        assert_eq!(max_fps(3840, 2160), 60);
        assert_eq!(max_fps(2560, 1440), 120);
        assert_eq!(max_fps(1920, 1080), 144);
        assert_eq!(max_fps(3440, 1440), 90);
        assert_eq!(max_fps(7680, 4320), 30, "never below the slowest option");
    }

    #[test]
    fn streams_fit_within_a_cap_keeping_their_shape() {
        assert_eq!(fit_within((3840, 2160), (2560, 1440)), (2560, 1440));
        assert_eq!(fit_within((3440, 1440), (2560, 1440)), (2560, 1072));
        assert_eq!(
            fit_within((1920, 1080), (2560, 1440)),
            (1920, 1080),
            "never up"
        );
        assert_eq!(max_fps(2560, 1440), 120, "the budget follows the stream");
    }

    #[test]
    fn sizes_are_even_and_within_range() {
        assert_eq!(usable_size(1921, 1081), (1920, 1080));
        assert_eq!(usable_size(100, 100), (640, 480));
        assert!(prefers_hidpi(3840, 2160) && !prefers_hidpi(1920, 1080));
    }

    #[test]
    fn full_screen_on_the_middle_monitor_goes_above_the_macbook() {
        let layout = desk();
        let middle = windows_triple_4k().displays[1].bounds;
        let main = macbook_16().displays[0].bounds;
        let origin = place_extra_display(&layout, MAC, middle, (1920.0, 1080.0)).unwrap();
        // Directly above, centred on the MacBook's display.
        assert_eq!(origin.y, main.top() - 1080.0);
        assert_eq!(origin.x, (main.center().x - 960.0).round());
    }

    #[test]
    fn a_diagonal_monitor_goes_on_the_side_it_is_furthest_towards() {
        let layout = desk();
        // The left monitor is up and to the left of the MacBook, further across than up.
        let left = windows_triple_4k().displays[0].bounds;
        let main = macbook_16().displays[0].bounds;
        let origin = place_extra_display(&layout, MAC, left, (1920.0, 1080.0)).unwrap();
        assert_eq!(origin.x, main.left() - 1920.0, "on the MacBook's left");
        assert!(origin.y < main.top(), "and raised, as the monitor is");
        // Still sharing a good stretch of edge, so the cursor can cross.
        assert!(origin.y + 1080.0 >= main.top() + main.height.min(1080.0) / 4.0 - 1.0);
    }

    #[test]
    fn a_window_beside_the_macbook_puts_it_beside() {
        let layout = desk();
        // A window on the desk to the right of the MacBook, below the right monitor.
        let mac = layout.machine(MAC).unwrap();
        let main = macbook_16().displays[0].bounds;
        let right_of_mac = mac.to_desk(Point::new(main.right(), main.top()));
        let local = layout.local();
        let origin_native = local.to_native(Point::new(right_of_mac.x + 50.0, right_of_mac.y));
        let window = Rect::new(origin_native.x, origin_native.y, 1600.0, 900.0);
        let origin = place_extra_display(&layout, MAC, window, (1600.0, 900.0)).unwrap();
        assert_eq!(origin.x, main.right());
    }
}
