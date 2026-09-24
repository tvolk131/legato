//! Building the shared layout from connected peers and `legato.toml`.

use legato_core::{Layout, MachineId};
use legato_proto::{Point, Screens};

use crate::config::Config;

/// A connected peer, as the layout needs it.
pub struct ConnectedPeer<'a> {
    pub machine: MachineId,
    /// The peer's id, as text (config entries match on a prefix).
    pub id: String,
    pub name: &'a str,
    pub screens: &'a Screens,
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
        let Some(neighbor) = config
            .neighbors
            .iter()
            .find(|n| !n.peer.is_empty() && peer.id.starts_with(&n.peer))
        else {
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

#[cfg(test)]
mod tests {
    use legato_core::{Align, Side};
    use legato_proto::{Display, Rect};

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
            },
            ConnectedPeer {
                machine: MachineId(2),
                id: "zzz".into(),
                name: "Laptop",
                screens: &mac,
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
        }];
        let (_, problems) = build(&local, &peer, &config);
        assert!(problems[0].contains("display 3"), "{problems:?}");
    }
}
