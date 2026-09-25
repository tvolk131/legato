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
    let main = modal(main, display_options_dialog(m), m.display_options_open);
    iced_m3::focus::scope(modal(main, pairing_dialog(m), m.pairing_open))
}

/// The stats shown over the Mac's display: what's streaming, how far behind it is, and
/// its worst frames (what shows as stutter).
pub fn stats_text(
    stats: &legato_engine::ViewerStats,
    display: std::time::Duration,
    display_worst: std::time::Duration,
) -> [String; 3] {
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    let total = stats.mac + stats.network + stats.decode + display;
    [
        format!(
            "{}×{} · {:.0} fps · {:.1} Mbit/s · {}",
            stats.width,
            stats.height,
            stats.fps,
            stats.megabits_per_second,
            if stats.relayed { "via relay" } else { "direct" }
        ),
        format!(
            "{:.0} ms behind: Mac {:.0} · network {:.0} · decode {:.0} · display {:.0}",
            ms(total),
            ms(stats.mac),
            ms(stats.network),
            ms(stats.decode),
            ms(display)
        ),
        format!(
            "Worst: Mac {:.0} · decode {:.0} · display {:.0} · longest gap {:.0} ms · {} skipped",
            ms(stats.mac_max),
            ms(stats.decode_max),
            ms(display_worst.max(display)),
            ms(stats.longest_gap),
            stats.skipped
        ),
    ]
}

/// The window showing a Mac's extra display, with `stats` over it if they're shown.
pub fn viewer(
    name: &str,
    frame: Option<legato_engine::ViewerFrame>,
    stats: Option<[String; 3]>,
) -> Element<'static, Message> {
    let content: Element<'static, Message> = match frame {
        Some(frame) => {
            let picture = iced::widget::shader(crate::viewer::Picture(frame))
                .width(Length::Fill)
                .height(Length::Fill);
            match stats {
                Some(lines) => iced::widget::stack![picture, stats_panel(lines)].into(),
                None => picture.into(),
            }
        }
        None => container(
            column![
                container(loading_indicator()).width(48).height(48),
                typography(
                    format!("Waiting for \"{name}\" to add its display…"),
                    TypeScale::BodyLarge
                ),
            ]
            .spacing(16)
            .align_x(Alignment::Center),
        )
        .center(Length::Fill)
        .into(),
    };
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Color::BLACK.into()),
            text_color: Some(iced::Color::WHITE),
            ..Default::default()
        })
        .into()
}

