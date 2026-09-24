//! The menu bar / notification area icon.

use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream, StreamExt};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Open,
    ToggleSharing,
    Quit,
}

type Channel = (
    mpsc::UnboundedSender<Event>,
    Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
);

fn channel() -> &'static Channel {
    static CHANNEL: OnceLock<Channel> = OnceLock::new();
    CHANNEL.get_or_init(|| {
        let (tx, rx) = mpsc::unbounded();
        (tx, Mutex::new(Some(rx)))
    })
}

pub struct Tray {
    _icon: TrayIcon,
    sharing: CheckMenuItem,
}

impl Tray {
    /// Creates the icon. On macOS this must run on the main thread once the app is running.
    pub fn new(sharing: bool) -> Result<Self> {
        let open = MenuItem::with_id("open", "Open Legato", true, None);
        let sharing_item = CheckMenuItem::with_id("sharing", "Sharing", true, sharing, None);
        let quit = MenuItem::with_id("quit", "Quit Legato", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &open,
            &PredefinedMenuItem::separator(),
            &sharing_item,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;
        let (rgba, w, h) = crate::icons::tray_rgba();
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Legato")
            .with_icon(Icon::from_rgba(rgba, w, h)?)
            .with_icon_as_template(true)
            .build()?;
        let tx = channel().0.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let event = match event.id.0.as_str() {
                "open" => Event::Open,
                "sharing" => Event::ToggleSharing,
                "quit" => Event::Quit,
                _ => return,
            };
            let _ = tx.unbounded_send(event);
        }));
        Ok(Self {
            _icon: icon,
            sharing: sharing_item,
        })
    }

    pub fn set_sharing(&self, on: bool) {
        self.sharing.set_checked(on);
    }
}

/// Tray events, for a subscription. Only the first subscriber gets them.
pub fn events() -> impl Stream<Item = Event> {
    iced::stream::channel(8, async |mut out| {
        let Some(mut rx) = channel().1.lock().unwrap().take() else {
            return;
        };
        while let Some(event) = rx.next().await {
            if out.send(event).await.is_err() {
                return;
            }
        }
    })
}
