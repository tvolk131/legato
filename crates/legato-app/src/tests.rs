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
        rail_expanded: false,
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
        add_device_open: false,
        unpair: None,
        unpair_open: false,
        config,
        push_distance: None,
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
        display_options: None,
        display_options_open: false,
        active: None,
        problems: vec![],
        transfer_failures: vec![],
        notice: None,
        autostart: Some(false),
    }
}

fn sized_simulator<'a>(
    size: (f32, f32),
    element: impl Into<crate::Element<'a, Message>>,
) -> iced_test::Simulator<'a, Message, Theme> {
    iced_test::Simulator::with_size(iced::Settings::default(), size, element.into())
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
    snapshot_theme(ui, name, false);
}

fn snapshot_theme(ui: &mut iced_test::Simulator<'_, Message, Theme>, name: &str, dark: bool) {
    let theme = crate::app_theme(dark);
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
fn devices_page_lists_paired_devices_and_offers_to_add_one() {
    let m = model(Page::Devices);
    let mut ui = simulator(crate::view::root(&m));
    snapshot(&mut ui, "devices");
    assert!(ui.find("Tommy's MacBook Pro").is_ok());
    assert!(ui.find("macOS · Connected · 2 ms").is_ok());
    // Unpaired devices nearby are in the dialog (the badge counts them), so nothing on
    // the page keeps animating.
    assert!(ui.find("Jane's Laptop").is_err());
    assert!(ui.find("Looking for Legato on this network…").is_err());
    ui.click("Add a device…").unwrap();
    assert!(
        ui.into_messages()
            .any(|m| matches!(m, Message::AddDevice(true)))
    );
}

#[test]
fn with_nothing_paired_the_page_points_to_adding_a_device() {
    let mut m = model(Page::Devices);
    m.paired.clear();
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    snapshot(&mut ui, "devices-empty");
    assert!(ui.find("Connect your first device").is_ok());
    assert!(
        ui.find("Open Legato on both devices on the same network, then choose Add a device.")
            .is_ok()
    );
    ui.click("Add a device…").unwrap();
    assert!(
        ui.into_messages()
            .any(|m| matches!(m, Message::AddDevice(true)))
    );
}

#[test]
fn the_add_device_dialog_lists_unpaired_devices_nearby() {
    let mut m = model(Page::Devices);
    m.add_device_open = true;
    let jane = m.nearby[0].id;
    let mut ui = simulator(crate::view::root(&m));
    snapshot(&mut ui, "add-device");
    assert!(ui.find("Jane's Laptop").is_ok());
    ui.click("Pair").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::Pair(id) if *id == jane)),
        "{messages:?}"
    );
    // With none nearby, it says it's looking (the only spinner, while it's open).
    m.nearby.clear();
    let mut ui = simulator(crate::view::root(&m));
    assert!(ui.find("Looking for Legato on this network…").is_ok());
    ui.click("Close").unwrap();
    assert!(
        ui.into_messages()
            .any(|m| matches!(m, Message::AddDevice(false)))
    );
}

#[test]
fn the_navigation_rail_expands_to_show_labels_beside_icons() {
    let mut m = model(Page::Devices);
    m.rail_expanded = true;
    let mut ui = simulator(crate::view::root(&m));
    snapshot(&mut ui, "rail-expanded");
    ui.click("Settings").unwrap();
    assert!(
        ui.into_messages()
            .any(|m| matches!(m, Message::Page(Page::Settings)))
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
    let mut ui = sized_simulator((1024.0, 1600.0), crate::view::root(&m));
    snapshot(&mut ui, "settings");
    ui.click("Reverse mouse wheel").unwrap();
    ui.click("Open at login").unwrap();
    ui.click("Mac Command key").unwrap();
    ui.click("Display performance").unwrap();
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
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, Message::InvertWheel(true)))
            .count(),
        1
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, Message::Remap(false)))
            .count(),
        1
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, Message::Stats(false)))
            .count(),
        1
    );
}

