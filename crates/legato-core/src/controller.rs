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
    /// The peer saw physical input and wants its cursor back.
    PeerYield(MachineId),
    /// The connection to the peer dropped.
    PeerLost(MachineId),
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
        /// Where the local cursor reappears (local native coordinates) if the peer takes
        /// control back.
        return_to: Point,
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
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Replaces the layout. If the cursor is on a peer that disappeared, control returns
    /// to the local machine.
    pub fn set_layout(&mut self, layout: Layout, out: &mut Vec<Action>) {
        self.layout = layout;
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
            State::Remote { peer, .. } => Some(peer),
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
            Event::PeerYield(peer) | Event::PeerLost(peer) => {
                let yielded = matches!(event, Event::PeerYield(_));
                if self.active_peer() == Some(peer) {
                    self.exit_to_local(yielded, out);
                }
                self.drop_routes(peer);
                Verdict::Pass
            }
        }
    }

    fn local_motion(
        &mut self,
        now: Instant,
        pos: Point,
        attempted: Point,
        out: &mut Vec<Action>,
    ) -> Verdict {
        if self.active_peer().is_some() {
            // Shouldn't happen while captured; don't let it move the shared cursor.
            return Verdict::Swallow;
        }
        let layout = &self.layout;
        let local = layout.local();
        let Some(display) = local
            .screens
            .displays
            .iter()
            .map(|d| d.bounds)
            .find(|b| b.contains(pos))
            .or_else(|| nearest_rect(local.screens.displays.iter().map(|d| d.bounds), pos))
        else {
            return Verdict::Pass;
        };

        // Which edges of its display is the pointer sitting on, and pushing into?
        let attempted_desk = scale(attempted, local.desk_per_native());
        let mut candidate = None;
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
                Some((target, _)) => {
                    candidate = Some((target, dir, pushing, beyond));
                    break;
                }
            }
        }

        let Some((target, dir, pushing, entry)) = candidate else {
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

    fn captured_motion(&mut self, now: Instant, delta: Point, out: &mut Vec<Action>) {
        let State::Remote { peer, cursor, .. } = self.state else {
            return;
        };
        let wanted = add(cursor, scale(delta, self.layout.local().desk_per_native()));
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
        self.config.block_switch_while_button_held && !self.buttons.is_empty()
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
        };
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
            State::Local => Point::default(),
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

fn clamp_into(r: Rect, p: Point, eps: f64) -> Point {
    Point::new(
        p.x.clamp(r.left(), (r.right() - eps).max(r.left())),
        p.y.clamp(r.top(), (r.bottom() - eps).max(r.top())),
    )
}

fn nearest_rect(rects: impl Iterator<Item = Rect>, p: Point) -> Option<Rect> {
    rects.min_by(|a, b| {
        dist2(clamp_into(*a, p, 1.0), p).total_cmp(&dist2(clamp_into(*b, p, 1.0), p))
    })
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
