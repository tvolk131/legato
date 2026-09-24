//! Tests against the real input system. They move the cursor, so they're ignored by
//! default: `cargo test -p legato-windows -- --ignored --test-threads=1`. CI runs them.
#![cfg(windows)]

use std::sync::mpsc;
use std::time::Duration;

use legato_core::controller::{Action, Controller, ControllerConfig, Event};
use legato_core::{Align, Layout, MachineId, Side};
use legato_proto::{Control, Display, Rect, Screens};
use legato_windows::{Capture, CaptureOptions, Command, screens};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

const PEER: MachineId = MachineId(1);

fn cursor() -> POINT {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).unwrap() };
    p
}

fn send(inputs: &[INPUT]) {
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    assert_eq!(sent as usize, inputs.len(), "SendInput was blocked");
    std::thread::sleep(Duration::from_millis(30));
}

fn mouse_move(dx: i32, dy: i32) {
    send(&[INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                dwFlags: MOUSEEVENTF_MOVE,
                ..Default::default()
            },
        },
    }]);
}

fn key(scancode: u16, down: bool) {
    let mut flags = KEYEVENTF_SCANCODE;
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    send(&[INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scancode,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }]);
}

#[test]
fn reports_displays() {
    let s = screens();
    assert!(!s.displays.is_empty());
    assert_eq!(s.displays.iter().filter(|d| d.primary).count(), 1, "{s:?}");
    assert!(s.native_per_desk >= 1.0, "{s:?}");
    for pair in s.displays.windows(2) {
        assert!(
            pair[0].bounds.x <= pair[1].bounds.x,
            "sorted left to right: {s:?}"
        );
    }
}

#[test]
#[ignore = "moves the cursor"]
fn crossing_an_edge_captures_forwards_input_and_yield_releases() {
    let local = screens();
    let rightmost = (0..local.displays.len())
        .max_by(|&a, &b| {
            local.displays[a]
                .bounds
                .right()
                .total_cmp(&local.displays[b].bounds.right())
        })
        .unwrap();
    let edge = local.displays[rightmost].bounds;
    let mut layout = Layout::new(local.clone());
    let peer = Screens {
        displays: vec![Display {
            id: 1,
            bounds: Rect::new(0.0, 0.0, 1000.0, 800.0),
            pixel_scale: 2.0,
            ui_scale: 1.0,
            primary: true,
            name: "peer".into(),
        }],
        native_per_desk: 1.0,
    };
    assert!(layout.place_next_to_local(PEER, peer, rightmost, Side::Right, Align::Center, 0.0));
    let controller = Controller::new(
        ControllerConfig {
            push_distance: 0.0,
            ..Default::default()
        },
        layout,
    );
    let (tx, rx) = mpsc::channel();
    let capture = Capture::start(
        controller,
        CaptureOptions {
            accept_injected: true,
        },
        move |action| {
            let _ = tx.send(action);
        },
    )
    .unwrap();

    let start = POINT {
        x: edge.right() as i32 - 3,
        y: edge.center().y as i32,
    };
    unsafe { SetCursorPos(start.x, start.y).unwrap() };
    for _ in 0..10 {
        mouse_move(25, 0);
    }
    let actions: Vec<Action> = rx.try_iter().collect();
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send {
                to: PEER,
                msg: Control::Enter { .. }
            }
        )),
        "never entered the peer: {actions:?}, cursor at {:?}",
        cursor()
    );

    // Captured: the cursor is pinned in the middle of the primary display...
    let primary = local.displays.iter().find(|d| d.primary).unwrap().bounds;
    let pinned = cursor();
    assert!(
        (pinned.x - primary.center().x as i32).abs() <= 1
            && (pinned.y - primary.center().y as i32).abs() <= 1,
        "cursor not pinned: {pinned:?}"
    );
    // ...and motion and keys go to the peer instead of Windows.
    mouse_move(-10, 5);
    mouse_move(-10, 5);
    key(0x64, true); // F13
    key(0x64, false);
    let actions: Vec<Action> = rx.try_iter().collect();
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Datagram { to: PEER, .. })),
        "{actions:?}"
    );
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send {
                msg: Control::Key {
                    usage: 0x68,
                    down: true
                },
                ..
            }
        )),
        "{actions:?}"
    );
    assert_eq!(
        cursor(),
        pinned,
        "swallowed motion must not move the cursor"
    );

    // The peer takes control back: the cursor reappears where it left Windows.
    capture.send(Command::Event(Event::PeerYield(PEER)));
    std::thread::sleep(Duration::from_millis(200));
    let back = cursor();
    assert!(
        back.x >= edge.right() as i32 - 2,
        "cursor not returned to the edge: {back:?}"
    );
    let actions: Vec<Action> = rx.try_iter().collect();
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Send {
                msg: Control::Leave,
                ..
            }
        )),
        "{actions:?}"
    );

    drop(capture);
}