#[test]
fn unpair_is_secondary_and_the_confirmation_can_be_cancelled() {
    let mut m = model(Page::Devices);
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    assert!(ui.find("Unpair…").is_err());
    ui.click("More").unwrap();
    ui.simulate([iced::Event::Window(iced::window::Event::RedrawRequested(
        iced::time::Instant::now() + Duration::from_secs(1),
    ))]);
    snapshot(&mut ui, "device-menu");
    ui.click("Unpair…").unwrap();
    assert!(
        ui.into_messages()
            .any(|msg| matches!(msg, Message::Unpair(id) if id == m.paired[0].device.id))
    );
    m.unpair = Some(m.paired[0].device.clone());
    m.unpair_open = true;
    for (dark, accept) in [(false, false), (true, true)] {
        let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
        snapshot_theme(&mut ui, if dark { "unpair-dark" } else { "unpair" }, dark);
        assert!(ui.find("Unpair device?").is_ok());
        let target = ui.click(if accept { "Unpair" } else { "Cancel" }).unwrap();
        assert_eq!(target.visible_bounds(), Some(target.bounds()));
        assert!(
            ui.into_messages()
                .any(|msg| matches!(msg, Message::ConfirmUnpair(value) if value == accept))
        );
    }
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    ui.tap_key(iced::keyboard::key::Named::Escape);
    assert!(
        ui.into_messages()
            .any(|msg| matches!(msg, Message::ConfirmUnpair(false)))
    );
}

#[test]
fn settings_rows_fit_compact_width_and_disabled_login_cannot_be_toggled() {
    for dark in [false, true] {
        let mut m = model(Page::Settings);
        m.this.os = Some(Os::MacOs);
        m.autostart = None;
        let mut ui = sized_simulator((640.0, 1000.0), crate::view::root(&m));
        snapshot_theme(
            &mut ui,
            if dark {
                "settings-compact-dark"
            } else {
                "settings-compact"
            },
            dark,
        );
        for label in [
            "Mac Command key",
            "Reverse mouse wheel",
            "Share clipboard",
            "Open at login",
        ] {
            let target = ui.find(label).unwrap();
            assert_eq!(target.visible_bounds(), Some(target.bounds()));
            assert!(target.bounds().height <= 24.0);
        }
        assert!(ui.find("Display performance").is_err());
        assert!(
            ui.find("Opening at login is unavailable on this system.")
                .is_ok()
        );
        ui.click("Open at login").unwrap();
        assert!(
            !ui.into_messages()
                .any(|msg| matches!(msg, Message::Autostart(_)))
        );
    }
}

#[test]
fn discovery_and_offline_states_explain_how_to_connect() {
    let mut m = model(Page::Devices);
    m.nearby.clear();
    m.add_device_open = true;
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    snapshot_theme(&mut ui, "discovery-empty-dark", true);
    assert!(
        ui.find("Device not appearing?")
            .unwrap()
            .visible_bounds()
            .is_some()
    );
    assert!(
        ui.find("After pairing, your devices can connect across different networks.")
            .is_ok()
    );
    let target = ui.click("Close").unwrap();
    assert_eq!(target.visible_bounds(), Some(target.bounds()));
    assert!(
        ui.into_messages()
            .any(|msg| matches!(msg, Message::AddDevice(false)))
    );
    m.add_device_open = false;
    m.paired[0].connection = None;
    for sharing in [false, true] {
        m.sharing = sharing;
        let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
        snapshot_theme(
            &mut ui,
            if sharing {
                "devices-offline"
            } else {
                "devices-paused-dark"
            },
            !sharing,
        );
        let status = if sharing {
            "macOS · Not connected · Open Legato there and check that both devices are online."
        } else {
            "macOS · Sharing is off · Turn on Sharing to reconnect."
        };
        let target = ui.find(status).unwrap();
        assert_eq!(target.visible_bounds(), Some(target.bounds()));
        assert!(ui.find("Show as display").is_err());
        ui.click("Send files…").unwrap();
        assert!(
            !ui.into_messages()
                .any(|msg| matches!(msg, Message::SendFiles(_)))
        );
    }
}

