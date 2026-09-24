//! The window's contents.

use iced::widget::{Space, canvas, column, container, row, scrollable};
use iced::{Alignment, Length};
use iced_m3::dialog::{dialog, modal};
use iced_m3::{
    ButtonVariant, Element, NavigationItem, TypeScale, app_bar, button, card, icon, list,
    list_item, loading_indicator, navigation_rail, slider, snackbar, switch, typography,
};
use legato_core::Layout;
use legato_engine::arrange::{self, ConnectedPeer};
use legato_engine::config::Remap;
use legato_net::PathKind;
use legato_proto::format_pairing_code;

use crate::Message;
use crate::editor::{Editor, PeerBox};
use crate::icons;
use crate::model::{Model, Page, PairingStage, os_name};

pub fn root(m: &Model) -> Element<'_, Message> {
    let rail = navigation_rail(
        [
            NavigationItem::new(Page::Devices, "Devices", icon(icons::devices())),
            NavigationItem::new(Page::Arrangement, "Arrangement", icon(icons::arrangement())),
            NavigationItem::new(Page::Settings, "Settings", icon(icons::settings())),
        ],
        Some(m.page),
    )
    .on_select(Message::Page);

    let sharing = switch(m.sharing)
        .label("Sharing")
        .on_toggle(Message::ToggleSharing);
    let content: Element<'_, Message> = match m.page {
        Page::Devices => devices(m),
        Page::Arrangement => arrangement(m),
        Page::Settings => settings(m),
    };
    let body = column![
        app_bar("Legato").action(sharing),
        scrollable(container(content).padding([8, 24]).width(Length::Fill)).height(Length::Fill),
    ]
    .width(Length::Fill);

    let main = iced_m3::snackbar::host(
        row![rail, body].height(Length::Fill),
        m.notice.as_ref().map(|(id, text)| {
            snackbar(text.clone())
                .id(*id)
                .on_dismiss(Message::DismissNotice)
        }),
    );
    iced_m3::focus::scope(modal(main, pairing_dialog(m), m.pairing_open))
}

fn heading(text: &str) -> Element<'_, Message> {
    container(typography(text, TypeScale::TitleMedium))
        .padding([12, 0])
        .into()
}

fn body(text: impl Into<String>) -> Element<'static, Message> {
    typography(text.into(), TypeScale::BodyMedium).into()
}