pub(crate) fn stats_panel([line1, line2, line3]: [String; 3]) -> Element<'static, Message> {
    container(
        container(
            column![
                typography(line1, TypeScale::LabelMedium),
                typography(line2, TypeScale::LabelMedium),
                typography(line3, TypeScale::LabelMedium),
            ]
            .spacing(2),
        )
        .padding([6, 10])
        .style(|_| container::Style {
            background: Some(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.6).into()),
            text_color: Some(iced::Color::WHITE),
            border: iced::border::rounded(8),
            ..Default::default()
        }),
    )
    .align_right(Length::Fill)
    .padding(12)
    .into()
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
            let display: Option<Element<'_, Message>> = m.can_view(p).then(|| {
                let main: Element<'_, Message> = if m.viewing == Some(p.device.id) {
                    button("Stop display")
                        .variant(ButtonVariant::Tonal)
                        .on_press(Message::StopDisplay)
                        .into()
                } else {
                    button("Show as display")
                        .variant(ButtonVariant::Tonal)
                        .on_press(Message::ShowDisplay(p.device.id))
                        .into()
                };
                row![
                    main,
                    button("Options…")
                        .variant(ButtonVariant::Text)
                        .on_press(Message::DisplayOptions(p.device.id)),
                ]
                .spacing(4)
                .align_y(Alignment::Center)
                .into()
            });
            list_item(p.device.name.clone())
                .supporting_text(format!("{} · {status}", os_name(p.device.os)))
                .trailing(
                    row![
                        display,
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
    ]
    .spacing(8);
    if m.this.os == Some(legato_proto::Os::Windows) {
        col = col.push(heading("Mac display"));
        col = col.push(
            switch(m.config.extend.stats)
                .label("Show frame rate and delays over the picture (F10)")
                .on_toggle(Message::Stats),
        );
    }
    col = col.push(heading("General"));
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

/// Fixed sizes offered for the Mac's display.
const FIXED_SIZES: [(u32, u32); 3] = [(3840, 2160), (2560, 1440), (1920, 1080)];

fn display_options_dialog(m: &Model) -> iced_m3::Dialog<'_, Message> {
    use iced_m3::{RadioOption, radio_group};
    use legato_engine::config::{Placement, Quality, Resolution};
    let Some(o) = m.display_options.clone() else {
        return dialog(column![]);
    };
    let name = m.name_of(&o.peer);
    let displays = m.displays();
    let changed = |o: crate::model::DisplayOptions| Message::DisplayOptionsChanged(o);

    let mut places: Vec<RadioOption<(Placement, usize)>> = displays
        .iter()
        .enumerate()
        .map(|(i, d)| {
            RadioOption::new(
                (Placement::FullScreen, i + 1),
                format!(
                    "Full screen on display {} ({}×{})",
                    i + 1,
                    d.bounds.width as u32,
                    d.bounds.height as u32
                ),
            )
        })
        .collect();
    places.push(RadioOption::new((Placement::Window, 0), "In a window"));
    let place = match o.placement {
        Placement::FullScreen => (Placement::FullScreen, o.display.min(displays.len()).max(1)),
        Placement::Window => (Placement::Window, 0),
    };
    let where_ = {
        let o = o.clone();
        radio_group(places, Some(place)).on_select(move |(placement, display)| {
            changed(crate::model::DisplayOptions {
                placement,
                display: display.max(1),
                ..o.clone()
            })
        })
    };

    // What "match" means here, for the frame-rate limit.
    let shown = match o.placement {
        Placement::FullScreen => displays
            .get(o.display.saturating_sub(1))
            .map(|d| (d.bounds.width as u32, d.bounds.height as u32)),
        Placement::Window => None,
    };
    let size = match o.resolution {
        Resolution::Match => shown,
        Resolution::Fixed => Some(o.fixed),
    };
    let mut sizes = vec![RadioOption::new(
        (Resolution::Match, (0, 0)),
        match o.placement {
            Placement::FullScreen => "Match the screen".to_string(),
            Placement::Window => "Match the window (changes when you finish resizing)".to_string(),
        },
    )];
    sizes.extend(
        FIXED_SIZES.iter().map(|&(w, h)| {
            RadioOption::new((Resolution::Fixed, (w, h)), format!("Always {w}×{h}"))
        }),
    );
    let resolution = match o.resolution {
        Resolution::Match => (Resolution::Match, (0, 0)),
        Resolution::Fixed => (Resolution::Fixed, o.fixed),
    };
    let size_choice = {
        let o = o.clone();
        radio_group(sizes, Some(resolution)).on_select(move |(resolution, fixed)| {
            changed(crate::model::DisplayOptions {
                resolution,
                fixed: if resolution == Resolution::Fixed {
                    fixed
                } else {
                    o.fixed
                },
                ..o.clone()
            })
        })
    };

    let qualities = [
        (Quality::Sharpest, "Sharpest: full resolution"),
        (
            Quality::Balanced,
            "Balanced: up to 2560×1440, about twice as quick at 4K",
        ),
        (Quality::Fastest, "Fastest: up to 1920×1080"),
    ]
    .map(|(q, label)| RadioOption::new(q, label));
    let quality_choice = {
        let o = o.clone();
        radio_group(qualities, Some(o.quality)).on_select(move |quality| {
            changed(crate::model::DisplayOptions {
                quality,
                ..o.clone()
            })
        })
    };
    // Encoding sets the pace, at the size it's sent at.
    let sent = size.map(|size| o.quality.stream_size(size));
    let max = sent.map(|(w, h)| legato_core::extend::max_fps(w, h));
    let rates = legato_core::extend::FRAME_RATES.map(|fps| {
        RadioOption::new(fps, format!("{fps} fps")).disabled(max.is_some_and(|max| fps > max))
    });
    let fps = max.map_or(o.fps, |max| o.fps.min(max));
    let rate_choice = {
        let o = o.clone();
        radio_group(rates, Some(fps))
            .on_select(move |fps| changed(crate::model::DisplayOptions { fps, ..o.clone() }))
    };
    let rate_note = match (sent, max) {
        (Some((w, h)), Some(max)) => format!(
            "Sent at {w}×{h}, the Mac can keep up with {max} fps. Smaller sizes can go faster."
        ),
        _ => "Limited to what the Mac can keep up with at the window's size: 60 fps at 4K, \
              120 at 2560×1440, 144 at 1920×1080."
            .to_string(),
    };

    let content = column![
        typography(
            format!("Show \"{name}\" as a display"),
            TypeScale::HeadlineSmall
        ),
        heading("Where"),
        where_,
        heading("Size"),
        size_choice,
        heading("Stream quality"),
        quality_choice,
        heading("Frame rate"),
        rate_choice,
        body(rate_note),
    ]
    .spacing(8);
    dialog(scrollable(content).height(Length::Shrink))
        .actions(
            row![
                button("Cancel")
                    .variant(ButtonVariant::Text)
                    .on_press(Message::DisplayOptionsDone(false)),
                button("Show")
                    .variant(ButtonVariant::Text)
                    .on_press(Message::DisplayOptionsDone(true)),
            ]
            .spacing(8)
            .wrap(),
        )
        .on_dismiss(Message::DisplayOptionsDone(false))
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