#[test]
fn control_mode_can_be_limited_to_one_device() {
    let m = model(Page::Settings);
    let mac = m.paired[0].device.id.to_string();
    let mut ui = sized_simulator((1024.0, 1600.0), crate::view::root(&m));
    ui.click("Any device").unwrap();
    ui.click("Only Tommy's MacBook Pro").unwrap();
    ui.click("Share clipboard").unwrap();
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
fn compact_device_rows_keep_identity_and_actions_readable() {
    for (name, width, expanded, device_name) in [
        ("devices-compact", 640.0, false, "Tommy's MacBook Pro"),
        (
            "devices-compact-long-name",
            640.0,
            true,
            "Tommy's MacBook Pro — Personal Development and Design",
        ),
        (
            "devices-expanded-long-name",
            920.0,
            true,
            "Tommy's MacBook Pro — Personal Development and Design",
        ),
    ] {
        let mut m = model(Page::Devices);
        m.rail_expanded = expanded;
        m.paired[0].device.name = device_name.into();
        let peer = m.paired[0].device.id;
        let mut ui = sized_simulator((width, 480.0), crate::view::root(&m));
        snapshot(&mut ui, name);
        let label = ui.find(device_name).unwrap().bounds();
        assert!(label.height <= 48.0, "{label:?}");
        for action in ["Show as display", "Send files…", "More"] {
            let target = ui.click(action).unwrap();
            assert_eq!(target.visible_bounds(), Some(target.bounds()));
            assert!(target.bounds().height <= 24.0, "{action}: {target:?}");
            assert!(target.bounds().y >= label.y + label.height);
        }
        ui.click("Unpair…").unwrap();
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::ShowDisplay(id) if *id == peer))
        );
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::SendFiles(id) if *id == peer))
        );
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::Unpair(id) if *id == peer))
        );
    }
}

#[test]
fn controller_picker_scales_and_preserves_saved_prefixes() {
    let mut m = model(Page::Settings);
    m.paired.extend((10..15).map(|n| Paired {
        device: Device {
            id: id(n),
            name: format!("Workstation {n} — Development and Design"),
            os: Some(Os::Windows),
        },
        connection: None,
    }));
    m.config.control.controller = Some(m.paired[0].device.id.to_string()[..12].into());
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    snapshot_theme(&mut ui, "settings-compact-many-devices", true);
    let target = ui.click("Only Tommy's MacBook Pro").unwrap();
    assert_eq!(target.visible_bounds(), Some(target.bounds()));
    ui.click("Any device").unwrap();
    assert!(
        ui.into_messages()
            .any(|m| matches!(m, Message::ControlMode(None)))
    );

    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    ui.click("Only Tommy's MacBook Pro").unwrap();
    ui.click("Only this device").unwrap();
    assert!(
        ui.into_messages().any(
            |msg| matches!(msg, Message::ControlMode(Some(id)) if id == m.this.id.to_string())
        )
    );

    // A controller outside the popup's visible portion stays reachable by keyboard.
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    ui.click("Only Tommy's MacBook Pro").unwrap();
    ui.tap_key(iced::keyboard::key::Named::End);
    ui.tap_key(iced::keyboard::key::Named::Enter);
    assert!(ui.into_messages().any(|msg| matches!(msg, Message::ControlMode(Some(id)) if id == m.paired.last().unwrap().device.id.to_string())));

    m.config.control.controller = Some("unavailable-controller".into());
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    assert!(ui.find("Unavailable device").is_ok());
    ui.click("Unavailable device").unwrap();
    ui.click("Any device").unwrap();
    assert!(
        ui.into_messages()
            .any(|m| matches!(m, Message::ControlMode(None)))
    );
}

