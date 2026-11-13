//! Where a dragged machine lands in the arrangement editor.
//!
//! Machines must touch this machine's displays along an outer edge for the cursor to
//! cross, so a drop snaps to the nearest such position.

use legato_core::Side;
use legato_proto::{Point, Rect};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapped {
    /// Top-left of the machine's bounding box, in desk units.
    pub top_left: Point,
    /// Index of the local display it touches.
    pub display: usize,
    pub side: Side,
}

/// Snaps `peer` (a bounding box in desk units) flush against the outer edge of a local
/// display, sharing at least a sensible length of edge, without overlapping any local
/// display. Picks the position closest to where it was dropped.
pub fn snap(local: &[Rect], peer: Rect) -> Option<Snapped> {
    let mut best: Option<(f64, Snapped)> = None;
    for (i, l) in local.iter().enumerate() {
        for side in [Side::Below, Side::Above, Side::Left, Side::Right] {
            let (x, y) = match side {
                Side::Below | Side::Above => {
                    let overlap = min_overlap(peer.width, l.width);
                    let x = peer
                        .x
                        .clamp(l.left() - peer.width + overlap, l.right() - overlap);
                    let y = if side == Side::Below {
                        l.bottom()
                    } else {
                        l.top() - peer.height
                    };
                    (x, y)
                }
                Side::Left | Side::Right => {
                    let overlap = min_overlap(peer.height, l.height);
                    let y = peer
                        .y
                        .clamp(l.top() - peer.height + overlap, l.bottom() - overlap);
                    let x = if side == Side::Right {
                        l.right()
                    } else {
                        l.left() - peer.width
                    };
                    (x, y)
                }
            };
            let placed = Rect::new(x, y, peer.width, peer.height);
            if local.iter().any(|other| overlaps(*other, placed)) {
                continue;
            }
            let d = (x - peer.x).powi(2) + (y - peer.y).powi(2);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((
                    d,
                    Snapped {
                        top_left: Point::new(x, y),
                        display: i,
                        side,
                    },
                ));
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Enough shared edge to aim at comfortably, but never more than either side has.
fn min_overlap(a: f64, b: f64) -> f64 {
    (a.min(b) * 0.25).clamp(1.0, 200.0)
}

/// Interiors intersect (touching edges don't count).
fn overlaps(a: Rect, b: Rect) -> bool {
    const EPS: f64 = 1e-6;
    a.left() < b.right() - EPS
        && b.left() < a.right() - EPS
        && a.top() < b.bottom() - EPS
        && b.top() < a.bottom() - EPS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three 4K monitors at 150%, in desk units.
    fn triple() -> Vec<Rect> {
        vec![
            Rect::new(-2560.0, 0.0, 2560.0, 1440.0),
            Rect::new(0.0, 0.0, 2560.0, 1440.0),
            Rect::new(2560.0, 0.0, 2560.0, 1440.0),
        ]
    }

    #[test]
    fn a_macbook_dropped_under_the_middle_monitor_snaps_to_its_bottom() {
        let s = snap(&triple(), Rect::new(400.0, 1500.0, 1728.0, 1117.0)).unwrap();
        assert_eq!(s.side, Side::Below);
        assert_eq!(s.display, 1);
        assert_eq!(s.top_left, Point::new(400.0, 1440.0));
    }

    #[test]
    fn dropping_on_top_of_the_monitors_moves_it_out() {
        let s = snap(&triple(), Rect::new(100.0, 100.0, 1728.0, 1117.0)).unwrap();
        let placed = Rect::new(s.top_left.x, s.top_left.y, 1728.0, 1117.0);
        assert!(triple().iter().all(|l| !overlaps(*l, placed)), "{s:?}");
    }

    #[test]
    fn keeps_enough_shared_edge_to_cross() {
        // Dropped far off to the right of the rightmost monitor's bottom edge.
        let s = snap(&triple(), Rect::new(9000.0, 1600.0, 1728.0, 1117.0)).unwrap();
        let placed = Rect::new(s.top_left.x, s.top_left.y, 1728.0, 1117.0);
        let l = triple()[s.display];
        let shared = match s.side {
            Side::Below | Side::Above => {
                placed.right().min(l.right()) - placed.left().max(l.left())
            }
            _ => placed.bottom().min(l.bottom()) - placed.top().max(l.top()),
        };
        assert!(shared >= 200.0 - 1e-9, "{s:?} shares {shared}");
    }

    #[test]
    fn no_local_displays_means_nowhere_to_snap() {
        assert_eq!(snap(&[], Rect::new(0.0, 0.0, 10.0, 10.0)), None);
    }
}
