//! The Legato app: a tray icon plus a window for pairing, arranging and settings.

// No console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use iced::futures::{SinkExt, Stream};
use iced::{Size, Subscription, Task, window};
use iced_m3::{Element, Theme};
use legato_engine::config::Remap;
use legato_engine::{Engine, Status};
use legato_net::{EndpointId, NearbyEvent, PairAttempt, PairOutcome};
use tokio::sync::broadcast::error::RecvError;

mod editor;
mod icons;
mod model;
mod platform;
mod snap;
mod tray;
mod view;

use model::{Device, Model, Page, Paired, Pairing, PairingStage};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Networking and input run on their own runtime, independent of the UI's executor.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("legato-engine")
            .build()
            .expect("starting the engine runtime")
    })
}

/// Runs `f` on the engine runtime and resolves to its output.
fn on_runtime<T: Send + 'static>(
    f: impl Future<Output = T> + Send + 'static,
) -> impl Future<Output = T> {
    let handle = runtime().spawn(f);
    async move { handle.await.expect("engine task panicked") }
}

/// A pairing attempt handed between the engine and the UI.
#[derive(Debug, Clone)]
pub struct Attempt(Arc<Mutex<Option<PairAttempt>>>);

#[derive(Debug, Clone)]
pub enum Message {
    Init,
    OpenWindow,
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    Tray(tray::Event),
    Quit,
    Page(Page),
    ToggleSharing(bool),
    Sharing(Result<(), String>),
    Status(Status),
    Nearby(NearbyEvent),
    Pair(EndpointId),
    PairStarted(Result<Attempt, String>),
    IncomingPair(Attempt),
    ConfirmPair(bool),
    PairFinished(Result<PairOutcome, String>),
    Unpair(EndpointId),
    DropPeer(EndpointId, legato_proto::Point),
    PushDistance(f32),
    SaveSettings,
    Remap(bool),
    InvertWheel(bool),
    Autostart(bool),
    ControlMode(Option<String>),
    Clipboard(bool),
    SendFiles(EndpointId),
    FilesPicked(EndpointId, Vec<std::path::PathBuf>),
    FileDropped(std::path::PathBuf),
    FlushDrops,
    DismissNotice,
}

struct App {
    engine: Arc<Engine>,
    model: Model,
    window: Option<window::Id>,
    tray: Option<tray::Tray>,
    attempt: Option<Attempt>,
    next_notice: u64,
    /// Files dropped on the window, gathered into one send.
    dropped: Vec<std::path::PathBuf>,
}

/// Hashes by identity, so subscriptions can be keyed on the engine.
#[derive(Clone)]
struct EngineRef(Arc<Engine>);

impl std::hash::Hash for EngineRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

impl App {
    fn new(engine: Arc<Engine>) -> Self {
        let net = engine.net();
        let model = Model {
            page: Page::Devices,
            this: Device {
                id: net.id(),
                name: net.config().name.clone(),
                os: Some(legato_engine::this_os()),
            },
            version: VERSION.into(),
            state_dir: engine.dir().display().to_string(),
            sharing: false,
            paired: Vec::new(),
            nearby: Vec::new(),
            pairing: None,
            pairing_open: false,
            config: engine.config(),
            local: legato_engine::local_screens(),
            known: engine.known_screens(),
            placed_us: HashMap::new(),
            active: None,
            problems: Vec::new(),
            notice: None,
            autostart: platform::autostart_enabled(),
        };
        let mut app = Self {
            engine,
            model,
            window: None,
            tray: None,
            attempt: None,
            next_notice: 0,
            dropped: Vec::new(),
        };
        app.refresh_paired();
        app
    }

    fn boot(engine: Arc<Engine>) -> (Self, Task<Message>) {
        let app = Self::new(engine);
        let open = if std::env::args().any(|a| a == "--hidden") {
            Task::none()
        } else {
            Task::done(Message::OpenWindow)
        };
        (app, Task::batch([Task::done(Message::Init), open]))
    }