fn devices(m: &Model) -> Element<'_, Message> {
    let this = card(
        column![
            typography(m.this.name.clone(), TypeScale::TitleLarge),
            body(format!(
                "{} · {}",
                os_name(m.this.os),
                m.this.id.fmt_short()
            )),
        ]
        .spacing(4),
    )
    .width(Length::Fill);

    let paired: Element<'_, Message> = if m.paired.is_empty() {
        body("None yet. Pair with a device below.")
    } else {
        list(m.paired.iter().map(|p| {
            let status = match (&p.connection, m.active == Some(p.device.id)) {
                (Some(_), true) => "Connected · in use".to_string(),
                (Some(Some((PathKind::Direct, rtt))), false) => {
                    format!("Connected · {:.0} ms", rtt.as_secs_f64() * 1000.0)
                }
                (Some(Some((PathKind::Relay, rtt))), false) => {
                    format!("Connected via relay · {:.0} ms", rtt.as_secs_f64() * 1000.0)
                }
                (Some(None), false) => "Connected".to_string(),
                (None, _) if m.sharing => "Not connected: is Legato running there?".to_string(),
                (None, _) => "Not connected".to_string(),
            };
            let send = button("Send files…").variant(ButtonVariant::Tonal);
            let send = if p.connection.is_some() {
                send.on_press(Message::SendFiles(p.device.id))
            } else {
                send
            };
            list_item(p.device.name.clone())
                .supporting_text(format!("{} · {status}", os_name(p.device.os)))
                .trailing(
                    row![
                        send,
                        button("Unpair")
                            .variant(ButtonVariant::Text)
                            .on_press(Message::Unpair(p.device.id)),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .into()
        }))
        .into()
    };

    let unpaired: Vec<_> = m
        .nearby
        .iter()
        .filter(|n| m.paired(&n.id).is_none())
        .collect();
    let nearby: Element<'_, Message> = if unpaired.is_empty() {
        row![
            container(loading_indicator()).width(32).height(32),
            body("Looking for Legato on this network…"),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
    } else {
        list(unpaired.into_iter().map(|n| {
            list_item(n.name.clone())
                .supporting_text(os_name(n.os))
                .trailing(button("Pair").on_press(Message::Pair(n.id)))
                .into()
        }))
        .into()
    };

    column![
        heading("This device"),
        this,
        heading("Paired devices"),
        paired,
        heading("Nearby"),
        nearby,
        body("Other devices appear here while Legato is open on them. Drop files on this window to send them to the device you're using."),
    ]
    .spacing(8)
    .into()
}

/// The editor's view of this machine and its paired peers, positioned by the settings.
pub fn editor(m: &Model) -> Editor {
    let local_layout = Layout::new(m.local.clone());
    let local: Vec<_> = local_layout.local().desk_rects().collect();
    let local_bounds = crate::editor::bounds(local.iter().copied());
    let primary = m.local.displays.iter().position(|d| d.primary);

    // Peers we know displays for, positioned by the saved layout.
    let known: Vec<_> = m
        .paired
        .iter()
        .filter_map(|p| m.known.get(&p.device.id).map(|s| (p, s)))
        .collect();
    let connected: Vec<ConnectedPeer<'_>> = known
        .iter()
        .enumerate()
        .map(|(i, (p, s))| ConnectedPeer {
            machine: legato_core::MachineId(i as u32 + 1),
            id: p.device.id.to_string(),
            name: &p.device.name,
            screens: s,
            placed_us_at: m.placed_us.get(&p.device.id).copied(),
        })
        .collect();
    let (layout, _) = arrange::build(&m.local, &connected, &m.config);
    let mut unplaced_x = local_bounds.map_or(0.0, |b| b.right() + 200.0);
    let peers = known
        .iter()
        .enumerate()
        .map(|(i, (p, s))| {
            let machine = legato_core::MachineId(i as u32 + 1);
            let (displays, placed) = match layout.machine(machine) {
                Some(placed) => (placed.desk_rects().collect(), true),
                None => {
                    // Not placed yet: park it to the right of this machine's displays.
                    let mut l = Layout::new(m.local.clone());
                    l.place_at(
                        machine,
                        (*s).clone(),
                        legato_proto::Point::new(unplaced_x, 0.0),
                    );
                    let rects: Vec<_> = l
                        .machine(machine)
                        .map(|mm| mm.desk_rects().collect())
                        .unwrap_or_default();
                    if let Some(b) = crate::editor::bounds(rects.iter().copied()) {
                        unplaced_x = b.right() + 200.0;
                    }
                    (rects, false)
                }
            };
            PeerBox {
                id: p.device.id,
                name: p.device.name.clone(),
                displays,
                placed,
            }
        })
        .collect();
    Editor {
        local,
        primary,
        peers,
    }
}

fn arrangement(m: &Model) -> Element<'_, Message> {
    let missing: Vec<_> = m
        .paired
        .iter()
        .filter(|p| !m.known.contains_key(&p.device.id))
        .map(|p| p.device.name.clone())
        .collect();
    let mut col = column![
        heading("Arrangement"),
        body(
            "Drag your other devices to where they sit on your desk. The cursor moves across \
             wherever they touch this device's displays."
        ),
        card(
            canvas(editor(m))
                .width(Length::Fill)
                .height(Length::Fixed(380.0))
        )
        .width(Length::Fill),
    ]
    .spacing(8);
    if !missing.is_empty() {
        col = col.push(body(format!(
            "Waiting to connect to {} once to learn its displays.",
            missing.join(", ")
        )));
    }
    for problem in &m.problems {
        col = col.push(body(problem.clone()));
    }
    col.into()
}

