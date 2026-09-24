//! Machines and their displays arranged on a shared "desk".
//!
//! Every machine reports its displays in native global coordinates (points on macOS,
//! physical pixels on Windows). To arrange machines next to each other we convert to
//! *desk units*: `desk = native / native_per_desk + offset`, where `native_per_desk` is a
//! per-machine factor that approximates the OS's logical units, and `offset` places the
//! machine on the desk. The local machine always sits at offset (0, 0).

use legato_proto::{Point, Rect, Screens};
use serde::{Deserialize, Serialize};

/// Index of a machine in a [`Layout`]. The engine maps these to network identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MachineId(pub u32);

impl MachineId {
    /// The machine this code is running on.
    pub const LOCAL: MachineId = MachineId(0);
}

#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    pub id: MachineId,
    pub screens: Screens,
    /// Where this machine's desk-space origin sits on the shared desk.
    pub offset: Point,
}

impl Machine {
    pub fn to_desk(&self, native: Point) -> Point {
        let s = self.screens.native_per_desk;
        Point::new(native.x / s + self.offset.x, native.y / s + self.offset.y)
    }

    pub fn to_native(&self, desk: Point) -> Point {
        let s = self.screens.native_per_desk;
        Point::new((desk.x - self.offset.x) * s, (desk.y - self.offset.y) * s)
    }

    pub fn desk_rect(&self, native: Rect) -> Rect {
        let s = self.screens.native_per_desk;
        Rect::new(
            native.x / s + self.offset.x,
            native.y / s + self.offset.y,
            native.width / s,
            native.height / s,
        )
    }

    /// Display rectangles in desk units.
    pub fn desk_rects(&self) -> impl Iterator<Item = Rect> + '_ {
        self.screens
            .displays
            .iter()
            .map(|d| self.desk_rect(d.bounds))
    }

    /// Bounding box of all displays in desk units, or `None` if the machine has no displays.
    pub fn desk_bounds(&self) -> Option<Rect> {
        bounding_box(self.desk_rects())
    }

    /// The size of one native unit, in desk units.
    pub fn desk_per_native(&self) -> f64 {
        1.0 / self.screens.native_per_desk
    }
}

/// Which side of an anchor display a machine is placed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
    Above,
    Below,
}

/// How a placed machine lines up with its anchor display along the shared edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    /// Left edges (for above/below) or top edges (for left/right) line up.
    Start,
    #[default]
    Center,
    /// Right edges (for above/below) or bottom edges (for left/right) line up.
    End,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    machines: Vec<Machine>,
}

impl Layout {
    /// Creates a layout containing only the local machine.
    pub fn new(local: Screens) -> Self {
        Self {
            machines: vec![Machine {
                id: MachineId::LOCAL,
                screens: local,
                offset: Point::default(),
            }],
        }
    }

    pub fn machines(&self) -> &[Machine] {
        &self.machines
    }

    pub fn machine(&self, id: MachineId) -> Option<&Machine> {
        self.machines.iter().find(|m| m.id == id)
    }

    pub fn local(&self) -> &Machine {
        self.machine(MachineId::LOCAL)
            .expect("the local machine is always present")
    }

    /// Adds or replaces a machine at an explicit desk offset.
    pub fn set_machine(&mut self, id: MachineId, screens: Screens, offset: Point) {
        let machine = Machine {
            id,
            screens,
            offset,
        };
        match self.machines.iter_mut().find(|m| m.id == id) {
            Some(existing) => *existing = machine,
            None => self.machines.push(machine),
        }
    }

    /// Updates a machine's displays, keeping its offset. No-op for unknown machines.
    pub fn update_screens(&mut self, id: MachineId, screens: Screens) {
        if let Some(m) = self.machines.iter_mut().find(|m| m.id == id) {
            m.screens = screens;
        }
    }

    pub fn remove_machine(&mut self, id: MachineId) {
        if id != MachineId::LOCAL {
            self.machines.retain(|m| m.id != id);
        }
    }

    /// Places `id` flush against one side of a local display.
    ///
    /// `display` indexes the local machine's display list. `nudge` shifts the machine along
    /// the shared edge (in desk units) after alignment. Returns `false` if the display
    /// index is out of range or the machine has no displays.
    pub fn place_next_to_local(
        &mut self,
        id: MachineId,
        screens: Screens,
        display: usize,
        side: Side,
        align: Align,
        nudge: f64,
    ) -> bool {
        let local = self.local();
        let Some(anchor) = local
            .screens
            .displays
            .get(display)
            .map(|d| local.desk_rect(d.bounds))
        else {
            return false;
        };
        let unplaced = Machine {
            id,
            screens,
            offset: Point::default(),
        };
        let Some(bounds) = unplaced.desk_bounds() else {
            return false;
        };

        let along = |anchor_start: f64, anchor_len: f64, len: f64| match align {
            Align::Start => anchor_start,
            Align::Center => anchor_start + (anchor_len - len) / 2.0,
            Align::End => anchor_start + anchor_len - len,
        } + nudge;

        let (x, y) = match side {
            Side::Below => (along(anchor.x, anchor.width, bounds.width), anchor.bottom()),
            Side::Above => (
                along(anchor.x, anchor.width, bounds.width),
                anchor.top() - bounds.height,
            ),
            Side::Right => (
                anchor.right(),
                along(anchor.y, anchor.height, bounds.height),
            ),
            Side::Left => (
                anchor.left() - bounds.width,
                along(anchor.y, anchor.height, bounds.height),
            ),
        };
        // Offset such that the machine's bounding box lands at (x, y).
        let offset = Point::new(x - bounds.x, y - bounds.y);
        self.set_machine(id, unplaced.screens, offset);
        true
    }

