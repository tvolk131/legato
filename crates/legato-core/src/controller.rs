//! The controlling side: decides when the cursor leaves this machine, tracks the shared
//! cursor while it is on a peer, and routes keys and buttons.
//!
//! The capture backend feeds every OS input event through [`Controller::handle`], which
//! must return synchronously whether the event is swallowed or passed to the local OS.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use legato_proto::{Button, Control, Datagram, Point, Rect, Scroll};

use crate::keymap::KeyRemap;
use crate::layout::{Layout, MachineId};

#[derive(Debug, Clone, PartialEq)]
pub struct ControllerConfig {
    /// How far (desk units) the pointer must be pushed past an edge before it crosses to
    /// the neighbouring machine. Stops accidental switches when aiming at a taskbar or
    /// menu bar that sits on the shared edge. Zero switches immediately.
    pub push_distance: f64,
    /// Pushes separated by more than this start over.
    pub push_reset_after: Duration,
    /// Don't switch machines while a mouse button is held (e.g. mid-drag).
    pub block_switch_while_button_held: bool,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            push_distance: 30.0,
            push_reset_after: Duration::from_millis(250),
            block_switch_while_button_held: true,
        }
    }
}

/// One input event observed by the capture backend.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The local pointer moved to `pos` (local native coordinates, as the OS reports it,
    /// possibly clamped to the screen). `attempted` is the motion the user made for this
    /// event in local native units, even if the OS clamped it at a screen edge; it is what
    /// push-through is measured with. Only meaningful while not captured.
    LocalMotion {
        pos: Point,
        attempted: Point,
    },
    /// While captured: relative pointer motion in local native units.
    CapturedMotion {
        delta: Point,
    },
    /// A key, as a HID usage. The OS's auto-repeat arrives as repeated `down: true`.
    Key {
        usage: u16,
        down: bool,
    },
    Button {
        button: Button,
        down: bool,
    },
    Scroll(Scroll),
    /// The peer saw physical input and wants its cursor back. While driving it, this
    /// machine parks: the pointer stays there, and this machine carries on from wherever
    /// the peer's own mouse leaves it (see [`Event::PeerPointer`]).
    PeerYield(MachineId),
    /// The peer came across onto this machine's screens (it sent `Enter` with this `seq`):
    /// the pointer is here now, where the peer put it. Its reports from before are stale.
    PeerEntered {
        peer: MachineId,
        seq: u32,
    },
    /// This machine's own keyboard or mouse took over from the peer driving it (which was
    /// sent `Yield`). While this machine's own pointer moves on its screens, that peer is
    /// told where, so it can carry on from there.
    YieldedTo(MachineId),
    /// A peer's own pointer (moved by its own mouse or trackpad) is at `pos`, in its
    /// native coordinates. Newer `seq` wins.
    PeerPointer {
        peer: MachineId,
        seq: u32,
        pos: Point,
    },
    /// The connection to the peer dropped.
    PeerLost(MachineId),
    /// The user is dragging these files (or stopped: `None`). While files are carried the
    /// cursor may cross with the button held; releasing it on a peer drops them there.
    Carrying(Option<Vec<std::path::PathBuf>>),
    /// The pointer moved over the picture in the [`Portal`] window, to `at` (0..1 across
    /// and down the picture). Sent instead of `LocalMotion` while it's there, with the
    /// same `pos` and `attempted` (a full-screen portal can be pushed past its screen's
    /// edge).
    PortalMotion {
        at: Point,
        pos: Point,
        attempted: Point,
    },
}

/// Where a picture of `size` sits when fitted into `area` (letterboxed, centred). The
/// portal window draws the peer's display this way, and the capture backend hit-tests it
/// the same way.
pub fn fit_picture(size: (f64, f64), area: Rect) -> Rect {
    let (w, h) = size;
    if w <= 0.0 || h <= 0.0 || area.width <= 0.0 || area.height <= 0.0 {
        return area;
    }
    let scale = (area.width / w).min(area.height / h);
    let (fw, fh) = (w * scale, h * scale);
    Rect::new(
        area.x + (area.width - fw) / 2.0,
        area.y + (area.height - fh) / 2.0,
        fw,
        fh,
    )
}

/// Where `pos` (the peer's native coordinates) is on a portal's picture showing `remote`:
/// 0..1 across and down it.
fn portal_at(remote: Rect, pos: Point) -> Point {
    Point::new(
        ((pos.x - remote.x) / remote.width).clamp(0.0, 1.0),
        ((pos.y - remote.y) / remote.height).clamp(0.0, 1.0),
    )
}

/// Virtual monitor mode: a window on this machine showing one of a peer's displays. While
/// the pointer is over the picture, input goes to that display, placed absolutely, and the
/// local cursor keeps moving as usual.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Portal {
    pub peer: MachineId,
    /// The display shown, in the peer's native coordinates.
    pub remote: Rect,
    /// The window it's shown in, as the capture backend knows it (an `HWND` on Windows).
    pub window: u64,
}

/// One of this machine's displays that isn't shared, shown by a peer: a Mac's extra
/// display, in a window or full screen on a PC. While this machine's own pointer is on
/// it, the peer is told where, so it can show its cursor there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown {
    pub peer: MachineId,
    /// The display, in this machine's native coordinates.
    pub display: Rect,
    /// The display's id, so the backend can follow it if it moves.
    pub id: u32,
    /// Where the peer shows it: the picture's rect in the peer's native coordinates, once
    /// known. Its edges lead onto the peer's own screens beside it.
    pub picture: Option<Rect>,
}