#[test]
fn compact_arrangement_fits_and_separates_unplaced_instructions() {
    for unplaced in [false, true] {
        let mut m = model(Page::Arrangement);
        if unplaced {
            m.config.neighbors.clear();
            m.paired[0].device.name =
                "Tommy's MacBook Pro — Personal Development and Design".into();
        }
        let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
        snapshot_theme(
            &mut ui,
            if unplaced {
                "arrangement-compact-unplaced"
            } else {
                "arrangement-compact"
            },
            unplaced,
        );
        let canvas = ui
            .find(iced::widget::Id::new("arrangement-canvas"))
            .unwrap();
        assert_eq!(canvas.visible_bounds(), Some(canvas.bounds()));
        assert!(canvas.bounds().height >= 180.0, "{canvas:?}");
        if unplaced {
            let hint = format!(
                "Drag {} next to one of this device's displays.",
                m.paired[0].device.name
            );
            let note = ui.find(hint).unwrap();
            assert_eq!(note.visible_bounds(), Some(note.bounds()));
            assert!(note.bounds().y >= canvas.bounds().y + canvas.bounds().height);
        }
    }
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
fn transfer_failure_feedback_is_visible_and_individually_dismissible() {
    for dark in [false, true] {
        let mut m = model(Page::Devices);
        let failure = "Couldn't send files to \"Tommy's MacBook Pro\": Disk full";
        m.transfer_failures = vec![failure.into()];
        let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
        snapshot_theme(
            &mut ui,
            if dark {
                "transfer-failure-dark"
            } else {
                "transfer-failure"
            },
            dark,
        );
        let target = ui.find(failure).unwrap();
        assert_eq!(target.visible_bounds(), Some(target.bounds()));
        ui.click("Dismiss").unwrap();
        assert!(
            ui.into_messages()
                .any(|m| matches!(m, Message::DismissTransferFailure(text) if text == failure))
        );
    }
}

#[test]
fn many_transfer_failures_remain_scrollable_and_leave_room_for_the_page() {
    let mut m = model(Page::Arrangement);
    m.transfer_failures = (0..20).map(|n| format!("Couldn't receive transfer {n}: permission denied while writing the selected destination folder.")).collect();
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    let canvas = ui
        .find(iced::widget::Id::new("arrangement-canvas"))
        .unwrap();
    assert!(canvas.bounds().height >= 100.0, "{canvas:?}");
    assert_eq!(canvas.visible_bounds(), Some(canvas.bounds()));
    let last = m.transfer_failures.last().unwrap();
    assert!(ui.find(last.clone()).unwrap().visible_bounds().is_none());
    let first = ui
        .find(m.transfer_failures[0].clone())
        .unwrap()
        .visible_bounds()
        .unwrap();
    ui.point_at(first.center());
    ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
        delta: iced::mouse::ScrollDelta::Pixels {
            x: 0.0,
            y: -10000.0,
        },
    })]);
    let target = ui.find(last.clone()).unwrap();
    assert_eq!(
        target.visible_bounds().unwrap().size(),
        target.bounds().size()
    );
}

#[test]
fn arrangement_keeps_long_diagnostics_scrollable_without_hiding_the_canvas() {
    let mut m = model(Page::Arrangement);
    m.problems = (0..20).map(|n| format!("Diagnostic message {n}")).collect();
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    let canvas = ui
        .find(iced::widget::Id::new("arrangement-canvas"))
        .unwrap()
        .bounds();
    assert!(canvas.height >= 100.0, "{canvas:?}");
    assert!(
        ui.find("Diagnostic message 19")
            .unwrap()
            .visible_bounds()
            .is_none()
    );
    let first = ui
        .find("Diagnostic message 0")
        .unwrap()
        .visible_bounds()
        .unwrap();
    ui.point_at(first.center());
    ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
        delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -1000.0 },
    })]);
    let last = ui.find("Diagnostic message 19").unwrap();
    let visible = last.visible_bounds().unwrap();
    assert_eq!(visible.size(), last.bounds().size());
    assert!(visible.y >= canvas.y + canvas.height && visible.y + visible.height <= 480.0);
    assert_eq!(
        ui.find(iced::widget::Id::new("arrangement-canvas"))
            .unwrap()
            .bounds(),
        canvas
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
    let mut ui = simulator(crate::view::viewer("Tommy's MacBook Pro", None, None));
    snapshot(&mut ui, "viewer-waiting");
    assert!(
        ui.find("Waiting for \"Tommy's MacBook Pro\" to add its display…")
            .is_ok()
    );
}