fn settings(m: &Model) -> Element<'_, Message> {
    let push = m.config.switching.push_distance as f32;
    let mut col = column![
        heading("Switching"),
        body(format!(
            "Edge resistance: {push:.0}. How far to keep pushing past a shared edge before the \
             cursor moves to the other device."
        )),
        slider(0.0..=100.0, push)
            .step(5.0)
            .labeled(true)
            .on_change(Message::PushDistance)
            .on_release(Message::SaveSettings),
        heading("Control"),
        body("Which device's keyboard and mouse can move onto the others. This applies to all your paired devices."),
        control_mode(m),
        heading("Keyboard"),
        switch(m.config.keys.remap == Remap::Auto)
            .label("On Macs, use the key next to the space bar as Command")
            .on_toggle(Message::Remap),
        heading("Scrolling"),
        switch(m.config.scrolling.invert_wheel)
            .label("Reverse the mouse wheel when this device is being controlled")
            .on_toggle(Message::InvertWheel),
        heading("Clipboard"),
        switch(m.config.clipboard.enabled)
            .label("Share copied text, images and files with paired devices")
            .on_toggle(Message::Clipboard),
        heading("General"),
    ]
    .spacing(8);
    col = col.push(match m.autostart {
        Some(on) => switch(on)
            .label("Open Legato when you log in")
            .on_toggle(Message::Autostart),
        None => switch(false).label("Open Legato when you log in (unavailable)"),
    });
    col.push(heading("About"))
        .push(body(format!("Legato {}", m.version)))
        .push(body(format!("Device id: {}", m.this.id)))
        .push(body(format!("Settings folder: {}", m.state_dir)))
        .push(Space::new().height(16))
        .into()
}

fn control_mode(m: &Model) -> Element<'_, Message> {
    use iced_m3::{Segment, SegmentSelection, segmented_buttons};
    let this = m.this.id.to_string();
    let mut segments = vec![
        Segment::new(None, "Any device"),
        Segment::new(Some(this.clone()), "Only this device"),
    ];
    for p in &m.paired {
        segments.push(Segment::new(
            Some(p.device.id.to_string()),
            format!("Only {}", p.device.name),
        ));
    }
    // Normalise a stored id prefix to the full id it matches.
    let selected = m.config.control.controller.as_ref().map(|c| {
        std::iter::once(this.clone())
            .chain(m.paired.iter().map(|p| p.device.id.to_string()))
            .find(|id| id.starts_with(c.as_str()))
            .unwrap_or_else(|| c.clone())
    });
    segmented_buttons(segments, SegmentSelection::Single(Some(selected)))
        .on_change(|selection| match selection {
            SegmentSelection::Single(Some(value)) => Message::ControlMode(value),
            _ => Message::ControlMode(None),
        })
        .into()
}

fn pairing_dialog(m: &Model) -> iced_m3::Dialog<'_, Message> {
    let (title, code, stage, name) = match &m.pairing {
        Some(p) => (
            if p.incoming {
                format!("\"{}\" wants to pair", p.name)
            } else {
                format!("Pair with \"{}\"?", p.name)
            },
            p.code.map(format_pairing_code),
            p.stage.clone(),
            p.name.clone(),
        ),
        None => (String::new(), None, PairingStage::Connecting, String::new()),
    };
    let mut content = column![typography(title, TypeScale::HeadlineSmall)].spacing(16);
    content = match (&stage, code) {
        (PairingStage::Connecting, _) | (_, None) => content.push(
            row![
                container(loading_indicator()).width(32).height(32),
                body(format!("Connecting to \"{name}\"…"))
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ),
        (_, Some(code)) => content
            .push(body(format!("Check that \"{name}\" shows the same code:")))
            .push(typography(code, TypeScale::DisplayMedium)),
    };
    if stage == PairingStage::Waiting {
        content = content.push(
            row![
                container(loading_indicator()).width(32).height(32),
                body(format!("Waiting for \"{name}\" to confirm…"))
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }
    let confirm = button("Codes match").variant(ButtonVariant::Text);
    let confirm = if stage == PairingStage::Confirm {
        confirm.on_press(Message::ConfirmPair(true))
    } else {
        confirm
    };
    dialog(content)
        .actions(
            row![
                button("Cancel")
                    .variant(ButtonVariant::Text)
                    .on_press(Message::ConfirmPair(false)),
                confirm,
            ]
            .spacing(8)
            .wrap(),
        )
        .on_dismiss(Message::ConfirmPair(false))
        .dismiss_on_outside(false)
}
