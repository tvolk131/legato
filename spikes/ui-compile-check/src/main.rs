// Compile probe: iced 0.14 daemon + iced-m3 + canvas + Subscription::run/stream::channel
// + window::run (raw handle for drag-rs) + tray-icon + arboard file lists + display-info.
use iced::futures::{SinkExt, Stream};
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path};
use iced::widget::{column, container};
use iced::{Length, Point, Rectangle, Renderer, Size, Subscription, Task, window};
use iced_m3::{Element, Theme, button, switch};

#[derive(Debug, Clone)]
enum Message {
    WindowOpened(window::Id),
    Net(NetEvent),
    Toggle(bool),
    MoveScreen(usize, Point),
    StartDrag,
    Tray,
}

#[derive(Debug, Clone)]
enum NetEvent {
    Connected,
    Peer(String),
}

struct App {
    main: Option<window::Id>,
    enabled: bool,
    screens: Vec<Rectangle>,
    _tray: Option<tray_icon::TrayIcon>,
}

fn net_service() -> impl Stream<Item = NetEvent> {
    iced::stream::channel(100, async |mut out| {
        let _ = out.send(NetEvent::Connected).await;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let _ = out.send(NetEvent::Peer("laptop".into())).await;
        }
    })
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let tray = tray_icon::TrayIconBuilder::new().with_tooltip("kvm").build().ok();
        let (_id, open) = window::open(window::Settings {
            size: Size::new(720.0, 480.0),
            ..Default::default()
        });
        let displays = display_info::DisplayInfo::all().unwrap_or_default();
        let screens = displays
            .iter()
            .map(|d| Rectangle::new(Point::new(d.x as f32, d.y as f32), Size::new(d.width as f32, d.height as f32)))
            .collect();
        (
            Self { main: None, enabled: true, screens, _tray: tray },
            open.map(Message::WindowOpened),
        )
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::WindowOpened(id) => self.main = Some(id),
            Message::Toggle(b) => self.enabled = b,
            Message::MoveScreen(i, p) => {
                if let Some(r) = self.screens.get_mut(i) {
                    r.x = p.x;
                    r.y = p.y;
                }
            }
            Message::StartDrag => {
                if let Some(id) = self.main {
                    return window::run(id, |w| {
                        #[cfg(any(target_os = "macos", target_os = "windows"))]
                        {
                            let _ = drag::start_drag(
                                &w,
                                drag::DragItem::Files(vec!["/tmp/x".into()]),
                                drag::Image::Raw(vec![]),
                                |_r, _p| {},
                                Default::default(),
                            );
                        }
                        let _ = w;
                    })
                    .discard();
                }
            }
            Message::Net(_) | Message::Tray => {
                let mut cb = arboard::Clipboard::new().unwrap();
                let _ = cb.get().file_list();
            }
        }
        Task::none()
    }

    fn view(&self, _id: window::Id) -> Element<'_, Message> {
        let editor: Element<'_, Message> = Canvas::new(Arrangement { screens: &self.screens })
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        iced_m3::focus::scope(
            container(column![
                switch(self.enabled).label("Share input").on_toggle(Message::Toggle),
                button("Drag test").on_press(Message::StartDrag),
                editor,
            ])
            .padding(24),
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run(net_service).map(Message::Net),
            iced::time::every(std::time::Duration::from_millis(100)).map(|_| Message::Tray),
        ])
    }
}

struct Arrangement<'a> {
    screens: &'a [Rectangle],
}

#[derive(Default)]
struct DragState {
    grabbed: Option<(usize, iced::Vector)>,
}

impl canvas::Program<Message, Theme> for Arrangement<'_> {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let pos = cursor.position_in(bounds)?;
        match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let i = self.screens.iter().position(|r| r.contains(pos))?;
                state.grabbed = Some((i, pos - self.screens[i].position()));
                Some(canvas::Action::capture())
            }
            iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let (i, off) = state.grabbed?;
                Some(canvas::Action::publish(Message::MoveScreen(i, pos - off)).and_capture())
            }
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.grabbed = None;
                None
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &DragState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        for r in self.screens {
            frame.fill(&Path::rectangle(r.position(), r.size()), theme.colors.primary);
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, s: &DragState, _b: Rectangle, _c: mouse::Cursor) -> mouse::Interaction {
        if s.grabbed.is_some() { mouse::Interaction::Grabbing } else { mouse::Interaction::Grab }
    }
}

fn main() -> iced::Result {
    iced::daemon(App::boot, App::update, App::view)
        .subscription(App::subscription)
        .theme(|_: &App, _id| Theme::dark())
        .default_font(iced_m3::fonts::REGULAR)
        .run()
}