fn sample_stats() -> legato_engine::ViewerStats {
    legato_engine::ViewerStats {
        width: 3840,
        height: 2160,
        fps: 59.6,
        megabits_per_second: 24.13,
        mac: Duration::from_millis(18),
        network: Duration::from_micros(1200),
        decode: Duration::from_millis(7),
        relayed: false,
        mac_max: Duration::from_millis(31),
        decode_max: Duration::from_millis(12),
        longest_gap: Duration::from_millis(48),
        skipped: 2,
        moving: false,
    }
}

#[test]
fn stats_add_up_the_delay_stage_by_stage() {
    assert_eq!(
        crate::view::stats_text(
            &sample_stats(),
            Duration::from_millis(3),
            Duration::from_millis(6)
        ),
        [
            "3840×2160 · 60 fps · 24.1 Mbit/s · direct",
            "29 ms behind: Mac 18 · network 1 · decode 7 · display 3",
            "Worst: Mac 31 · decode 12 · display 6 · longest gap 48 ms · 2 skipped",
        ]
    );
    let moving = legato_engine::ViewerStats {
        width: 1920,
        height: 1080,
        moving: true,
        ..sample_stats()
    };
    assert_eq!(
        crate::view::stats_text(&moving, Duration::ZERO, Duration::ZERO)[0],
        "1920×1080 while moving · 60 fps · 24.1 Mbit/s · direct"
    );
}

#[test]
fn the_stats_panel_sits_in_the_corner() {
    let lines = crate::view::stats_text(
        &sample_stats(),
        Duration::from_millis(3),
        Duration::from_millis(6),
    );
    let mut ui = sized_simulator(
        (1000.0, 220.0),
        iced::widget::container(crate::view::stats_panel(lines))
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .style(|_| iced::widget::container::Style {
                background: Some(iced::Color::from_rgb(0.2, 0.3, 0.5).into()),
                ..Default::default()
            }),
    );
    snapshot(&mut ui, "viewer-stats");
    assert!(ui.find("3840×2160 · 60 fps · 24.1 Mbit/s · direct").is_ok());
}

fn with_display_options(placement: legato_engine::config::Placement) -> Model {
    let mut m = model(Page::Devices);
    let mac = m.paired[0].device.id;
    let mut options = crate::model::DisplayOptions::from_config(mac, &m.config.extend);
    options.placement = placement;
    options.display = 2;
    options.fps = 60;
    m.display_options = Some(options);
    m.display_options_open = true;
    m
}

#[test]
fn display_options_separate_the_display_from_the_stream() {
    use legato_engine::config::{MovingSize, Placement, Quality, Resolution};
    // Adaptive quality, the default: full size when still, 1920×1080 while moving.
    let m = with_display_options(Placement::FullScreen);
    let mut ui = sized_simulator((1024.0, 1800.0), crate::view::root(&m));
    snapshot(&mut ui, "display-options");
    assert!(
        ui.find("A 3840×2160 display at 60 Hz: the size of display 2, counted from the left.")
            .is_ok()
    );
    assert!(
        ui.find(
            "You'll see 3840×2160 when still and 1920×1080 while things move, at up to 60 fps."
        )
        .is_ok()
    );
    ui.click("1440p (sharper)").unwrap();
    ui.click("144 Hz").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(messages.iter().any(|m| matches!(
        m,
        Message::DisplayOptionsChanged(o) if o.while_moving == MovingSize::Qhd
    )));
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::DisplayOptionsChanged(o) if o.fps == 144))
    );

    // Full 4K at 120 Hz: the display runs at 120, but Legato caps the stream at 60.
    let mut m = with_display_options(Placement::FullScreen);
    if let Some(o) = &mut m.display_options {
        o.quality = Quality::Sharpest;
        o.fps = 120;
    }
    let mut ui = sized_simulator((1024.0, 1600.0), crate::view::root(&m));
    assert!(ui.find("1080p (quicker)").is_err(), "adaptive only");
    assert!(
        ui.find(
            "You'll see 3840×2160 at up to 60 fps. Legato limits streaming at this resolution; \
             Balanced allows up to 120 fps."
        )
        .is_ok()
    );
    ui.click("1440p").unwrap();
    ui.click("90 Hz").unwrap();
    ui.click("In a window").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(messages.iter().any(|m| matches!(
        m,
        Message::DisplayOptionsChanged(o) if o.resolution == Resolution::Fixed && o.fixed == (2560, 1440)
    )));
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::DisplayOptionsChanged(o) if o.fps == 90))
    );
    assert!(messages.iter().any(|m| matches!(
        m,
        Message::DisplayOptionsChanged(o) if o.placement == Placement::Window
    )));

    // Balanced sends 4K as 2560×1440, which keeps up with 120.
    let mut m = with_display_options(Placement::FullScreen);
    if let Some(o) = &mut m.display_options {
        o.quality = Quality::Balanced;
        o.fps = 120;
    }
    let mut ui = sized_simulator((1024.0, 1600.0), crate::view::root(&m));
    assert!(ui.find("You'll see 2560×1440 at up to 120 fps.").is_ok());

    // In a window, sizes follow the window.
    let m = with_display_options(Placement::Window);
    let mut ui = sized_simulator((1024.0, 1600.0), crate::view::root(&m));
    assert!(
        ui.find("The window's size, updated when you finish resizing it, at 60 Hz.")
            .is_ok()
    );
    assert!(
        ui.find(
            "You'll see the window's full size when still and up to 1920×1080 while things \
             move, at up to 60 fps."
        )
        .is_ok()
    );

    // Choosing and showing.
    let mut m = with_display_options(Placement::FullScreen);
    if let Some(o) = &mut m.display_options {
        o.resolution = Resolution::Fixed;
        o.fixed = (2560, 1440);
    }
    let mut ui = sized_simulator((1024.0, 1600.0), crate::view::root(&m));
    ui.click("120 Hz").unwrap();
    ui.click("Show").unwrap();
    let messages: Vec<_> = ui.into_messages().collect();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::DisplayOptionsChanged(o) if o.fps == 120))
    );
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::DisplayOptionsDone(true)))
    );
}