/// Sent to a capture backend's thread from elsewhere.
#[derive(Debug)]
pub enum CaptureCommand {
    /// A peer yielded or disconnected.
    Event(Event),
    SetLayout(Layout),
    SetRemap(MachineId, KeyRemap),
    SetConfig(ControllerConfig),
    SetPortal(Option<Portal>),
    /// Shows (or stops showing) one of this machine's displays on a peer.
    SetShown(Option<Shown>),
    /// Where the peer shows that display (see [`Shown::picture`]).
    SetShownPicture(Option<Rect>),
    Stop,
}

/// Whether the backend should let the OS see the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Swallow,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send a reliable control message.
    Send { to: MachineId, msg: Control },
    /// Send an unreliable datagram.
    Datagram { to: MachineId, msg: Datagram },
    /// Start capturing: swallow all pointer and keyboard input, pin and hide the local
    /// cursor, and report motion as [`Event::CapturedMotion`].
    Capture,
    /// Stop capturing and show the local cursor at `warp` (local native coordinates).
    Release { warp: Point },
    /// Stop capturing and show the local cursor over the portal's picture, at `at` (0..1
    /// across and down it): the peer's cursor moved onto the display the portal shows.
    EnterPortal { at: Point },
    /// Stop capturing and show the local cursor where it is: the peer's pointer came
    /// across onto this machine's screens and put it there.
    Unpark,
    /// Move the local cursor over the portal's picture, at `at` (0..1 across and down it),
    /// without capturing: the peer's own pointer is there, on the display the portal
    /// shows. The move mustn't count as this machine's own input.
    ShowOnPortal { at: Point },
    /// The user let go of dragged files over a peer: send them there.
    Drop {
        to: MachineId,
        files: Vec<std::path::PathBuf>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Route {
    Local,
    Peer(MachineId),
    /// Went to a peer we've since left: the peer already released it, so the matching
    /// release is swallowed and goes nowhere.
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    fn unit(self) -> Point {
        match self {
            Dir::Left => Point::new(-1.0, 0.0),
            Dir::Right => Point::new(1.0, 0.0),
            Dir::Up => Point::new(0.0, -1.0),
            Dir::Down => Point::new(0.0, 1.0),
        }
    }

    /// Component of `v` pointing in this direction (negative if pointing away).
    fn component(self, v: Point) -> f64 {
        let u = self.unit();
        u.x * v.x + u.y * v.y
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Push {
    target: MachineId,
    dir: Dir,
    distance: f64,
    last: Instant,
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    Local,
    Remote {
        peer: MachineId,
        /// Shared cursor, in desk units.
        cursor: Point,
        /// Where the local cursor reappears (local native coordinates) if the peer goes
        /// away.
        return_to: Point,
        /// Parked: the peer's own keyboard or mouse is driving it. This machine's cursor
        /// stays hidden, `cursor` follows the peer's reports, and this machine's next
        /// input carries on from there.
        idle: bool,
    },
    /// The pointer is over the portal window.
    Portal {
        peer: MachineId,
        /// The peer's cursor, in its native coordinates.
        pos: Point,
    },
}

pub struct Controller {
    config: ControllerConfig,
    layout: Layout,
    remaps: HashMap<MachineId, KeyRemap>,
    state: State,
    push: Option<Push>,
    /// Incremented for every `Enter` and every motion datagram, so the receiver can drop
    /// stale or reordered motion.
    seq: u32,
    keys: HashMap<u16, Route>,
    buttons: HashMap<Button, Route>,
    carrying: Option<Vec<std::path::PathBuf>>,
    portal: Option<Portal>,
    shown: Option<Shown>,
    /// The peer that last had the pointer, before this machine did (it came home from
    /// there, or this machine's own input took over from it): while this machine's own
    /// pointer moves here, that peer is told where, so it can carry on from there.
    watcher: Option<MachineId>,
    /// The newest [`Event::PeerPointer`] used from each peer.
    pointer_seq: HashMap<MachineId, u32>,
    /// The local pointer's last position (local native).
    last_local: Point,
    /// The local pointer was last on the shown display.
    on_shown: bool,
}

impl Controller {
    pub fn new(config: ControllerConfig, layout: Layout) -> Self {
        Self {
            config,
            layout,
            remaps: HashMap::new(),
            state: State::Local,
            push: None,
            seq: 0,
            keys: HashMap::new(),
            buttons: HashMap::new(),
            carrying: None,
            portal: None,
            shown: None,
            watcher: None,
            pointer_seq: HashMap::new(),
            last_local: Point::default(),
            on_shown: false,
        }
    }

    pub fn portal(&self) -> Option<&Portal> {
        self.portal.as_ref()
    }

    /// Shows (or stops showing) a peer's display in a window here.
    pub fn set_portal(&mut self, portal: Option<Portal>, out: &mut Vec<Action>) {
        if let State::Portal { peer, .. } = self.state
            && portal.is_none_or(|p| p.peer != peer)
        {
            self.leave_portal(peer, out);
        }
        self.portal = portal;
    }

    /// One of this machine's displays is shown by a peer (or no longer is).
    pub fn set_shown(&mut self, shown: Option<Shown>) {
        self.shown = shown;
    }

    pub fn shown(&self) -> Option<&Shown> {
        self.shown.as_ref()
    }

    /// Where the peer shows the shown display (see [`Shown::picture`]).
    pub fn set_shown_picture(&mut self, picture: Option<Rect>) {
        if let Some(shown) = &mut self.shown {
            shown.picture = picture;
        }
    }

    /// Whether files are being carried.
    pub fn is_carrying(&self) -> bool {
        self.carrying.is_some()
    }

    /// The peer the local pointer would cross to by pushing past an edge at `pos` (local
    /// native coordinates), if any.
    pub fn neighbor_at_edge(&self, pos: Point) -> Option<MachineId> {
        let local = self.layout.local();
        let display = local
            .screens
            .displays
            .iter()
            .map(|d| d.bounds)
            .find(|b| b.contains(pos))?;
        [Dir::Left, Dir::Right, Dir::Up, Dir::Down]
            .into_iter()
            .filter(|&dir| at_native_edge(display, pos, dir))
            .find_map(|dir| {
                let beyond = add(
                    local.to_desk(edge_point_native(display, pos, dir)),
                    scale(dir.unit(), 0.5),
                );
                match self.layout.display_at(beyond) {
                    Some((MachineId::LOCAL, _)) | None => None,
                    Some((target, _)) => Some(target),
                }
            })
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Replaces the layout. If the cursor is on a peer that disappeared, control returns
    /// to the local machine.
    pub fn set_layout(&mut self, layout: Layout, out: &mut Vec<Action>) {
        self.layout = layout;
        if let State::Portal { peer, .. } = self.state
            && self.layout.machine(peer).is_none()
        {
            self.leave_portal(peer, out);
        }
        if let State::Remote { peer, cursor, .. } = self.state {
            match self.layout.machine(peer) {
                None => {
                    self.exit_to_local(false, out);
                    self.drop_routes(peer);
                }
                Some(_) => {
                    let clamped = self.clamp_to_machine(peer, cursor);
                    if let State::Remote { cursor, .. } = &mut self.state {
                        *cursor = clamped;
                    }
                }
            }
        }
    }

    pub fn set_config(&mut self, config: ControllerConfig) {
        self.config = config;
        self.push = None;
    }

    pub fn set_remap(&mut self, peer: MachineId, remap: KeyRemap) {
        self.remaps.insert(peer, remap);
    }

    /// The peer the cursor is currently on, if any.
    pub fn active_peer(&self) -> Option<MachineId> {
        match self.state {
            State::Local => None,
            State::Remote { peer, .. } | State::Portal { peer, .. } => Some(peer),
        }
    }

    pub fn handle(&mut self, now: Instant, event: Event, out: &mut Vec<Action>) -> Verdict {
        match event {
            Event::LocalMotion { pos, attempted } => self.local_motion(now, pos, attempted, out),
            Event::CapturedMotion { delta } => {
                self.captured_motion(now, delta, out);
                Verdict::Swallow
            }
            Event::Key { usage, down } => self.key(usage, down, out),
            Event::Button { button, down } => self.button(button, down, out),
            Event::Scroll(scroll) => self.scroll(scroll, out),
            Event::Carrying(files) => {
                self.carrying = files;
                Verdict::Pass
            }
            Event::PortalMotion { at, pos, attempted } => {
                self.portal_motion(now, at, pos, attempted, out)
            }
            Event::PeerYield(peer) => {
                match &mut self.state {
                    State::Remote { peer: p, idle, .. } if *p == peer => {
                        // The pointer stays over there: park, and carry on from wherever
                        // the peer's own mouse takes it.
                        *idle = true;
                        self.push = None;
                    }
                    State::Portal { peer: p, .. } if *p == peer => self.exit_to_local(true, out),
                    _ => {}
                }
                self.drop_routes(peer);
                Verdict::Pass
            }
            Event::PeerLost(peer) => {
                if self.active_peer() == Some(peer) {
                    self.exit_to_local(false, out);
                }
                self.drop_routes(peer);
                if self.watcher == Some(peer) {
                    self.watcher = None;
                }
                self.pointer_seq.remove(&peer);
                Verdict::Pass
            }
            Event::PeerEntered { peer, seq } => {
                if self.watcher == Some(peer) {
                    // It's driving this machine now; it knows where the pointer is.
                    self.watcher = None;
                }
                // Its reports sent before it came across (they can arrive after) are stale.
                if self
                    .pointer_seq
                    .get(&peer)
                    .is_none_or(|&last| newer(seq, last))
                {
                    self.pointer_seq.insert(peer, seq);
                }
                if let State::Remote { peer: p, .. } = self.state
                    && p == peer
                {
                    self.state = State::Local;
                    self.push = None;
                    out.push(Action::Unpark);
                }
                Verdict::Pass
            }
            Event::YieldedTo(peer) => {
                self.watcher = Some(peer);
                Verdict::Pass
            }
            Event::PeerPointer { peer, seq, pos } => {
                self.peer_pointer(peer, seq, pos, out);
                Verdict::Pass
            }
        }
    }

    /// A peer's own mouse moved its pointer to `pos` (its native coordinates).
    fn peer_pointer(&mut self, peer: MachineId, seq: u32, pos: Point, out: &mut Vec<Action>) {
        if self
            .pointer_seq
            .get(&peer)
            .is_some_and(|&last| !newer(seq, last))
        {
            return;
        }
        self.pointer_seq.insert(peer, seq);
        let Some(machine) = self.layout.machine(peer) else {
            return;
        };
        // On the display a portal here shows (a Mac's extra display), or on one of the
        // peer's shared screens. Anywhere else is a report out of date with the layout.
        let on_portal = self
            .portal
            .filter(|p| p.peer == peer && p.remote.contains(pos))
            .map(|p| portal_at(p.remote, pos));
        let on_screens = machine
            .screens
            .displays
            .iter()
            .any(|d| d.bounds.contains(pos));
        let desk = machine.to_desk(pos);
        match self.state {
            State::Remote {
                peer: p,
                idle: true,
                ..
            } if p == peer => {
                if let Some(at) = on_portal {
                    // Onto its display shown here: the pointer is on this machine's
                    // screen now, so its cursor shows it.
                    self.state = State::Local;
                    self.push = None;
                    self.watcher = Some(peer);
                    out.push(Action::Unpark);
                    out.push(Action::ShowOnPortal { at });
                } else if on_screens {
                    let clamped = self.clamp_to_machine(peer, desk);
                    if let State::Remote { cursor, .. } = &mut self.state {
                        *cursor = clamped;
                    }
                }
            }
            State::Local => {
                if let Some(at) = on_portal {
                    out.push(Action::ShowOnPortal { at });
                    // This machine's cursor is the pointer now: if this machine's own mouse
                    // takes it elsewhere, the peer hears where.
                    self.watcher = Some(peer);
                } else if on_screens {
                    // Its own mouse took the pointer onto its own screens: it's there, not
                    // here. Park, so this machine's next input carries on from there.
                    self.push = None;
                    self.state = State::Remote {
                        peer,
                        cursor: self.clamp_to_machine(peer, desk),
                        return_to: self.last_local,
                        idle: true,
                    };
                    out.push(Action::Capture);
                }
            }
            // This machine's own input is driving: it knows where the pointer is.
            State::Remote { .. } | State::Portal { .. } => {}
        }
    }

    /// This machine's own input while parked: take the pointer back, from wherever the
    /// peer's own mouse left it.
    fn resume(&mut self, out: &mut Vec<Action>) {
        let State::Remote {
            peer,
            cursor,
            idle: true,
            ..
        } = self.state
        else {
            return;
        };
        let Some(machine) = self.layout.machine(peer) else {
            return;
        };
        self.seq = self.seq.wrapping_add(1);
        out.push(Action::Send {
            to: peer,
            msg: Control::Enter {
                seq: self.seq,
                pos: machine.to_native(cursor),
            },
        });
        if let State::Remote { idle, .. } = &mut self.state {
            *idle = false;
        }
    }

    fn local_motion(
        &mut self,
        now: Instant,
        pos: Point,
        attempted: Point,
        out: &mut Vec<Action>,
    ) -> Verdict {
        if let State::Portal { peer, pos: at } = self.state {
            if self.holding_on(peer) {
                // Dragging something off the portal's picture: carry on on the peer's own
                // displays rather than letting go at the edge.
                let dir = self
                    .portal
                    .map_or(Dir::Left, |p| nearest_edge(p.remote, at));
                self.portal_to_remote(peer, dir, pos, out);
                return Verdict::Swallow;
            }
            // Off the portal's picture: back to this machine.
            self.leave_portal(peer, out);
        }
        if self.active_peer().is_some() {
            // Shouldn't happen while captured; don't let it move the shared cursor.
            return Verdict::Swallow;
        }
        self.last_local = pos;
        let on_shown = self.shown.filter(|s| s.display.contains(pos));
        // Just off it, the peer that shows it hears once more, to stop showing it.
        let left_shown = std::mem::replace(&mut self.on_shown, on_shown.is_some());
        // Tell the peer that shows this display, and the one this machine last took over
        // from, where this machine's own pointer is: it's theirs to show, or to carry on
        // from.
        let told: Vec<MachineId> = on_shown
            .or(self.shown.filter(|_| left_shown))
            .map(|s| s.peer)
            .into_iter()
            .chain(self.watcher)
            .collect();
        for (i, &to) in told.iter().enumerate() {
            if told[..i].contains(&to) {
                continue;
            }
            self.seq = self.seq.wrapping_add(1);
            out.push(Action::Datagram {
                to,
                msg: Datagram::Pointer { seq: self.seq, pos },
            });
        }
        if let Some(shown) = on_shown {
            // On a display a peer shows: the pointer stays this machine's, unless it's
            // pushed off an edge onto the peer's own screens beside the picture.
            let Some((target, dir, pushing, entry)) =
                self.shown_edge_crossing(shown, pos, attempted)
            else {
                self.push = None;
                return Verdict::Pass;
            };
            if !self.accumulate_push(now, target, dir, pushing) || self.switch_blocked() {
                return Verdict::Pass;
            }
            self.enter_peer(target, entry, pos, out);
            out.push(Action::Capture);
            return Verdict::Swallow;
        }
        let Some((target, dir, pushing, entry)) = self.edge_crossing(pos, attempted) else {
            self.push = None;
            return Verdict::Pass;
        };
        if !self.accumulate_push(now, target, dir, pushing) || self.switch_blocked() {
            return Verdict::Pass;
        }
        self.enter_peer(target, entry, pos, out);
        out.push(Action::Capture);
        Verdict::Swallow
    }

    /// The peer beyond the edge of this machine's displays that the pointer at `pos` is
    /// pushing into, with the direction, how far it pushed (desk units), and where it
    /// would enter (desk).
    fn edge_crossing(&self, pos: Point, attempted: Point) -> Option<(MachineId, Dir, f64, Point)> {
        let layout = &self.layout;
        let local = layout.local();
        let display = local
            .screens
            .displays
            .iter()
            .map(|d| d.bounds)
            // Backends report positions on their displays (Windows clamps what its hook
            // reports). One on none of them is on a display this machine doesn't share,
            // like the extra display a Mac shows on a PC, where nothing is an edge.
            .find(|b| b.contains(pos))?;
        // Which edges of its display is the pointer sitting on, and pushing into?
        let attempted_desk = scale(attempted, local.desk_per_native());
        for dir in [Dir::Left, Dir::Right, Dir::Up, Dir::Down] {
            let pushing = dir.component(attempted_desk);
            if pushing <= 0.0 || !at_native_edge(display, pos, dir) {
                continue;
            }
            // Probe just beyond the edge on the desk.
            let edge = edge_point_native(display, pos, dir);
            let beyond = add(local.to_desk(edge), scale(dir.unit(), 0.5));
            match layout.display_at(beyond) {
                // Another local display: the OS moves the cursor there itself.
                Some((MachineId::LOCAL, _)) | None => {}
                Some((target, _)) => return Some((target, dir, pushing, beyond)),
            }
        }
        None
    }

    /// Pushing off an edge of the shown display with none of this machine's displays
    /// beyond: onto the peer's screens just beside the same spot on the picture, if the
    /// peer has one there. Like [`Self::edge_crossing`]'s result.
    fn shown_edge_crossing(
        &self,
        shown: Shown,
        pos: Point,
        attempted: Point,
    ) -> Option<(MachineId, Dir, f64, Point)> {
        let picture = shown.picture?;
        let machine = self.layout.machine(shown.peer)?;
        let local = self.layout.local();
        let display = shown.display;
        let attempted_desk = scale(attempted, local.desk_per_native());
        for dir in [Dir::Left, Dir::Right, Dir::Up, Dir::Down] {
            let pushing = dir.component(attempted_desk);
            if pushing <= 0.0 || !at_native_edge(display, pos, dir) {
                continue;
            }
            // Another of this machine's displays beyond: the OS moves the cursor there.
            let here = add(edge_point_native(display, pos, dir), scale(dir.unit(), 0.5));
            if local
                .screens
                .displays
                .iter()
                .any(|d| d.bounds.contains(here))
            {
                continue;
            }
            let at = portal_at(display, pos);
            let on_picture = Point::new(
                picture.x + at.x * picture.width,
                picture.y + at.y * picture.height,
            );
            let edge = machine.to_desk(edge_point_native(picture, on_picture, dir));
            let beyond = add(edge, scale(dir.unit(), 0.5));
            if let Some((target, _)) = self.layout.display_at(beyond)
                && target == shown.peer
            {
                return Some((target, dir, pushing, beyond));
            }
        }
        None
    }

    /// If `wanted` (desk) is on the picture of this machine's display that `peer` shows,
    /// comes home onto that display: the pointer's on this machine's display again.
    fn remote_to_shown(&mut self, peer: MachineId, wanted: Point, out: &mut Vec<Action>) -> bool {
        let Some(shown) = self.shown.filter(|s| s.peer == peer) else {
            return false;
        };
        if self.holding_on(peer) {
            // Dragging something on the peer across the picture: keep dragging it there.
            return false;
        }
        let (Some(picture), Some(machine)) = (shown.picture, self.layout.machine(peer)) else {
            return false;
        };
        let native = machine.to_native(wanted);
        if !picture.contains(native) {
            return false;
        }
        let at = portal_at(picture, native);
        let d = shown.display;
        let warp = Point::new(
            (d.x + at.x * d.width).min(d.right() - 1.0),
            (d.y + at.y * d.height).min(d.bottom() - 1.0),
        );
        self.leave_peer(peer, out);
        self.state = State::Local;
        self.push = None;
        out.push(Action::Release { warp });
        self.came_home(peer, warp, out);
        true
    }

    /// The pointer came home from `peer` to `pos` (local native): the peer hears where it
    /// is now, and whenever this machine's own mouse moves it, so the peer carries on
    /// from there rather than from where it last had it.
    fn came_home(&mut self, peer: MachineId, pos: Point, out: &mut Vec<Action>) {
        self.watcher = Some(peer);
        self.last_local = pos;
        self.on_shown = self.shown.is_some_and(|s| s.display.contains(pos));
        self.seq = self.seq.wrapping_add(1);
        out.push(Action::Datagram {
            to: peer,
            msg: Datagram::Pointer { seq: self.seq, pos },
        });
    }

    /// Whether a mouse button pressed on `peer` is still held (e.g. dragging a window).
    fn holding_on(&self, peer: MachineId) -> bool {
        self.buttons.values().any(|&r| r == Route::Peer(peer))
    }

    /// Moves from the portal onto the peer's own displays, next to the portal's display
    /// in direction `dir`, keeping everything held: it's the same machine.
    fn portal_to_remote(
        &mut self,
        peer: MachineId,
        dir: Dir,
        return_to: Point,
        out: &mut Vec<Action>,
    ) {
        let State::Portal { pos, .. } = self.state else {
            return;
        };
        let Some(machine) = self.layout.machine(peer) else {
            return;
        };
        let beyond = add(pos, scale(dir.unit(), 2.0));
        let cursor = self.clamp_to_machine(peer, machine.to_desk(beyond));
        self.state = State::Remote {
            peer,
            cursor,
            return_to,
            idle: false,
        };
        self.push = None;
        out.push(Action::Capture);
        self.send_motion(peer, cursor, out);
    }

    fn portal_motion(
        &mut self,
        now: Instant,
        at: Point,
        local: Point,
        attempted: Point,
        out: &mut Vec<Action>,
    ) -> Verdict {
        let Some(portal) = self.portal else {
            return Verdict::Pass;
        };
        if matches!(self.state, State::Remote { .. }) || self.layout.machine(portal.peer).is_none()
        {
            // Captured (shouldn't happen), or not allowed to drive that peer.
            return Verdict::Pass;
        }
        let r = portal.remote;
        let pos = Point::new(
            (r.x + at.x.clamp(0.0, 1.0) * r.width).min(r.right() - 1.0),
            (r.y + at.y.clamp(0.0, 1.0) * r.height).min(r.bottom() - 1.0),
        );
        self.seq = self.seq.wrapping_add(1);
        match self.state {
            State::Portal { peer, .. } if peer == portal.peer => {
                out.push(Action::Datagram {
                    to: peer,
                    msg: Datagram::Motion { seq: self.seq, pos },
                });
            }
            _ => {
                self.push = None;
                out.push(Action::Send {
                    to: portal.peer,
                    msg: Control::Enter { seq: self.seq, pos },
                });
            }
        }
        self.state = State::Portal {
            peer: portal.peer,
            pos,
        };
        // A full-screen portal has no local screen around it to leave onto: pushing past
        // its screen's edge towards the same peer carries on on the peer's displays.
        match self.edge_crossing(local, attempted) {
            Some((target, dir, pushing, _)) if target == portal.peer => {
                if self.accumulate_push(now, target, dir, pushing) {
                    self.portal_to_remote(portal.peer, dir, local, out);
                    return Verdict::Swallow;
                }
            }
            _ => self.push = None,
        }
        Verdict::Pass
    }

    /// If `wanted` (desk) is on the display the portal shows, moves there: the local
    /// cursor reappears over the picture, and everything held stays held.
    fn remote_to_portal(&mut self, peer: MachineId, wanted: Point, out: &mut Vec<Action>) -> bool {
        let Some(portal) = self.portal.filter(|p| p.peer == peer) else {
            return false;
        };
        let Some(machine) = self.layout.machine(peer) else {
            return false;
        };
        let native = machine.to_native(wanted);
        let r = portal.remote;
        if !r.contains(native) {
            return false;
        }
        self.state = State::Portal { peer, pos: native };
        self.push = None;
        self.seq = self.seq.wrapping_add(1);
        out.push(Action::Datagram {
            to: peer,
            msg: Datagram::Motion {
                seq: self.seq,
                pos: native,
            },
        });
        out.push(Action::EnterPortal {
            at: Point::new((native.x - r.x) / r.width, (native.y - r.y) / r.height),
        });
        true
    }

    fn leave_portal(&mut self, peer: MachineId, out: &mut Vec<Action>) {
        self.leave_peer(peer, out);
        self.state = State::Local;
        // The pointer's this machine's again; its next motion tells the peer where.
        self.watcher = Some(peer);
    }

    fn captured_motion(&mut self, now: Instant, delta: Point, out: &mut Vec<Action>) {
        if delta.x != 0.0 || delta.y != 0.0 {
            self.resume(out);
        }
        let State::Remote { peer, cursor, .. } = self.state else {
            return;
        };
        let wanted = add(cursor, scale(delta, self.layout.local().desk_per_native()));
        if self.remote_to_portal(peer, wanted, out) || self.remote_to_shown(peer, wanted, out) {
            return;
        }
        let clamped = self.clamp_to_machine(peer, wanted);
        let overshoot = sub(wanted, clamped);

        // Pushing past one of the peer's outer edges towards another machine?
        let mut crossed = false;
        if overshoot.x != 0.0 || overshoot.y != 0.0 {
            let dir = dominant_dir(overshoot);
            let beyond = add(
                clamped,
                scale(dir.unit(), 0.5 + self.peer_edge_epsilon(peer)),
            );
            match self.layout.display_at(beyond) {
                Some((target, _)) if target != peer => {
                    let pushing = dir.component(overshoot);
                    if self.accumulate_push(now, target, dir, pushing) && !self.switch_blocked() {
                        self.leave_peer(peer, out);
                        if target == MachineId::LOCAL {
                            self.state = State::Local;
                            let warp = self.layout.local().to_native(beyond);
                            out.push(Action::Release { warp });
                            self.came_home(peer, warp, out);
                        } else {
                            self.enter_peer(target, beyond, self.return_point(), out);
                        }
                        crossed = true;
                    }
                }
                _ => self.push = None,
            }
        } else {
            self.push = None;
        }

        if !crossed && clamped != cursor {
            if let State::Remote { cursor, .. } = &mut self.state {
                *cursor = clamped;
            }
            self.send_motion(peer, clamped, out);
        }
    }

    fn key(&mut self, usage: u16, down: bool, out: &mut Vec<Action>) -> Verdict {
        // Caps Lock toggles the machine it's pressed on, and connected machines share it
        // (see `crate::locks`): it isn't sent along, unless remapped to another key there.
        let remapped = match self.current_route() {
            Route::Peer(peer) => self.remaps.get(&peer).map_or(usage, |r| r.apply(usage)),
            Route::Local | Route::Dropped => usage,
        };
        if usage == crate::keymap::usage::CAPS_LOCK && remapped == usage {
            return Verdict::Pass;
        }
        if down && !self.keys.contains_key(&usage) {
            self.resume(out);
        }
        if down {
            if let Some(&route) = self.keys.get(&usage) {
                // OS auto-repeat. The receiving side repeats on its own.
                return match route {
                    Route::Local => Verdict::Pass,
                    Route::Peer(_) | Route::Dropped => Verdict::Swallow,
                };
            }
            let route = self.current_route();
            self.keys.insert(usage, route);
            self.send_key(route, usage, true, out)
        } else {
            match self.keys.remove(&usage) {
                Some(route) => self.send_key(route, usage, false, out),
                // A release we never saw pressed: follow the cursor.
                None => match self.current_route() {
                    Route::Local => Verdict::Pass,
                    Route::Peer(_) | Route::Dropped => Verdict::Swallow,
                },
            }
        }
    }

    fn send_key(&self, route: Route, usage: u16, down: bool, out: &mut Vec<Action>) -> Verdict {
        match route {
            Route::Local => Verdict::Pass,
            Route::Dropped => Verdict::Swallow,
            Route::Peer(peer) => {
                let usage = self.remaps.get(&peer).map_or(usage, |r| r.apply(usage));
                out.push(Action::Send {
                    to: peer,
                    msg: Control::Key { usage, down },
                });
                Verdict::Swallow
            }
        }
    }

    fn button(&mut self, button: Button, down: bool, out: &mut Vec<Action>) -> Verdict {
        if down {
            self.resume(out);
        }
        if !down
            && button == Button::Left
            && let Some(files) = self.carrying.take()
            && let Route::Peer(to) = self.current_route()
        {
            out.push(Action::Drop { to, files });
        }
        let route = if down {
            let route = self.current_route();
            self.buttons.insert(button, route);
            route
        } else {
            self.buttons
                .remove(&button)
                .unwrap_or_else(|| self.current_route())
        };
        match route {
            Route::Local => Verdict::Pass,
            Route::Dropped => Verdict::Swallow,
            Route::Peer(peer) => {
                // Clicks carry the position so they land where the user sees the cursor,
                // even if the last motion datagram was dropped.
                let pos = self.peer_cursor_native(peer).unwrap_or_default();
                out.push(Action::Send {
                    to: peer,
                    msg: Control::Button { button, down, pos },
                });
                Verdict::Swallow
            }
        }
    }

    fn scroll(&mut self, scroll: Scroll, out: &mut Vec<Action>) -> Verdict {
        self.resume(out);
        let Route::Peer(peer) = self.current_route() else {
            return Verdict::Pass;
        };
        let scroll = match scroll {
            // Continuous scrolling is in native units; convert to the peer's.
            Scroll::Pixels { x, y } => {
                let factor = self.layout.local().desk_per_native()
                    * self
                        .layout
                        .machine(peer)
                        .map_or(1.0, |m| m.screens.native_per_desk);
                Scroll::Pixels {
                    x: x * factor,
                    y: y * factor,
                }
            }
            wheel @ Scroll::Wheel { .. } => wheel,
        };
        out.push(Action::Send {
            to: peer,
            msg: Control::Scroll(scroll),
        });
        Verdict::Swallow
    }

    fn current_route(&self) -> Route {
        self.active_peer().map_or(Route::Local, Route::Peer)
    }

    fn switch_blocked(&self) -> bool {
        self.config.block_switch_while_button_held
            && !self.buttons.is_empty()
            && self.carrying.is_none()
    }

    /// Adds `pushing` to the push in progress; returns true once it's far enough to switch.
    fn accumulate_push(&mut self, now: Instant, target: MachineId, dir: Dir, pushing: f64) -> bool {
        let continuing = self.push.filter(|p| {
            p.target == target
                && p.dir == dir
                && now.saturating_duration_since(p.last) <= self.config.push_reset_after
        });
        let distance = continuing.map_or(0.0, |p| p.distance) + pushing;
        if distance >= self.config.push_distance {
            self.push = None;
            true
        } else {
            self.push = Some(Push {
                target,
                dir,
                distance,
                last: now,
            });
            false
        }
    }

    fn enter_peer(
        &mut self,
        peer: MachineId,
        entry: Point,
        return_to: Point,
        out: &mut Vec<Action>,
    ) {
        let cursor = self.clamp_to_machine(peer, entry);
        self.seq = self.seq.wrapping_add(1);
        let pos = self
            .layout
            .machine(peer)
            .map_or(Point::default(), |m| m.to_native(cursor));
        out.push(Action::Send {
            to: peer,
            msg: Control::Enter { seq: self.seq, pos },
        });
        self.state = State::Remote {
            peer,
            cursor,
            return_to,
            idle: false,
        };
        self.watcher = None;
    }

    fn leave_peer(&mut self, peer: MachineId, out: &mut Vec<Action>) {
        out.push(Action::Send {
            to: peer,
            msg: Control::Leave,
        });
        self.drop_routes(peer);
    }

    /// The peer has released (or lost) everything we pressed there.
    fn drop_routes(&mut self, peer: MachineId) {
        for route in self.keys.values_mut().chain(self.buttons.values_mut()) {
            if *route == Route::Peer(peer) {
                *route = Route::Dropped;
            }
        }
    }

    fn exit_to_local(&mut self, notify_peer: bool, out: &mut Vec<Action>) {
        if let State::Portal { peer, .. } = self.state {
            if notify_peer {
                self.leave_peer(peer, out);
            }
            self.state = State::Local;
            return;
        }
        let State::Remote {
            peer, return_to, ..
        } = self.state
        else {
            return;
        };
        if notify_peer {
            self.leave_peer(peer, out);
        }
        self.state = State::Local;
        self.push = None;
        out.push(Action::Release { warp: return_to });
    }

    fn return_point(&self) -> Point {
        match self.state {
            State::Remote { return_to, .. } => return_to,
            State::Local | State::Portal { .. } => Point::default(),
        }
    }

    fn send_motion(&mut self, peer: MachineId, cursor: Point, out: &mut Vec<Action>) {
        if let Some(m) = self.layout.machine(peer) {
            self.seq = self.seq.wrapping_add(1);
            out.push(Action::Datagram {
                to: peer,
                msg: Datagram::Motion {
                    seq: self.seq,
                    pos: m.to_native(cursor),
                },
            });
        }
    }

    fn peer_cursor_native(&self, peer: MachineId) -> Option<Point> {
        match self.state {
            State::Remote {
                peer: p, cursor, ..
            } if p == peer => self.layout.machine(peer).map(|m| m.to_native(cursor)),
            State::Portal { peer: p, pos } if p == peer => Some(pos),
            _ => None,
        }
    }

    /// One native unit of the peer, in desk units: the cursor may not sit on the
    /// exclusive right/bottom edge of a display.
    fn peer_edge_epsilon(&self, peer: MachineId) -> f64 {
        self.layout
            .machine(peer)
            .map_or(1.0, |m| m.desk_per_native())
    }

    /// Clamps a desk point onto the nearest point of the peer's displays.
    fn clamp_to_machine(&self, peer: MachineId, p: Point) -> Point {
        let Some(machine) = self.layout.machine(peer) else {
            return p;
        };
        let eps = machine.desk_per_native();
        let mut best: Option<(f64, Point)> = None;
        for r in machine.desk_rects() {
            let c = clamp_into(r, p, eps);
            let d = dist2(c, p);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, c));
            }
        }
        best.map_or(p, |(_, c)| c)
    }
}

/// The edge of `r` nearest to `p`.
fn nearest_edge(r: Rect, p: Point) -> Dir {
    [
        (Dir::Left, p.x - r.left()),
        (Dir::Right, r.right() - p.x),
        (Dir::Up, p.y - r.top()),
        (Dir::Down, r.bottom() - p.y),
    ]
    .into_iter()
    .min_by(|a, b| a.1.total_cmp(&b.1))
    .map_or(Dir::Left, |(d, _)| d)
}

fn clamp_into(r: Rect, p: Point, eps: f64) -> Point {
    Point::new(
        p.x.clamp(r.left(), (r.right() - eps).max(r.left())),
        p.y.clamp(r.top(), (r.bottom() - eps).max(r.top())),
    )
}

/// Is `pos` on the outermost native pixel row/column of `display` in direction `dir`?
fn at_native_edge(display: Rect, pos: Point, dir: Dir) -> bool {
    match dir {
        Dir::Left => pos.x < display.left() + 1.0,
        Dir::Right => pos.x >= display.right() - 1.0,
        Dir::Up => pos.y < display.top() + 1.0,
        Dir::Down => pos.y >= display.bottom() - 1.0,
    }
}

/// The point on the display's boundary line in direction `dir`, level with `pos`.
fn edge_point_native(display: Rect, pos: Point, dir: Dir) -> Point {
    match dir {
        Dir::Left => Point::new(display.left(), pos.y),
        Dir::Right => Point::new(display.right(), pos.y),
        Dir::Up => Point::new(pos.x, display.top()),
        Dir::Down => Point::new(pos.x, display.bottom()),
    }
}

/// Whether `seq` comes after `last`, allowing for wrapping.
fn newer(seq: u32, last: u32) -> bool {
    (seq.wrapping_sub(last) as i32) > 0
}

fn dominant_dir(v: Point) -> Dir {
    if v.x.abs() >= v.y.abs() {
        if v.x < 0.0 { Dir::Left } else { Dir::Right }
    } else if v.y < 0.0 {
        Dir::Up
    } else {
        Dir::Down
    }
}

fn add(a: Point, b: Point) -> Point {
    Point::new(a.x + b.x, a.y + b.y)
}

fn sub(a: Point, b: Point) -> Point {
    Point::new(a.x - b.x, a.y - b.y)
}

fn scale(p: Point, k: f64) -> Point {
    Point::new(p.x * k, p.y * k)
}

fn dist2(a: Point, b: Point) -> f64 {
    let d = sub(a, b);
    d.x * d.x + d.y * d.y
}

#[cfg(test)]
mod tests;
