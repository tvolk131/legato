//! The window's contents.

use iced::widget::{Space, canvas, column, container, row, scrollable};
use iced::{Alignment, Length};
use iced_m3::dialog::{dialog, modal};
use iced_m3::{
    ButtonSize, ButtonVariant, Element, MenuItem, NavigationItem, TypeScale, app_bar, badged,
    button, button_group, card, icon, icon_button, list, list_item, loading_indicator,
    navigation_rail, slider, snackbar, split_button, switch, typography,
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
    .header(icon_button(icon(icons::menu())).on_press(Message::ToggleRail))
    .expanded(m.rail_expanded)
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
    let main = modal(main, add_device_dialog(m), m.add_device_open);
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
            "{}×{}{} · {:.0} fps · {:.1} Mbit/s · {}",
            stats.width,
            stats.height,
            if stats.moving { " while moving" } else { "" },
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

/// The window showing a Mac's extra display (`pictures`, once the first has come), with
/// `stats` over it if they're shown.
pub fn viewer(
    name: &str,
    pictures: Option<crate::viewer::Source>,
    stats: Option<[String; 3]>,
) -> Element<'static, Message> {
    let content: Element<'static, Message> = match pictures {
        Some(pictures) => {
            let picture = iced::widget::shader(crate::viewer::Picture(pictures))
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
        body("None yet.")
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
            // The main action, with where and how to show it in its menu.
            let display: Option<Element<'_, Message>> = m.can_view(p).then(|| {
                let (label, action) = if m.viewing == Some(p.device.id) {
                    ("Stop display", Message::StopDisplay)
                } else {
                    ("Show as display", Message::ShowDisplay(p.device.id))
                };
                split_button(
                    label,
                    action,
                    [MenuItem::new(
                        "Display options…",
                        Message::DisplayOptions(p.device.id),
                    )],
                )
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

    // Adding one is the way forward until something's paired. The badge counts unpaired
    // devices nearby, so a new one gets noticed without anything animating on the page.
    let add = button("Add a device…")
        .variant(if m.paired.is_empty() {
            ButtonVariant::Filled
        } else {
            ButtonVariant::Tonal
        })
        .on_press(Message::AddDevice(true));
    let add: Element<'_, Message> = match unpaired(m).count() {
        0 => add.into(),
        n => badged(add, Some(n as u32)),
    };

    column![
        heading("This device"),
        this,
        heading("Paired devices"),
        paired,
        container(add).padding([8, 0]),
        body("Drop files on this window to send them to the device you're using."),
    ]
    .spacing(8)
    .into()
}

/// Legato devices on this network that aren't paired with this one.
fn unpaired(m: &Model) -> impl Iterator<Item = &crate::model::Device> {
    m.nearby.iter().filter(|n| m.paired(&n.id).is_none())
}

/// Unpaired devices nearby, to pair with. While none are found it shows a spinner: the
/// only one in the main window that can run for long, and only while this is open.
fn add_device_dialog(m: &Model) -> iced_m3::Dialog<'_, Message> {
    let found: Vec<_> = unpaired(m).collect();
    let found: Element<'_, Message> = if found.is_empty() {
        row![
            container(loading_indicator()).width(32).height(32),
            body("Looking for Legato on this network…"),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .into()
    } else {
        list(found.into_iter().map(|n| {
            list_item(n.name.clone())
                .supporting_text(os_name(n.os))
                .trailing(button("Pair").on_press(Message::Pair(n.id)))
                .into()
        }))
        .into()
    };
    dialog(
        column![
            typography("Add a device", TypeScale::HeadlineSmall),
            body(
                "Open Legato on the other device. It shows up here while it's on the same \
                 network as this one."
            ),
            found,
        ]
        .spacing(16),
    )
    .actions(
        row![
            button("Close")
                .variant(ButtonVariant::Text)
                .on_press(Message::AddDevice(false)),
        ]
        .spacing(8),
    )
    .on_dismiss(Message::AddDevice(false))
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
    let this = m.this.id.to_string();
    let mut choices = vec![
        (None, "Any device".to_string()),
        (Some(this.clone()), "Only this device".to_string()),
    ];
    for p in &m.paired {
        choices.push((
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
    // One choice of several: a connected group of toggle buttons.
    button_group(choices.into_iter().map(|(value, label)| {
        button(label)
            .variant(ButtonVariant::Tonal)
            .selected(value == selected)
            .on_press(Message::ControlMode(value))
    }))
    .connected(true)
    .into()
}

/// Fixed sizes offered for the Mac's display, with their short labels.
const FIXED_SIZES: [(u32, u32, &str); 3] = [
    (3840, 2160, "4K"),
    (2560, 1440, "1440p"),
    (1920, 1080, "1080p"),
];

/// One choice of a few, as a connected group of toggle buttons: (chosen, label, what
/// choosing it sends). Choices that send nothing are disabled.
fn choices<'a>(
    items: impl IntoIterator<Item = (bool, String, Option<Message>)>,
) -> Element<'a, Message> {
    button_group(items.into_iter().map(|(chosen, label, message)| {
        button(label)
            .size(ButtonSize::ExtraSmall)
            .variant(ButtonVariant::Tonal)
            .selected(chosen)
            .on_press_maybe(message)
    }))
    .connected(true)
    .into()
}