    fn refresh_paired(&mut self) {
        let connections: HashMap<_, _> = self
            .model
            .paired
            .iter()
            .filter_map(|p| p.connection.map(|c| (p.device.id, c)))
            .collect();
        self.model.paired = self
            .engine
            .net()
            .paired_peers()
            .into_iter()
            .map(|p| Paired {
                connection: connections.get(&p.id).copied(),
                device: Device {
                    id: p.id,
                    name: p.name,
                    os: Some(p.os),
                },
            })
            .collect();
    }

    fn notify(&mut self, text: impl Into<String>) {
        self.next_notice += 1;
        self.model.notice = Some((self.next_notice, text.into()));
    }

    fn start_sharing(&self) -> Task<Message> {
        let engine = self.engine.clone();
        Task::perform(
            on_runtime(async move { engine.start_sharing().map_err(|e| format!("{e:#}")) }),
            Message::Sharing,
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Init => {
                match tray::Tray::new(self.model.sharing) {
                    Ok(tray) => self.tray = Some(tray),
                    Err(e) => tracing::warn!("no tray icon: {e:#}"),
                }
                if !self.model.paired.is_empty() {
                    return self.start_sharing();
                }
            }
            Message::OpenWindow => {
                if let Some(id) = self.window {
                    return window::gain_focus(id);
                }
                let (id, open) = window::open(window::Settings {
                    size: Size::new(920.0, 640.0),
                    min_size: Some(Size::new(640.0, 480.0)),
                    ..Default::default()
                });
                self.window = Some(id);
                platform::show_in_dock(true);
                return open.map(Message::WindowOpened);
            }
            Message::WindowOpened(_) => {}
            Message::WindowClosed(id) => {
                if self.window == Some(id) {
                    self.window = None;
                    platform::show_in_dock(false);
                }
            }
            Message::Tray(event) => match event {
                tray::Event::Open => return self.update(Message::OpenWindow),
                tray::Event::ToggleSharing => {
                    return self.update(Message::ToggleSharing(!self.model.sharing));
                }
                tray::Event::Quit => return self.update(Message::Quit),
            },
            Message::Quit => {
                let engine = self.engine.clone();
                return Task::perform(
                    on_runtime(async move {
                        engine.stop_sharing().await;
                        engine.net().clone().shutdown().await;
                    }),
                    |()| (),
                )
                .then(|()| iced::exit());
            }
            Message::Page(page) => self.model.page = page,
            Message::ToggleSharing(on) => {
                if on {
                    return self.start_sharing();
                }
                let engine = self.engine.clone();
                return Task::perform(
                    on_runtime(async move {
                        engine.stop_sharing().await;
                        Ok(())
                    }),
                    Message::Sharing,
                );
            }
            Message::Sharing(Err(e)) => self.notify(format!("Couldn't start sharing: {e}")),
            Message::Sharing(Ok(())) => {}
            Message::Status(status) => self.status(status),
            Message::Nearby(event) => match event {
                NearbyEvent::Found(d) => {
                    self.model.nearby.retain(|n| n.id != d.id);
                    self.model.nearby.push(Device {
                        id: d.id,
                        name: d.name,
                        os: d.os,
                    });
                    self.model.nearby.sort_by(|a, b| a.name.cmp(&b.name));
                }
                NearbyEvent::Lost(id) => self.model.nearby.retain(|n| n.id != id),
            },
            Message::Pair(id) => {
                let name = self.model.name_of(&id);
                self.model.pairing = Some(Pairing {
                    name,
                    code: None,
                    incoming: false,
                    stage: PairingStage::Connecting,
                });
                self.model.pairing_open = true;
                let engine = self.engine.clone();
                return Task::perform(
                    on_runtime(async move {
                        engine
                            .net()
                            .pair(id)
                            .await
                            .map(|a| Attempt(Arc::new(Mutex::new(Some(a)))))
                            .map_err(|e| format!("{e:#}"))
                    }),
                    Message::PairStarted,
                );
            }
            Message::PairStarted(Ok(attempt)) | Message::IncomingPair(attempt) => {
                if self.attempt.is_some() {
                    // Busy with another pairing: refuse this one.
                    return decide(attempt, false).discard();
                }
                let guard = attempt.0.lock().unwrap();
                let Some(a) = guard.as_ref() else {
                    return Task::none();
                };
                self.model.pairing = Some(Pairing {
                    name: a.peer.name.clone(),
                    code: Some(a.code),
                    incoming: a.incoming,
                    stage: PairingStage::Confirm,
                });
                drop(guard);
                self.model.pairing_open = true;
                self.attempt = Some(attempt);
                if self.window.is_none() {
                    return self.update(Message::OpenWindow);
                }
            }
            Message::PairStarted(Err(e)) => {
                self.model.pairing_open = false;
                self.notify(format!("Couldn't pair: {e}"));
            }
            Message::ConfirmPair(accept) => {
                let Some(attempt) = self.attempt.take() else {
                    self.model.pairing_open = false;
                    return Task::none();
                };
                if accept {
                    if let Some(p) = &mut self.model.pairing {
                        p.stage = PairingStage::Waiting;
                    }
                } else {
                    self.model.pairing_open = false;
                }
                return decide(attempt, accept);
            }
            Message::PairFinished(result) => {
                self.model.pairing_open = false;
                let name = self
                    .model
                    .pairing
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                match result {
                    Ok(PairOutcome::Paired(peer)) => {
                        self.refresh_paired();
                        self.notify(format!(
                            "Paired with \"{}\". Arrange it on the Arrangement page.",
                            peer.name
                        ));
                        if !self.model.sharing {
                            return self.start_sharing();
                        }
                    }
                    Ok(PairOutcome::DeclinedThere) => self.notify(format!("\"{name}\" declined.")),
                    Ok(PairOutcome::DeclinedHere) => {}
                    Err(e) => self.notify(format!("Pairing failed: {e}")),
                }
            }
            Message::Unpair(id) => {
                let engine = self.engine.clone();
                let result = engine.net().unpair(&id);
                let _ = self.engine.update_config(|c| {
                    let text = id.to_string();
                    c.neighbors.retain(|n| !text.starts_with(&n.peer));
                });
                self.model.config = self.engine.config();
                self.refresh_paired();
                if let Err(e) = result {
                    self.notify(format!("Couldn't unpair: {e:#}"));
                }
            }
            Message::DropPeer(id, top_left) => self.drop_peer(id, top_left),
            Message::PushDistance(v) => self.model.config.switching.push_distance = v.into(),
            Message::SaveSettings => self.save_config(),
            Message::Remap(on) => {
                self.model.config.keys.remap = if on { Remap::Auto } else { Remap::None };
                self.save_config();
            }
            Message::InvertWheel(on) => {
                self.model.config.scrolling.invert_wheel = on;
                self.save_config();
            }
            Message::Autostart(on) => match platform::set_autostart(on) {
                Ok(()) => self.model.autostart = Some(on),
                Err(e) => self.notify(format!("Couldn't change login item: {e:#}")),
            },
            Message::ControlMode(controller) => {
                self.model.config.control.controller = controller;
                self.model.config.control.updated_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                self.save_config();
            }
            Message::Clipboard(on) => {
                self.model.config.clipboard.enabled = on;
                self.save_config();
            }
            Message::SendFiles(id) => {
                let name = self.model.name_of(&id);
                return Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_title(format!("Send to \"{name}\""))
                            .pick_files()
                            .await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|f| f.path().to_path_buf())
                            .collect::<Vec<_>>()
                    },
                    move |paths| Message::FilesPicked(id, paths),
                );
            }
            Message::FilesPicked(id, paths) => {
                if !paths.is_empty()
                    && let Err(e) = self.engine.send_files(id, paths)
                {
                    self.notify(format!("Couldn't send: {e:#}"));
                }
            }
            Message::FileDropped(path) => {
                self.dropped.push(path);
                // Files dropped together arrive one event at a time; gather them first.
                return Task::perform(
                    on_runtime(tokio::time::sleep(std::time::Duration::from_millis(200))),
                    |()| Message::FlushDrops,
                );
            }
            Message::FlushDrops => {
                if self.dropped.is_empty() {
                    return Task::none();
                }
                let paths = std::mem::take(&mut self.dropped);
                let target = self
                    .model
                    .active
                    .filter(|id| {
                        self.model
                            .paired(id)
                            .is_some_and(|p| p.connection.is_some())
                    })
                    .or_else(|| self.model.connected().next().map(|p| p.device.id));
                match target {
                    Some(id) => {
                        if let Err(e) = self.engine.send_files(id, paths) {
                            self.notify(format!("Couldn't send: {e:#}"));
                        }
                    }
                    None => self.notify("Connect a device first to send it files."),
                }
            }
            Message::DismissNotice => self.model.notice = None,
        }
        Task::none()
    }

    fn save_config(&mut self) {
        let config = self.model.config.clone();
        if let Err(e) = self.engine.update_config(|c| *c = config) {
            self.notify(format!("Couldn't save settings: {e:#}"));
        }
    }

    fn drop_peer(&mut self, id: EndpointId, top_left: legato_proto::Point) {
        let editor = view::editor(&self.model);
        let Some(peer) = editor.peers.iter().find(|p| p.id == id) else {
            return;
        };
        let Some(b) = peer.bounds() else { return };
        let dropped = legato_proto::Rect::new(top_left.x, top_left.y, b.width, b.height);
        let Some(snapped) = snap::snap(&editor.local, dropped) else {
            return;
        };
        let text = id.to_string();
        self.model
            .config
            .neighbors
            .retain(|n| !text.starts_with(&n.peer));
        self.model
            .config
            .neighbors
            .push(legato_engine::config::Neighbor {
                peer: text,
                side: snapped.side,
                display: snapped.display + 1,
                align: legato_core::Align::Center,
                nudge: 0.0,
                offset: Some([snapped.top_left.x, snapped.top_left.y]),
            });
        self.model.problems.clear();
        self.save_config();
    }

    fn status(&mut self, status: Status) {
        match status {
            Status::Sharing(on) => {
                self.model.sharing = on;
                if !on {
                    for p in &mut self.model.paired {
                        p.connection = None;
                    }
                    self.model.active = None;
                }
                if let Some(tray) = &self.tray {
                    tray.set_sharing(on);
                }
            }
            Status::PeerConnected { id, path, .. } => {
                if let Some(p) = self.model.paired.iter_mut().find(|p| p.device.id == id) {
                    p.connection = Some(path);
                } else {
                    self.refresh_paired();
                }
            }
            Status::PeerDisconnected { id, .. } => {
                if let Some(p) = self.model.paired.iter_mut().find(|p| p.device.id == id) {
                    p.connection = None;
                }
                if self.model.active == Some(id) {
                    self.model.active = None;
                }
            }
            Status::PeerScreens { id, screens } => {
                self.model.known.insert(id, screens);
            }
            Status::Controlling(id) | Status::ControlledBy(id) => self.model.active = id,
            Status::Problem(p) => {
                if !self.model.problems.contains(&p) {
                    self.model.problems.push(p);
                }
            }
            Status::Error(e) => self.notify(format!("Sharing stopped: {e}")),
            Status::PeerPlacedUs { id, offset } => match offset {
                Some(o) => {
                    self.model.placed_us.insert(id, o);
                }
                None => {
                    self.model.placed_us.remove(&id);
                }
            },
            Status::FilesSending { to, count } => {
                let name = self.model.name_of(&to);
                self.notify(format!("Sending {count} item(s) to \"{name}\"…"));
            }
            Status::FilesSent { to, bytes } => {
                let name = self.model.name_of(&to);
                self.notify(format!(
                    "Sent {} to \"{name}\".",
                    legato_engine::human_bytes(bytes)
                ));
            }
            Status::FilesReceived(received) => {
                let name = self.model.name_of(&received.from);
                match received.purpose {
                    legato_proto::FilePurpose::Clipboard => {
                        self.notify(format!("Files copied on \"{name}\" are ready to paste."));
                    }
                    _ => {
                        self.notify(format!(
                            "Received {} item(s) from \"{name}\" in Downloads/Legato.",
                            received.paths.len()
                        ));
                        if received.purpose == legato_proto::FilePurpose::Drop {
                            platform::reveal(&received.paths);
                        }
                    }
                }
            }
        }
    }

    fn view(&self, _window: window::Id) -> Element<'_, Message> {
        view::root(&self.model)
    }

    fn subscription(&self) -> Subscription<Message> {
        let engine = EngineRef(self.engine.clone());
        Subscription::batch([
            Subscription::run_with(engine.clone(), status_stream),
            Subscription::run_with(engine.clone(), nearby_stream),
            Subscription::run_with(engine, pairing_stream),
            Subscription::run(tray::events).map(Message::Tray),
            window::close_events().map(Message::WindowClosed),
            iced::event::listen_with(|event, _, _| match event {
                iced::Event::Window(window::Event::FileDropped(path)) => {
                    Some(Message::FileDropped(path))
                }
                _ => None,
            }),
        ])
    }
}

