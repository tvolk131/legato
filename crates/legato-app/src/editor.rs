//! The arrangement editor: this machine's displays, and paired machines you drag into
//! place around them.

use iced::alignment;
use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::widget::text;
use iced::{Color, Point, Rectangle, Renderer, Size, Vector};
use iced_m3::Theme;
use legato_net::EndpointId;
use legato_proto::Rect;

use crate::Message;

/// A paired machine's displays, where they currently sit (desk units).
#[derive(Debug, Clone, PartialEq)]
pub struct PeerBox {
    pub id: EndpointId,
    pub name: String,
    pub displays: Vec<Rect>,
    /// Whether it has a saved position yet.
    pub placed: bool,
}

impl PeerBox {
    pub fn bounds(&self) -> Option<Rect> {
        bounds(self.displays.iter().copied())
    }
}

pub fn bounds(rects: impl IntoIterator<Item = Rect>) -> Option<Rect> {
    let mut it = rects.into_iter();
    let first = it.next()?;
    let (mut l, mut t, mut r, mut b) = (first.left(), first.top(), first.right(), first.bottom());
    for rect in it {
        l = l.min(rect.left());
        t = t.min(rect.top());
        r = r.max(rect.right());
        b = b.max(rect.bottom());
    }
    Some(Rect::new(l, t, r - l, b - t))
}

pub struct Editor {
    /// This machine's displays in desk units, left to right.
    pub local: Vec<Rect>,
    pub primary: Option<usize>,
    pub peers: Vec<PeerBox>,
}

#[derive(Debug, Default)]
pub struct Drag {
    /// Peer index, grab offset from its top-left (desk units), and current top-left.
    active: Option<(usize, legato_proto::Point, legato_proto::Point)>,
}

/// Maps desk units onto the canvas, fitting everything with a margin.
struct Viewport {
    scale: f32,
    origin: Vector,
}

impl Viewport {
    fn fit(editor: &Editor, size: Size) -> Self {
        let all = editor
            .local
            .iter()
            .copied()
            .chain(editor.peers.iter().flat_map(|p| p.displays.iter().copied()));
        let b = bounds(all).unwrap_or(Rect::new(0.0, 0.0, 1.0, 1.0));
        // Leave room around the edges to drag machines into.
        let margin = 0.25 * b.width.max(b.height);
        let (w, h) = (b.width + 2.0 * margin, b.height + 2.0 * margin);
        let scale = (size.width as f64 / w).min(size.height as f64 / h) as f32;
        let cx = size.width / 2.0 - ((b.x + b.width / 2.0) as f32) * scale;
        let cy = size.height / 2.0 - ((b.y + b.height / 2.0) as f32) * scale;
        Self {
            scale,
            origin: Vector::new(cx, cy),
        }
    }

    fn to_canvas(&self, r: Rect) -> (Point, Size) {
        (
            Point::new(
                r.x as f32 * self.scale + self.origin.x,
                r.y as f32 * self.scale + self.origin.y,
            ),
            Size::new(r.width as f32 * self.scale, r.height as f32 * self.scale),
        )
    }

    fn to_desk(&self, p: Point) -> legato_proto::Point {
        legato_proto::Point::new(
            ((p.x - self.origin.x) / self.scale) as f64,
            ((p.y - self.origin.y) / self.scale) as f64,
        )
    }
}

impl Editor {
    fn peer_at(&self, desk: legato_proto::Point) -> Option<usize> {
        self.peers
            .iter()
            .position(|p| p.displays.iter().any(|r| r.contains(desk)))
    }

    /// The peer's displays, moved to `top_left` if it's being dragged.
    fn displays(&self, index: usize, drag: &Drag) -> Vec<Rect> {
        let peer = &self.peers[index];
        match (drag.active, peer.bounds()) {
            (Some((i, _, top_left)), Some(b)) if i == index => {
                let (dx, dy) = (top_left.x - b.x, top_left.y - b.y);
                peer.displays
                    .iter()
                    .map(|r| Rect::new(r.x + dx, r.y + dy, r.width, r.height))
                    .collect()
            }
            _ => peer.displays.clone(),
        }
    }
}

