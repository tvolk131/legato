//! Building the shared layout from connected peers and `legato.toml`.

use legato_core::{Align, Layout, MachineId};
use legato_net::EndpointId;
use legato_proto::{Point, Rect, Screens};

use crate::config::{Config, Neighbor};

pub mod snap;

/// A connected peer, as the layout needs it.
pub struct ConnectedPeer<'a> {
    pub machine: MachineId,
    /// The peer's id, as text (config entries match on a prefix).
    pub id: String,
    pub name: &'a str,
    pub screens: &'a Screens,
    /// Where the peer has placed this machine (this machine's desk origin in its desk
    /// space), used when there's no arrangement for it here.
    pub placed_us_at: Option<Point>,
}

/// Places every connected peer that has a configured position. Returns the layout and a
/// message for each peer that couldn't be placed.
pub fn build(
    local: &Screens,
    peers: &[ConnectedPeer<'_>],
    config: &Config,
) -> (Layout, Vec<String>) {
    let mut layout = Layout::new(local.clone());
    let mut problems = Vec::new();
    for peer in peers {
        let neighbor = config.neighbors.iter().find(|n| n.is_for(&peer.id));
        if neighbor.is_none()
            && let Some(o) = peer.placed_us_at
        {
            // Mirror the peer's arrangement: it put us at `o`, so it sits at `-o`.
            layout.set_machine(peer.machine, peer.screens.clone(), Point::new(-o.x, -o.y));
            continue;
        }
        let Some(neighbor) = neighbor else {
            problems.push(format!(
                "\"{}\" has no position yet, so the cursor can't reach it. Run e.g. \
                 `legato layout \"{}\" --side below --display 2`.",
                peer.name, peer.name
            ));
            continue;
        };
        let placed = match neighbor.offset {
            Some([x, y]) => layout.place_at(peer.machine, peer.screens.clone(), Point::new(x, y)),
            None => {
                neighbor.display >= 1
                    && layout.place_next_to_local(
                        peer.machine,
                        peer.screens.clone(),
                        neighbor.display - 1,
                        neighbor.side,
                        neighbor.align,
                        neighbor.nudge,
                    )
            }
        };
        if !placed {
            problems.push(format!(
                "\"{}\" is set to sit next to display {}, but this machine has {} display(s). \
                 See `legato doctor`.",
                peer.name,
                neighbor.display,
                local.displays.len()
            ));
        }
    }
    (layout, problems)
}

/// A paired peer with known displays, as the arrangement editor needs it.
pub struct EditorPeer<'a> {
    pub id: EndpointId,
    pub name: &'a str,
    pub screens: &'a Screens,
    /// Where the peer has placed this machine, if it has (see [`ConnectedPeer`]).
    pub placed_us_at: Option<Point>,
}

/// A paired machine's displays, where they currently sit (desk units).
#[derive(Debug, Clone, PartialEq)]
pub struct PeerBox {
    pub id: EndpointId,
    pub name: String,
    pub displays: Vec<Rect>,
    /// Index of its primary display, if it reports one.
    pub primary: Option<usize>,
    /// Whether it has a position yet. Unplaced peers are parked to the right.
    pub placed: bool,
}

impl PeerBox {
    pub fn bounds(&self) -> Option<Rect> {
        legato_core::layout::bounding_box(self.displays.iter().copied())
    }
}

/// What the arrangement editor shows: this machine's displays and every paired peer
/// whose displays are known, positioned by the settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    /// This machine's displays in desk units, left to right.
    pub local: Vec<Rect>,
    pub primary: Option<usize>,
    pub peers: Vec<PeerBox>,
    /// Why some peers couldn't be placed as configured.
    pub problems: Vec<String>,
}

