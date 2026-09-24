//! UI tests: the real views, rendered headlessly, driven like a user would.
//!
//! Each screen is also compared, pixel for pixel, with a golden image in `src/snapshots/`.
//! After a deliberate change, rewrite them with `LEGATO_UPDATE_SNAPSHOTS=1 cargo test -p
//! legato-app` and review the new images in the diff. On a mismatch, the new rendering and
//! a diff (changed pixels in red) are saved to `target/snapshots/`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use iced_m3::Theme;
use iced_test::simulator;
use legato_engine::config::{Config, Neighbor};
use legato_net::PathKind;
use legato_proto::{Display, Os, Rect, Screens};

use crate::Message;
use crate::model::{Device, Model, Page, Paired, Pairing, PairingStage};

/// A fixed device id, so ids shown on screen are the same in every run.
fn id(n: u8) -> legato_net::EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
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
        id: id(2),
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
            id: id(3),
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
            id: id(4),
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
        viewing: None,
        active: None,
        problems: vec![],
        notice: None,
        autostart: Some(false),
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_png(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?));
    let mut reader = decoder.read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut rgba).ok()?;
    rgba.truncate(info.buffer_size());
    Some((info.width, info.height, rgba))
}

fn write_png(path: &Path, (width, height, rgba): (u32, u32, &[u8])) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(rgba).unwrap();
    writer.finish().unwrap();
}

/// Compares the screen with its golden image (see the module docs).
fn snapshot(ui: &mut iced_test::Simulator<'_, Message, Theme>, name: &str) {
    let theme = Theme::from_accent(iced::Color::from_rgb8(0x3d, 0x5a, 0xfe), false);
    // iced_test only hands the pixels over as a PNG it writes itself (named after the
    // renderer), so render into a scratch folder and read that back.
    let scratch = std::env::temp_dir().join(format!("legato-ui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    ui.snapshot(&theme)
        .unwrap()
        .matches_image(scratch.join(name))
        .unwrap();
    let rendered = scratch.join(format!("{name}-tiny-skia.png"));
    assert!(
        rendered.exists(),
        "UI snapshots are rendered with tiny-skia; set ICED_TEST_BACKEND=tiny-skia"
    );
    let (width, height, actual) = read_png(&rendered).unwrap();
    let _ = std::fs::remove_dir_all(&scratch);

    let golden_path = manifest_dir()
        .join("src/snapshots")
        .join(format!("{name}.png"));
    if std::env::var_os("LEGATO_UPDATE_SNAPSHOTS").is_some() {
        write_png(&golden_path, (width, height, &actual));
        return;
    }
    let Some((golden_w, golden_h, golden)) = read_png(&golden_path) else {
        panic!(
            "no golden image for \"{name}\"; create it with LEGATO_UPDATE_SNAPSHOTS=1 cargo test -p legato-app"
        );
    };
    let out = manifest_dir().join("../../target/snapshots");
    let differing = if (golden_w, golden_h) == (width, height) {
        golden
            .as_chunks::<4>()
            .0
            .iter()
            .zip(actual.as_chunks::<4>().0.iter())
            .filter(|(g, a)| g != a)
            .count()
    } else {
        usize::MAX
    };
    if differing == 0 {
        return;
    }
    write_png(&out.join(format!("{name}.png")), (width, height, &actual));
    if differing != usize::MAX {
        // Changed pixels in red over a faded copy of the golden image.
        let diff: Vec<u8> = golden
            .as_chunks::<4>()
            .0
            .iter()
            .zip(actual.as_chunks::<4>().0.iter())
            .flat_map(|(g, a)| {
                if g == a {
                    let sum = u16::from(g[0]) + u16::from(g[1]) + u16::from(g[2]);
                    let grey = 200 + (sum / 14) as u8;
                    [grey, grey, grey, 255]
                } else {
                    [255, 0, 0, 255]
                }
            })
            .collect();
        write_png(
            &out.join(format!("{name}-diff.png")),
            (width, height, &diff),
        );
    }
    panic!(
        "\"{name}\" looks different from src/snapshots/{name}.png ({}); see target/snapshots/. \
         If the change is intended, run LEGATO_UPDATE_SNAPSHOTS=1 cargo test -p legato-app",
        if differing == usize::MAX {
            format!("{width}×{height}, was {golden_w}×{golden_h}")
        } else {
            format!("{differing} pixels differ")
        }
    );
}

#[test]
fn devices_page_lists_paired_and_nearby_devices() {
    let m = model(Page::Devices);
    let jane = m.nearby[0].id;
    let mut ui = simulator(crate::view::root(&m));
    snapshot(&mut ui, "devices");
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
    snapshot(&mut ui, "pairing");
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
    snapshot(&mut ui, "arrangement");
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
    snapshot(&mut ui, "settings");
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

#[test]
fn a_connected_mac_can_be_shown_as_a_display_on_windows() {
    let mut m = model(Page::Devices);
    let mac = m.paired[0].device.id;
    let mut ui = simulator(crate::view::root(&m));
    ui.click("Show as display").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::ShowDisplay(id) if *id == mac)),
        "{messages:?}"
    );

    m.viewing = Some(mac);
    let mut ui = simulator(crate::view::root(&m));
    ui.click("Stop display").unwrap();
    assert!(
        ui.into_messages()
            .any(|m| matches!(m, Message::StopDisplay))
    );

    // A Mac can't show another machine's display.
    m.this.os = Some(Os::MacOs);
    m.viewing = None;
    let mut ui = simulator(crate::view::root(&m));
    assert!(ui.find("Show as display").is_err());
}

#[test]
fn the_viewer_waits_for_the_first_picture() {
    let mut ui = simulator(crate::view::viewer("Tommy's MacBook Pro", None));
    snapshot(&mut ui, "viewer-waiting");
    assert!(
        ui.find("Waiting for \"Tommy's MacBook Pro\" to add its display…")
            .is_ok()
    );
}