impl canvas::Program<Message, Theme> for Editor {
    type State = Drag;

    fn update(
        &self,
        state: &mut Drag,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let view = Viewport::fit(self, bounds.size());
        let iced::Event::Mouse(event) = event else {
            return None;
        };
        match event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let desk = view.to_desk(cursor.position_in(bounds)?);
                let index = self.peer_at(desk)?;
                let b = self.peers[index].bounds()?;
                let grab = legato_proto::Point::new(desk.x - b.x, desk.y - b.y);
                state.active = Some((index, grab, legato_proto::Point::new(b.x, b.y)));
                Some(canvas::Action::capture())
            }
            mouse::Event::CursorMoved { .. } => {
                let (index, grab, _) = state.active?;
                let desk = view.to_desk(cursor.position_in(bounds).or(cursor.position())?);
                let top_left = legato_proto::Point::new(desk.x - grab.x, desk.y - grab.y);
                state.active = Some((index, grab, top_left));
                Some(canvas::Action::request_redraw().and_capture())
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                let (index, _, top_left) = state.active.take()?;
                Some(canvas::Action::publish(Message::DropPeer(
                    self.peers[index].id,
                    top_left,
                )))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Drag,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let view = Viewport::fit(self, bounds.size());
        let c = &theme.colors;
        let radius = (8.0 * view.scale * 20.0).clamp(4.0, 12.0);

        let draw_box = |frame: &mut Frame,
                        r: Rect,
                        fill: Color,
                        stroke: Color,
                        label: String,
                        sub: Option<&str>| {
            let (p, s) = view.to_canvas(r);
            let path = Path::rounded_rectangle(p, s, radius.into());
            frame.fill(&path, fill);
            frame.stroke(&path, Stroke::default().with_color(stroke).with_width(1.5));
            let center = Point::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
            let size = (s.height * 0.22).clamp(11.0, 22.0);
            frame.fill_text(Text {
                content: label,
                position: sub.map_or(center, |_| Point::new(center.x, center.y - size * 0.45)),
                max_width: s.width - 8.0,
                color: c.on_surface,
                size: size.into(),
                font: iced_m3::fonts::MEDIUM,
                align_x: text::Alignment::Center,
                align_y: alignment::Vertical::Center,
                ..Text::default()
            });
            if let Some(sub) = sub {
                frame.fill_text(Text {
                    content: sub.to_string(),
                    position: Point::new(center.x, center.y + size * 0.6),
                    max_width: s.width - 8.0,
                    color: c.on_surface_variant,
                    size: (size * 0.7).max(10.0).into(),
                    font: iced_m3::fonts::REGULAR,
                    align_x: text::Alignment::Center,
                    align_y: alignment::Vertical::Center,
                    ..Text::default()
                });
            }
        };

        for (i, r) in self.local.iter().enumerate() {
            let sub = (self.primary == Some(i)).then_some("main display");
            draw_box(
                &mut frame,
                *r,
                c.surface_container_high,
                c.outline,
                format!("{}", i + 1),
                sub,
            );
        }
        for (i, peer) in self.peers.iter().enumerate() {
            let dragging = state.active.is_some_and(|(d, _, _)| d == i);
            let fill = if dragging {
                c.primary_container
            } else {
                c.tertiary_container
            };
            let sub = if peer.placed {
                None
            } else {
                Some("drag next to a display")
            };
            for r in self.displays(i, state) {
                draw_box(&mut frame, r, fill, c.primary, peer.name.clone(), sub);
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Drag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.active.is_some() {
            return mouse::Interaction::Grabbing;
        }
        let view = Viewport::fit(self, bounds.size());
        match cursor.position_in(bounds) {
            Some(p) if self.peer_at(view.to_desk(p)).is_some() => mouse::Interaction::Grab,
            _ => mouse::Interaction::default(),
        }
    }
}