impl Editor {
    pub fn new(local: &Screens, peers: &[EditorPeer<'_>], config: &Config) -> Self {
        let local_rects: Vec<_> = Layout::new(local.clone()).local().desk_rects().collect();
        let primary = local.displays.iter().position(|d| d.primary);
        let connected: Vec<_> = peers
            .iter()
            .enumerate()
            .map(|(i, p)| ConnectedPeer {
                machine: MachineId(i as u32 + 1),
                id: p.id.to_string(),
                name: p.name,
                screens: p.screens,
                placed_us_at: p.placed_us_at,
            })
            .collect();
        let (layout, problems) = build(local, &connected, config);
        let mut unplaced_x = legato_core::layout::bounding_box(local_rects.iter().copied())
            .map_or(0.0, |b| b.right() + 200.0);
        let peers = peers
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let machine = MachineId(i as u32 + 1);
                let (displays, placed) = match layout.machine(machine) {
                    Some(placed) => (placed.desk_rects().collect(), true),
                    None => {
                        // Not placed yet: park it to the right of this machine's displays.
                        let mut l = Layout::new(local.clone());
                        l.place_at(machine, p.screens.clone(), Point::new(unplaced_x, 0.0));
                        let rects: Vec<_> = l
                            .machine(machine)
                            .map(|m| m.desk_rects().collect())
                            .unwrap_or_default();
                        if let Some(b) = legato_core::layout::bounding_box(rects.iter().copied()) {
                            unplaced_x = b.right() + 200.0;
                        }
                        (rects, false)
                    }
                };
                PeerBox {
                    id: p.id,
                    name: p.name.to_string(),
                    displays,
                    primary: p.screens.displays.iter().position(|d| d.primary),
                    placed,
                }
            })
            .collect();
        Self {
            local: local_rects,
            primary,
            peers,
            problems,
        }
    }

    /// Where peer `id` lands when dropped with its top-left at `top_left`, if anywhere.
    pub fn snap(&self, id: EndpointId, top_left: Point) -> Option<snap::Snapped> {
        let b = self.peers.iter().find(|p| p.id == id)?.bounds()?;
        snap::snap(
            &self.local,
            Rect::new(top_left.x, top_left.y, b.width, b.height),
        )
    }
}

/// Saves where the arrangement editor put peer `id`, replacing any earlier position.
pub fn place(config: &mut Config, id: EndpointId, snapped: snap::Snapped) {
    let id = id.to_string();
    config.forget_neighbor(&id);
    config.neighbors.push(Neighbor {
        peer: id,
        side: snapped.side,
        display: snapped.display + 1,
        align: Align::Center,
        nudge: 0.0,
        offset: Some([snapped.top_left.x, snapped.top_left.y]),
    });
}

#[cfg(test)]
mod tests {
    use legato_core::{Align, Side};
    use legato_proto::{Display, Point, Rect};

    use super::*;
    use crate::config::Neighbor;

    fn display(x: f64, w: f64, h: f64, primary: bool, ui: f64) -> Display {
        Display {
            id: 0,
            bounds: Rect::new(x, 0.0, w, h),
            pixel_scale: 1.0,
            ui_scale: ui,
            primary,
            name: String::new(),
        }
    }