    /// Places `id` so that the bounding box of its displays has its top-left corner at
    /// `top_left` (desk units). Returns `false` if the machine has no displays.
    pub fn place_at(&mut self, id: MachineId, screens: Screens, top_left: Point) -> bool {
        let unplaced = Machine {
            id,
            screens,
            offset: Point::default(),
        };
        let Some(bounds) = unplaced.desk_bounds() else {
            return false;
        };
        let offset = Point::new(top_left.x - bounds.x, top_left.y - bounds.y);
        self.set_machine(id, unplaced.screens, offset);
        true
    }

    /// The machine and desk rectangle of the display containing `desk`, if any.
    pub fn display_at(&self, desk: Point) -> Option<(MachineId, Rect)> {
        self.machines
            .iter()
            .find_map(|m| m.desk_rects().find(|r| r.contains(desk)).map(|r| (m.id, r)))
    }
}

pub(crate) fn bounding_box(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    let mut iter = rects.into_iter();
    let first = iter.next()?;
    let (mut l, mut t, mut r, mut b) = (first.left(), first.top(), first.right(), first.bottom());
    for rect in iter {
        l = l.min(rect.left());
        t = t.min(rect.top());
        r = r.max(rect.right());
        b = b.max(rect.bottom());
    }
    Some(Rect::new(l, t, r - l, b - t))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use legato_proto::{Display, Rect, Screens};

    fn display(id: u32, bounds: Rect, pixel_scale: f64, ui_scale: f64, primary: bool) -> Display {
        Display {
            id,
            bounds,
            pixel_scale,
            ui_scale,
            primary,
            name: format!("display {id}"),
        }
    }

    /// Three 4K monitors side by side at 150%, primary in the middle.
    pub fn windows_triple_4k() -> Screens {
        Screens {
            displays: vec![
                display(1, Rect::new(-3840.0, 0.0, 3840.0, 2160.0), 1.0, 1.5, false),
                display(2, Rect::new(0.0, 0.0, 3840.0, 2160.0), 1.0, 1.5, true),
                display(3, Rect::new(3840.0, 0.0, 3840.0, 2160.0), 1.0, 1.5, false),
            ],
            native_per_desk: 1.5,
        }
    }

    /// A 16" MacBook Pro at its default "looks like 1728 × 1117" resolution.
    pub fn macbook_16() -> Screens {
        Screens {
            displays: vec![display(
                1,
                Rect::new(0.0, 0.0, 1728.0, 1117.0),
                2.0,
                1.0,
                true,
            )],
            native_per_desk: 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    const MAC: MachineId = MachineId(1);

    #[test]
    fn native_desk_round_trip() {
        let layout = Layout::new(windows_triple_4k());
        let local = layout.local();
        let p = Point::new(-1234.0, 2159.0);
        let back = local.to_native(local.to_desk(p));
        assert!((back.x - p.x).abs() < 1e-9 && (back.y - p.y).abs() < 1e-9);
        assert_eq!(
            local.to_desk(Point::new(3840.0, 2160.0)),
            Point::new(2560.0, 1440.0)
        );
    }

    #[test]
    fn mac_centered_below_middle_monitor() {
        let mut layout = Layout::new(windows_triple_4k());
        assert!(layout.place_next_to_local(MAC, macbook_16(), 1, Side::Below, Align::Center, 0.0));
        let mac = layout.machine(MAC).unwrap();
        // Middle monitor is desk x 0..2560; the 1728-wide Mac is centered under it.
        assert_eq!(
            mac.desk_bounds().unwrap(),
            Rect::new(416.0, 1440.0, 1728.0, 1117.0)
        );
        assert_eq!(layout.display_at(Point::new(500.0, 1440.0)).unwrap().0, MAC);
        assert_eq!(
            layout.display_at(Point::new(500.0, 1439.9)).unwrap().0,
            MachineId::LOCAL
        );
        // Beneath the outer monitors there is nothing.
        assert!(layout.display_at(Point::new(-100.0, 1500.0)).is_none());
    }

    #[test]
    fn place_at_puts_the_bounding_box_there() {
        let mut layout = Layout::new(windows_triple_4k());
        assert!(layout.place_at(MAC, macbook_16(), Point::new(100.0, 1440.0)));
        assert_eq!(
            layout.machine(MAC).unwrap().desk_bounds().unwrap(),
            Rect::new(100.0, 1440.0, 1728.0, 1117.0)
        );
    }

    #[test]
    fn placement_sides_and_alignment() {
        let mut layout = Layout::new(macbook_16());
        let peer = MachineId(2);
        let one = || Screens {
            displays: vec![legato_proto::Display {
                id: 9,
                bounds: Rect::new(100.0, 100.0, 200.0, 100.0),
                pixel_scale: 1.0,
                ui_scale: 1.0,
                primary: true,
                name: String::new(),
            }],
            native_per_desk: 1.0,
        };
        let cases = [
            (
                Side::Right,
                Align::Start,
                Rect::new(1728.0, 0.0, 200.0, 100.0),
            ),
            (
                Side::Left,
                Align::End,
                Rect::new(-200.0, 1017.0, 200.0, 100.0),
            ),
            (
                Side::Above,
                Align::Center,
                Rect::new(764.0, -100.0, 200.0, 100.0),
            ),
            (
                Side::Below,
                Align::End,
                Rect::new(1528.0, 1117.0, 200.0, 100.0),
            ),
        ];
        for (side, align, expected) in cases {
            assert!(layout.place_next_to_local(peer, one(), 0, side, align, 0.0));
            assert_eq!(
                layout.machine(peer).unwrap().desk_bounds().unwrap(),
                expected,
                "{side:?} {align:?}"
            );
        }
        assert!(!layout.place_next_to_local(peer, one(), 5, Side::Left, Align::Start, 0.0));
    }
}
