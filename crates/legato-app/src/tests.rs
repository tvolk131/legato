//! UI tests: the real views, rendered headlessly, driven like a user would.
//!
//! Set `LEGATO_SNAPSHOT_DIR` to also save a PNG of each screen for review.

use std::collections::HashMap;
use std::time::Duration;

use iced_m3::Theme;
use iced_test::simulator;
use legato_engine::config::{Config, Neighbor};
use legato_net::PathKind;
use legato_proto::{Display, Os, Rect, Screens};

use crate::Message;
use crate::model::{Device, Model, Page, Paired, Pairing, PairingStage};

fn id() -> legato_net::EndpointId {
    iroh::SecretKey::generate().public()
}

fn display(x: f64, w: f64, h: f64, primary: bool) -> Display {
    Display {
        id: 0,
        bounds: Rect::new(x, 0.0, w, h),
        pixel_scale: 1.0,
        ui_scale: 1.5,
        primary,
        name: String::new(),
    }
}

/// The user's setup: a triple-4K Windows PC with a MacBook paired and placed below.
fn model(page: Page) -> Model {
    let mac = Device {
        id: id(),
        name: "Tommy's MacBook Pro".into(),
        os: Some(Os::MacOs),
    };
    let mut config = Config::default();
    config.neighbors.push(Neighbor {
        peer: mac.id.to_string(),
        side: legato_core::Side::Below,
        display: 2,
        align: legato_core::Align::Center,
        nudge: 0.0,
        offset: None,
    });
    let mut known = HashMap::new();
    known.insert(
        mac.id,
        Screens {
            displays: vec![Display {
                id: 1,
                bounds: Rect::new(0.0, 0.0, 1728.0, 1117.0),
                pixel_scale: 2.0,
                ui_scale: 1.0,
                primary: true,
                name: "Built-in display".into(),
            }],
            native_per_desk: 1.0,
        },
    );
    Model {
        page,
        this: Device {
            id: id(),
            name: "STUDIO-PC".into(),
            os: Some(Os::Windows),
        },
        version: "0.1.0".into(),
        state_dir: r"C:\Users\tommy\AppData\Roaming\tvolk131\Legato\data".into(),
        sharing: true,
        paired: vec![Paired {
            device: mac,
            connection: Some(Some((PathKind::Direct, Duration::from_millis(2)))),
        }],
        nearby: vec![Device {
            id: id(),
            name: "Jane's Laptop".into(),
            os: Some(Os::Windows),
        }],
        pairing: None,
        pairing_open: false,
        config,
        local: Screens {
            displays: vec![
                display(-3840.0, 3840.0, 2160.0, false),
                display(0.0, 3840.0, 2160.0, true),
                display(3840.0, 3840.0, 2160.0, false),
            ],
            native_per_desk: 1.5,
        },
        known,
        placed_us: HashMap::new(),
        active: None,
        problems: vec![],
        notice: None,
        autostart: Some(false),
    }
}

fn save(ui: &mut iced_test::Simulator<'_, Message, Theme>, name: &str) {
    if let Some(dir) = std::env::var_os("LEGATO_SNAPSHOT_DIR") {
        let theme = Theme::from_accent(iced::Color::from_rgb8(0x3d, 0x5a, 0xfe), false);
        let path = std::path::Path::new(&dir).join(name);
        let _ = std::fs::remove_file(path.with_extension("png"));
        ui.snapshot(&theme).unwrap().matches_image(path).unwrap();
    }
}

#[test]
fn devices_page_lists_paired_and_nearby_devices() {
    let m = model(Page::Devices);
    let jane = m.nearby[0].id;
    let mut ui = simulator(crate::view::root(&m));
    save(&mut ui, "devices");
    assert!(ui.find("Tommy's MacBook Pro").is_ok());
    assert!(ui.find("macOS · Connected · 2 ms").is_ok());
    assert!(ui.find("Jane's Laptop").is_ok());
    ui.click("Pair").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::Pair(id) if *id == jane)),
        "{messages:?}"
    );
}

#[test]
fn pairing_dialog_shows_the_code_and_confirms() {
    let mut m = model(Page::Devices);
    m.pairing = Some(Pairing {
        name: "Jane's Laptop".into(),
        code: Some(42_917),
        incoming: true,
        stage: PairingStage::Confirm,
    });
    m.pairing_open = true;
    let mut ui = simulator(crate::view::root(&m));
    save(&mut ui, "pairing");
    assert!(ui.find("\"Jane's Laptop\" wants to pair").is_ok());
    assert!(ui.find("042 917").is_ok());
    ui.click("Codes match").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::ConfirmPair(true))),
        "{messages:?}"
    );
}

#[test]
fn arrangement_page_draws_the_editor() {
    let m = model(Page::Arrangement);
    let editor = crate::view::editor(&m);
    assert_eq!(editor.local.len(), 3);
    assert_eq!(editor.primary, Some(1));
    // The MacBook sits centred below the middle monitor, in desk units.
    let mac = &editor.peers[0];
    assert!(mac.placed);
    assert_eq!(mac.bounds(), Some(Rect::new(416.0, 1440.0, 1728.0, 1117.0)));
    let mut ui = simulator(crate::view::root(&m));
    save(&mut ui, "arrangement");
    assert!(ui.find("Arrangement").is_ok());
}

#[test]
fn unplaced_peers_are_parked_beside_the_displays() {
    let mut m = model(Page::Arrangement);
    m.config.neighbors.clear();
    let editor = crate::view::editor(&m);
    let mac = &editor.peers[0];
    assert!(!mac.placed);
    assert!(
        mac.bounds().unwrap().x > 2560.0 + 2560.0,
        "{:?}",
        mac.bounds()
    );
}

#[test]
fn settings_toggles_send_messages() {
    let m = model(Page::Settings);
    let mut ui = iced_test::Simulator::with_size(
        iced::Settings::default(),
        (1024.0, 1600.0),
        crate::view::root(&m),
    );
    save(&mut ui, "settings");
    ui.click("Reverse the mouse wheel when this device is being controlled")
        .unwrap();
    ui.click("Open Legato when you log in").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::InvertWheel(true))),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::Autostart(true))),
        "{messages:?}"
    );
}

#[test]
fn control_mode_can_be_limited_to_one_device() {
    let m = model(Page::Settings);
    let mac = m.paired[0].device.id.to_string();
    let mut ui = iced_test::Simulator::with_size(
        iced::Settings::default(),
        (1024.0, 1600.0),
        crate::view::root(&m),
    );
    save(&mut ui, "settings-full");
    ui.click("Only Tommy's MacBook Pro").unwrap();
    ui.click("Share copied text, images and files with paired devices")
        .unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::ControlMode(Some(id)) if *id == mac)),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::Clipboard(false))),
        "{messages:?}"
    );
}

#[test]
fn connected_devices_can_be_sent_files() {
    let m = model(Page::Devices);
    let mac = m.paired[0].device.id;
    let mut ui = simulator(crate::view::root(&m));
    ui.click("Send files…").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::SendFiles(id) if *id == mac)),
        "{messages:?}"
    );
}