#[test]
fn with_many_displays_the_display_is_picked_from_a_list() {
    use legato_engine::config::Placement;
    let mut m = with_display_options(Placement::FullScreen);
    m.local
        .displays
        .push(display(7680.0, 3840.0, 2160.0, false));
    m.local
        .displays
        .push(display(11520.0, 3840.0, 2160.0, false));
    let mut ui = sized_simulator((1024.0, 1600.0), crate::view::root(&m));
    // Five displays don't fit as buttons: the chosen one shows in a closed list.
    assert!(ui.find("Display 2").is_ok());
    assert!(ui.find("Display 5").is_err());
    // With three, each is a button.
    let m = with_display_options(Placement::FullScreen);
    let mut ui = sized_simulator((1024.0, 1600.0), crate::view::root(&m));
    assert!(ui.find("Display 3").is_ok());
}

#[test]
fn display_options_are_saved_to_the_settings() {
    use legato_engine::config::{Extend, Placement, Resolution};
    let mut extend = Extend::default();
    let options = crate::model::DisplayOptions {
        peer: id(9),
        placement: Placement::FullScreen,
        display: 3,
        resolution: Resolution::Fixed,
        quality: legato_engine::config::Quality::Balanced,
        while_moving: legato_engine::config::MovingSize::Qhd,
        fixed: (1920, 1080),
        fps: 144,
    };
    options.save_to(&mut extend);
    assert_eq!(extend.placement, Some(Placement::FullScreen));
    assert_eq!(
        (extend.display, extend.width, extend.height, extend.fps),
        (3, 1920, 1080, 144)
    );
    assert_eq!(
        crate::model::DisplayOptions::from_config(id(9), &extend),
        options
    );
}

#[test]
fn display_options_fit_the_default_window() {
    use legato_engine::config::Placement;
    let m = with_display_options(Placement::FullScreen);
    // The main window's default size: every choice should fit without scrolling.
    let mut ui = sized_simulator((920.0, 640.0), crate::view::root(&m));
    snapshot(&mut ui, "display-options-default-window");
    for label in [
        "Resolution",
        "Refresh rate",
        "Stream quality",
        "1080p (quicker)",
        "Show",
    ] {
        let target = ui.find(label).unwrap();
        assert_eq!(target.visible_bounds(), Some(target.bounds()), "{label}");
    }
}