/// A labelled choice, with a line about the current choice under it (if any).
fn field<'a>(
    label: &'static str,
    choice: impl Into<Element<'a, Message>>,
    note: Option<String>,
) -> Element<'a, Message> {
    column![typography(label, TypeScale::TitleSmall), choice.into()]
        .push(note.map(|note| typography(note, TypeScale::BodySmall)))
        .spacing(6)
        .into()
}

fn display_options_dialog(m: &Model) -> iced_m3::Dialog<'_, Message> {
    use crate::model::DisplayOptions;
    use iced_m3::{SelectOption, select};
    use legato_engine::config::{MovingSize, Placement, Quality, Resolution};
    let Some(o) = m.display_options.clone() else {
        return dialog(column![]);
    };
    let name = m.name_of(&o.peer);
    let displays = m.displays();
    // What choosing something sends: these options with that one thing changed.
    let change = |set: &dyn Fn(&mut DisplayOptions)| {
        let mut o = o.clone();
        set(&mut o);
        Message::DisplayOptionsChanged(o)
    };
    let display = o.display.clamp(1, displays.len().max(1));
    let display_size = displays
        .get(display - 1)
        .map(|d| (d.bounds.width as u32, d.bounds.height as u32));

    // Where: full screen or a window, then (for full screen) which display.
    let full = o.placement == Placement::FullScreen;
    let mut where_ = column![choices([
        (
            full,
            "Full screen".to_string(),
            Some(change(&|o| o.placement = Placement::FullScreen)),
        ),
        (
            !full,
            "In a window".to_string(),
            Some(change(&|o| o.placement = Placement::Window)),
        ),
    ])]
    .spacing(8);
    if full && displays.len() > 1 {
        where_ = where_.push(if displays.len() <= 4 {
            choices((1..=displays.len()).map(|i| {
                (
                    i == display,
                    format!("Display {i}"),
                    Some(change(&move |o| o.display = i)),
                )
            }))
        } else {
            let o = o.clone();
            select(
                "Display",
                (1..=displays.len()).map(|i| SelectOption::new(i, format!("Display {i}"))),
                Some(display),
            )
            .on_select(move |display| {
                Message::DisplayOptionsChanged(DisplayOptions {
                    display,
                    ..o.clone()
                })
            })
            .into()
        });
    }
    // Which display (and its size) is in the size note, under the next choice.
    let where_note = (!full).then(|| "A window you can move and resize.".to_string());

    // Size: what "match" means depends on where it's shown.
    let shown = if full { display_size } else { None };
    let size = match o.resolution {
        Resolution::Match => shown,
        Resolution::Fixed => Some(o.fixed),
    };
    let size_choice = choices(
        std::iter::once((
            o.resolution == Resolution::Match,
            "Match".to_string(),
            Some(change(&|o| o.resolution = Resolution::Match)),
        ))
        .chain(FIXED_SIZES.iter().map(|&(w, h, label)| {
            (
                o.resolution == Resolution::Fixed && o.fixed == (w, h),
                label.to_string(),
                Some(change(&move |o| {
                    o.resolution = Resolution::Fixed;
                    o.fixed = (w, h);
                })),
            )
        })),
    );
    let size_note = match (o.resolution, shown) {
        (Resolution::Match, Some((w, h))) if displays.len() > 1 => {
            format!("The same size as display {display} (counted from the left): {w}×{h}.")
        }
        (Resolution::Match, Some((w, h))) => format!("The same size as the screen: {w}×{h}."),
        (Resolution::Match, None) => {
            "The window's size, updated when you finish resizing it.".to_string()
        }
        (Resolution::Fixed, _) => format!("Always {}×{}.", o.fixed.0, o.fixed.1),
    };

    let quality_choice = choices(
        [
            (Quality::Adaptive, "Adaptive"),
            (Quality::Sharpest, "Sharpest"),
            (Quality::Balanced, "Balanced"),
            (Quality::Fastest, "Fastest"),
        ]
        .map(|(q, label)| {
            (
                o.quality == q,
                label.to_string(),
                Some(change(&move |o| o.quality = q)),
            )
        }),
    );
    let quality_note = match o.quality {
        Quality::Adaptive => "Full resolution, and smaller while things move.",
        Quality::Sharpest => "Full resolution, always.",
        Quality::Balanced => "Up to 2560×1440: about twice as quick at 4K.",
        Quality::Fastest => "Up to 1920×1080: the quickest.",
    }
    .to_string();
    let moving_choice = choices(
        [
            (MovingSize::Hd, "Up to 1080p"),
            (MovingSize::Qhd, "Up to 1440p"),
        ]
        .map(|(size, label)| {
            (
                o.while_moving == size,
                label.to_string(),
                Some(change(&move |o| o.while_moving = size)),
            )
        }),
    );

    // What the Mac would be asked for at the fastest rate: encoding sets the pace.
    let fastest = *legato_core::extend::FRAME_RATES.last().unwrap_or(&60);
    let request = size.map(|(w, h)| {
        let mut extend = m.config.extend.clone();
        o.save_to(&mut extend);
        extend.fps = fastest;
        extend.request_for(w, h)
    });
    let max = match (request, o.quality) {
        (Some(r), _) => Some(r.fps),
        // In a window of any size, moving pictures are at most this big.
        (None, Quality::Adaptive) => {
            let (w, h) = o.while_moving.cap();
            Some(legato_core::extend::max_fps(w, h))
        }
        (None, _) => None,
    };
    let fps = max.map_or(o.fps, |max| o.fps.min(max));
    // Rates the Mac can't keep up with stay visible, but can't be chosen.
    let rate_choice = choices(legato_core::extend::FRAME_RATES.map(|rate| {
        (
            rate == fps,
            rate.to_string(),
            max.is_none_or(|max| rate <= max)
                .then(|| change(&move |o| o.fps = rate)),
        )
    }));
    let rate_note = match request {
        Some(r) if r.moving_width > 0 => format!(
            "Up to {} fps: sent at {}×{} while things move, {}×{} when still.",
            r.fps, r.moving_width, r.moving_height, r.stream_width, r.stream_height
        ),
        Some(r) => format!(
            "Up to {} fps: sent at {}×{}. Smaller sizes can go faster.",
            r.fps, r.stream_width, r.stream_height
        ),
        None if o.quality == Quality::Adaptive => {
            let (w, h) = o.while_moving.cap();
            format!(
                "Up to {} fps: sent at up to {w}×{h} while things move.",
                max.unwrap_or(fastest)
            )
        }
        None => {
            "Limited by the window's size: 60 fps at 4K, 120 at 1440p, 144 at 1080p.".to_string()
        }
    };

    let content = column![
        typography(
            format!("Show \"{name}\" as a display"),
            TypeScale::HeadlineSmall
        ),
        field("Where", where_, where_note),
        field("Size", size_choice, Some(size_note)),
        // Adaptive quality's size while things move, under it like the display under Where.
        field(
            "Stream quality",
            column![quality_choice]
                .push((o.quality == Quality::Adaptive).then_some(moving_choice))
                .spacing(6),
            Some(quality_note)
        ),
        field("Frame rate", rate_choice, Some(rate_note)),
    ]
    .spacing(12);
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