fn decide(attempt: Attempt, accept: bool) -> Task<Message> {
    let taken = attempt.0.lock().unwrap().take();
    let Some(attempt) = taken else {
        return Task::none();
    };
    Task::perform(
        on_runtime(async move { attempt.decide(accept).await.map_err(|e| format!("{e:#}")) }),
        Message::PairFinished,
    )
}

fn status_stream(engine: &EngineRef) -> impl Stream<Item = Message> + use<> {
    let engine = engine.0.clone();
    iced::stream::channel(64, async move |mut out| {
        let mut rx = engine.subscribe();
        loop {
            match rx.recv().await {
                Ok(status) => {
                    if out.send(Message::Status(status)).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    })
}

fn nearby_stream(engine: &EngineRef) -> impl Stream<Item = Message> + use<> {
    let engine = engine.0.clone();
    iced::stream::channel(16, async move |mut out| {
        let Ok(mut nearby) = on_runtime(async move { engine.net().nearby().await }).await else {
            return;
        };
        use iced::futures::StreamExt;
        while let Some(event) = nearby.next().await {
            if out.send(Message::Nearby(event)).await.is_err() {
                return;
            }
        }
    })
}

fn pairing_stream(engine: &EngineRef) -> impl Stream<Item = Message> + use<> {
    let engine = engine.0.clone();
    iced::stream::channel(4, async move |mut out| {
        let mut incoming = engine.net().listen_for_pairing();
        while let Some(attempt) = incoming.recv().await {
            let attempt = Attempt(Arc::new(Mutex::new(Some(attempt))));
            if out.send(Message::IncomingPair(attempt)).await.is_err() {
                return;
            }
        }
    })
}

fn theme(_app: &App, _window: window::Id) -> Theme {
    Theme::from_accent(
        iced::Color::from_rgb8(0x3d, 0x5a, 0xfe),
        platform::dark_mode(),
    )
}

fn main() -> iced::Result {
    let filter = tracing_subscriber::EnvFilter::try_from_env("LEGATO_LOG").unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "error,legato=info,legato_app=info,legato_engine=info,legato_net=info",
        )
    });
    let home: Option<std::path::PathBuf> =
        std::env::var_os(legato_net::store::HOME_ENV).map(Into::into);
    // The Windows app has no console, so also log to a file next to the settings.
    let log_file = home
        .clone()
        .or_else(|| legato_net::store::default_dir().ok())
        .and_then(|dir| {
            std::fs::create_dir_all(&dir).ok()?;
            std::fs::File::create(dir.join("legato-app.log")).ok()
        });
    match log_file {
        Some(file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    platform::init();

    let engine = match runtime().block_on(Engine::start(home, VERSION)) {
        Ok(engine) => engine,
        Err(e) => {
            tracing::error!("couldn't start Legato: {e:#}");
            std::process::exit(1);
        }
    };

    iced::daemon(move || App::boot(engine.clone()), App::update, App::view)
        .title(|_: &App, _| "Legato".to_string())
        .subscription(App::subscription)
        .theme(theme)
        .default_font(iced_m3::fonts::REGULAR)
        .run()
}

#[cfg(test)]
mod tests;