#[test]
fn compact_display_options_keep_actions_visible_while_scrolling() {
    use legato_engine::config::Placement;
    let m = with_display_options(Placement::FullScreen);
    let mut ui = sized_simulator((640.0, 480.0), crate::view::root(&m));
    for label in ["Cancel", "Show"] {
        let target = ui.find(label).unwrap();
        assert_eq!(target.visible_bounds(), Some(target.bounds()));
    }
    let position = ui
        .find("Resolution")
        .unwrap()
        .visible_bounds()
        .unwrap()
        .center();
    ui.point_at(position);
    ui.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
        delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -1000.0 },
    })]);
    snapshot_theme(&mut ui, "display-options-compact-dark", true);
    let target = ui.find("1080p (quicker)").unwrap();
    assert_eq!(
        target.visible_bounds().unwrap().size(),
        target.bounds().size()
    );
    ui.click("1440p (sharper)").unwrap();
    ui.click("Cancel").unwrap();
    assert!(
        ui.into_messages()
            .any(|msg| matches!(msg, Message::DisplayOptionsDone(false)))
    );
}

/// Renders what the GPU draws, with whatever backend `ICED_TEST_BACKEND` and
/// `WGPU_BACKEND` pick (CI runs it on Windows with DirectX 12), and checks the pixels
/// come out: the dialogs, and the viewer's NV12 shader drawing a known colour.
#[test]
#[ignore = "needs a GPU backend: ICED_TEST_BACKEND=wgpu"]
fn gpu_renders_the_dialogs_and_the_viewer() {
    use legato_engine::config::Placement;
    let theme = crate::app_theme(false);
    let render = |element: crate::Element<'_, Message>, size: (f32, f32), name: &str| {
        let mut ui = sized_simulator(size, element);
        let dir = std::env::temp_dir().join(format!("legato-gpu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ui.snapshot(&theme)
            .unwrap()
            .matches_image(dir.join(name))
            .unwrap();
        let file = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        eprintln!("rendered {}", file.display());
        let image = read_png(&file).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        image
    };
    let m = with_display_options(Placement::FullScreen);
    render(crate::view::root(&m), (920.0, 640.0), "dialog");
    render(
        crate::view::root(&model(Page::Arrangement)),
        (920.0, 640.0),
        "arrangement",
    );

    // A solid colour through the viewer's shader: BT.709 limited-range Y 102, Cb 81,
    // Cr 215 is orange, about (255, 64, 0) in RGB.
    let (w, h) = (64u32, 36u32);
    let mut data = vec![102u8; (w * h) as usize];
    data.extend(std::iter::repeat_n([81u8, 215u8], (w * h / 4) as usize).flatten());
    let frame = std::sync::Arc::new(legato_engine::Picture {
        nv12: legato_screen::Nv12 {
            width: w,
            height: h,
            stride: w,
            data,
        },
        decoded_at: std::time::Instant::now(),
        mac: Duration::ZERO,
        network: Duration::ZERO,
        decode: Duration::ZERO,
    });
    let (width, height, rgba) = render(
        crate::view::viewer(
            "Tommy's MacBook Pro",
            Some(tokio::sync::watch::channel(Some(frame)).1),
            None,
        ),
        (320.0, 180.0),
        "viewer",
    );
    let i = ((height / 2 * width + width / 2) * 4) as usize;
    let centre = &rgba[i..i + 3];
    eprintln!("centre pixel: {centre:?}");
    let close = |got: u8, want: u8| got.abs_diff(want) <= 12;
    assert!(
        close(centre[0], 255) && close(centre[1], 64) && close(centre[2], 0),
        "the picture is drawn in the right colour: {centre:?}"
    );
}

/// The UI thread isn't in the engine's runtime: timers must still work from it. (Making
/// a tokio timer there crashed the app when the viewer window opened.)
#[test]
fn timers_can_be_started_from_the_ui_thread() {
    assert!(
        tokio::runtime::Handle::try_current().is_err(),
        "not in a runtime here"
    );
    let started = std::time::Instant::now();
    iced::futures::executor::block_on(crate::after(Duration::from_millis(20)));
    assert!(started.elapsed() >= Duration::from_millis(20));
}