    #[test]
    fn places_configured_peers_and_reports_the_rest() {
        let windows = Screens {
            displays: vec![
                display(-3840.0, 3840.0, 2160.0, false, 1.5),
                display(0.0, 3840.0, 2160.0, true, 1.5),
                display(3840.0, 3840.0, 2160.0, false, 1.5),
            ],
            native_per_desk: 1.5,
        };
        let mac = Screens {
            displays: vec![display(0.0, 1728.0, 1117.0, true, 1.0)],
            native_per_desk: 1.0,
        };
        let config = Config {
            neighbors: vec![Neighbor {
                peer: "abc".into(),
                side: Side::Below,
                display: 2,
                align: Align::Center,
                nudge: 0.0,
                offset: None,
            }],
            ..Default::default()
        };
        let peers = [
            ConnectedPeer {
                machine: MachineId(1),
                id: "abcdef".into(),
                name: "MacBook",
                screens: &mac,
                placed_us_at: None,
            },
            ConnectedPeer {
                machine: MachineId(2),
                id: "zzz".into(),
                name: "Laptop",
                screens: &mac,
                placed_us_at: None,
            },
        ];
        let (layout, problems) = build(&windows, &peers, &config);
        assert_eq!(
            layout
                .display_at(Point::new(1280.0, 1441.0))
                .map(|(m, _)| m),
            Some(MachineId(1))
        );
        assert!(layout.machine(MachineId(2)).is_none());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("Laptop"), "{problems:?}");
    }

    #[test]
    fn mirrors_the_peers_arrangement_when_there_is_none_here() {
        let mac = Screens {
            displays: vec![display(0.0, 1728.0, 1117.0, true, 1.0)],
            native_per_desk: 1.0,
        };
        let windows = Screens {
            displays: vec![
                display(-3840.0, 3840.0, 2160.0, false, 1.5),
                display(0.0, 3840.0, 2160.0, true, 1.5),
                display(3840.0, 3840.0, 2160.0, false, 1.5),
            ],
            native_per_desk: 1.5,
        };
        // Windows put the Mac's origin at (416, 1440) in its desk space.
        let peer = [ConnectedPeer {
            machine: MachineId(1),
            id: "win".into(),
            name: "PC",
            screens: &windows,
            placed_us_at: Some(Point::new(416.0, 1440.0)),
        }];
        let (layout, problems) = build(&mac, &peer, &Config::default());
        assert!(problems.is_empty(), "{problems:?}");
        // Just above the Mac's top edge is the PC's middle monitor.
        assert_eq!(
            layout.display_at(Point::new(10.0, -1.0)).map(|(m, _)| m),
            Some(MachineId(1))
        );
    }

    #[test]
    fn out_of_range_display_is_reported() {
        let local = Screens {
            displays: vec![display(0.0, 100.0, 100.0, true, 1.0)],
            native_per_desk: 1.0,
        };
        let config = Config {
            neighbors: vec![Neighbor {
                peer: "a".into(),
                side: Side::Right,
                display: 3,
                align: Align::Center,
                nudge: 0.0,
                offset: None,
            }],
            ..Default::default()
        };
        let peer = [ConnectedPeer {
            machine: MachineId(1),
            id: "a".into(),
            name: "Peer",
            screens: &local,
            placed_us_at: None,
        }];
        let (_, problems) = build(&local, &peer, &config);
        assert!(problems[0].contains("display 3"), "{problems:?}");
    }

    #[test]
    fn editor_parks_unplaced_peers_and_drops_snap_into_saved_positions() {
        let mac = Screens {
            displays: vec![display(0.0, 1728.0, 1117.0, true, 1.0)],
            native_per_desk: 1.0,
        };
        let pc = Screens {
            displays: vec![
                display(0.0, 2560.0, 1440.0, true, 1.0),
                display(2560.0, 2560.0, 1440.0, false, 1.0),
            ],
            native_per_desk: 1.0,
        };
        let id = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let peers = [EditorPeer {
            id,
            name: "PC",
            screens: &pc,
            placed_us_at: None,
        }];
        let mut config = Config {
            // A blank entry must not count as this peer's position, or be removed.
            neighbors: vec![Neighbor {
                peer: String::new(),
                side: Side::Below,
                display: 1,
                align: Align::Center,
                nudge: 0.0,
                offset: None,
            }],
            ..Default::default()
        };

        let editor = Editor::new(&mac, &peers, &config);
        assert_eq!(editor.primary, Some(0));
        let parked = &editor.peers[0];
        assert!(!parked.placed);
        assert_eq!(parked.primary, Some(0));
        assert!(parked.bounds().unwrap().x >= 1728.0 + 200.0, "{parked:?}");
        assert_eq!(editor.problems.len(), 1, "{:?}", editor.problems);

        // Dropped just above the Mac: it lands flush against the top edge.
        let snapped = editor.snap(id, Point::new(-1000.0, -1500.0)).unwrap();
        assert_eq!(snapped.side, Side::Above);
        assert_eq!(snapped.top_left.y, -1440.0);
        place(&mut config, id, snapped);
        place(&mut config, id, snapped);
        assert_eq!(config.neighbors.len(), 2, "replaced, not added twice");

        let editor = Editor::new(&mac, &peers, &config);
        assert!(editor.peers[0].placed);
        assert!(editor.problems.is_empty(), "{:?}", editor.problems);
        assert_eq!(editor.peers[0].bounds().unwrap().bottom(), 0.0);

        assert!(config.forget_neighbor(&id.to_string()));
        assert!(!config.forget_neighbor(&id.to_string()));
        assert_eq!(config.neighbors.len(), 1);
        assert!(config.neighbors[0].peer.is_empty());
    }
}
