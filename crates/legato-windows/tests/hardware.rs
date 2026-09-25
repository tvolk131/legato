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
        |_| {},
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

#[test]
#[ignore = "moves the cursor"]
fn injected_moves_land_exactly_and_are_not_local_input() {
    use legato_core::{Inject, LocalInput};
    let local = screens();
    let primary = local.displays.iter().find(|d| d.primary).unwrap().bounds;
    let (tx, rx) = mpsc::channel::<LocalInput>();
    let capture = Capture::start(
        Controller::new(ControllerConfig::default(), Layout::new(local)),
        CaptureOptions {
            accept_injected: true,
        },
        |_| {},
        move |input| {
            let _ = tx.send(input);
        },
    )
    .unwrap();
    let mut injector = legato_windows::Injector::new();
    let target = legato_proto::Point::new(primary.center().x + 37.0, primary.center().y + 11.0);
    injector.apply(&Inject::MoveTo { pos: target });
    std::thread::sleep(Duration::from_millis(100));
    let p = cursor();
    assert!(
        (p.x - target.x as i32).abs() <= 1 && (p.y - target.y as i32).abs() <= 1,
        "cursor at {p:?}, wanted {target:?}"
    );
    assert_eq!(
        rx.try_iter().count(),
        0,
        "our own injection counted as local input"
    );

    // Untagged input (like the user's mouse) is reported.
    mouse_move(8, 0);
    assert!(
        rx.try_iter()
            .any(|i| matches!(i, LocalInput::Motion { .. }))
    );
    drop(capture);
}

mod viewer {
    //! A stand-in for the app's viewer window: shown on its own thread, like iced's UI
    //! thread, optionally slow to handle its messages, like a thread busy drawing.

    use std::sync::mpsc;
    use std::time::Duration;

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
        GetMessageW, GetWindowThreadProcessId, MSG, PostThreadMessageW, RegisterClassW,
        SetForegroundWindow, TranslateMessage, WM_QUIT, WNDCLASSW, WS_EX_TOPMOST, WS_POPUP,
        WS_VISIBLE,
    };
    use windows::core::w;

    unsafe extern "system" fn proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    pub struct Viewer {
        pub hwnd: HWND,
        thread_id: u32,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Viewer {
        /// A borderless window at `(x, y, w, h)` whose thread takes `busy` over each
        /// message.
        pub fn open(x: i32, y: i32, w: i32, h: i32, busy: Duration) -> Self {
            let (tx, rx) = mpsc::channel();
            let thread = std::thread::spawn(move || unsafe {
                let instance = GetModuleHandleW(None).unwrap().into();
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    hInstance: instance,
                    lpszClassName: w!("LegatoTestViewer"),
                    ..Default::default()
                });
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST,
                    w!("LegatoTestViewer"),
                    w!("Legato test viewer"),
                    WS_POPUP | WS_VISIBLE,
                    x,
                    y,
                    w,
                    h,
                    None,
                    None,
                    Some(instance),
                    None,
                )
                .unwrap();
                tx.send((hwnd.0 as usize, GetCurrentThreadId())).unwrap();
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                    if !busy.is_zero() {
                        std::thread::sleep(busy);
                    }
                }
                let _ = DestroyWindow(hwnd);
            });
            let (hwnd, thread_id) = rx.recv().unwrap();
            std::thread::sleep(Duration::from_millis(200));
            Self {
                hwnd: HWND(hwnd as *mut _),
                thread_id,
                thread: Some(thread),
            }
        }

        /// Tries to make this the foreground window (Windows may refuse). Returns whether
        /// it is.
        pub fn focus(&self) -> bool {
            unsafe {
                let current = GetForegroundWindow();
                let theirs = GetWindowThreadProcessId(current, None);
                let ours = GetCurrentThreadId();
                let attached = theirs != 0 && AttachThreadInput(ours, theirs, true).as_bool();
                let _ = SetForegroundWindow(self.hwnd);
                if attached {
                    let _ = AttachThreadInput(ours, theirs, false);
                }
                std::thread::sleep(Duration::from_millis(200));
                GetForegroundWindow() == self.hwnd
            }
        }
    }

    impl Drop for Viewer {
        fn drop(&mut self) {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

fn click(down: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP};
    send(&[INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dwFlags: if down {
                    MOUSEEVENTF_LEFTDOWN
                } else {
                    MOUSEEVENTF_LEFTUP
                },
                ..Default::default()
            },
        },
    }]);
}

/// What `WindowFromPoint` (which the portal goes by) finds at `(x, y)`: `viewer`, or
/// what else and why.
fn window_at(x: i32, y: i32, viewer: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::Foundation::{CloseHandle, RECT};
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GW_HWNDNEXT, GWL_EXSTYLE, GetAncestor, GetClassNameW, GetForegroundWindow,
        GetTopWindow, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
        GetWindowThreadProcessId, IsWindowVisible, WindowFromPoint,
    };
    unsafe {
        let under = WindowFromPoint(POINT { x, y });
        let root = GetAncestor(under, GA_ROOT);
        if root == viewer {
            return "the viewer".into();
        }
        let mut name = [0u16; 128];
        let len = GetClassNameW(root, &mut name) as usize;
        let mut cloaked = 0u32;
        let cloak = DwmGetWindowAttribute(
            root,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        );
        let mut r = RECT::default();
        let _ = GetWindowRect(root, &mut r);
        // Where it and the viewer come in the z-order, from the top.
        let (mut at, mut viewer_at, mut i) = (None, None, 0);
        let mut next = GetTopWindow(None).ok();
        while let Some(h) = next.filter(|h| !h.is_invalid() && i < 4096) {
            if h == root {
                at = Some(i);
            }
            if h == viewer {
                viewer_at = Some(i);
            }
            next = GetWindow(h, GW_HWNDNEXT).ok();
            i += 1;
        }
        let mut title = [0u16; 128];
        let title_len = GetWindowTextW(root, &mut title) as usize;
        let mut pid = 0u32;
        GetWindowThreadProcessId(root, Some(&mut pid));
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .ok()
            .and_then(|p| {
                let mut path = [0u16; 260];
                let mut size = path.len() as u32;
                let ok = QueryFullProcessImageNameW(
                    p,
                    PROCESS_NAME_WIN32,
                    windows::core::PWSTR(path.as_mut_ptr()),
                    &mut size,
                )
                .is_ok();
                let _ = CloseHandle(p);
                ok.then(|| String::from_utf16_lossy(&path[..size as usize]))
            })
            .unwrap_or_default();
        format!(
            "{} \"{}\" of {} (foreground {}, visible {}, cloaked {cloaked} {}, extended style \
             {:#x}, at {},{} to {},{}, z-order {at:?} vs the viewer's {viewer_at:?})",
            String::from_utf16_lossy(&name[..len]),
            String::from_utf16_lossy(&title[..title_len]),
            process.rsplit('\\').next().unwrap_or_default(),
            GetForegroundWindow() == root,
            IsWindowVisible(root).as_bool(),
            if cloak.is_ok() {
                "(read)"
            } else {
                "(unreadable)"
            },
            GetWindowLongW(root, GWL_EXSTYLE) as u32,
            r.left,
            r.top,
            r.right,
            r.bottom
        )
    }
}

fn windows_key(down: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_EXTENDEDKEY, VK_LWIN};
    let mut flags = KEYEVENTF_EXTENDEDKEY;
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    send(&[INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_LWIN,
                wScan: 0x5b,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }]);
}

/// Virtual monitor mode: with the pointer over the viewer's picture, clicks and keys go
/// to the Mac whether or not the viewer window has focus, however busy its thread is, and
/// even with no motion since the viewer appeared or since the Mac took its cursor back.
#[test]
#[ignore = "moves the cursor and shows a window"]
fn keys_go_through_the_portal_whether_or_not_the_viewer_has_focus() {
    use legato_core::controller::Portal;

    let local = screens();
    let primary = local.displays.iter().find(|d| d.primary).unwrap().bounds;
    let rightmost = (0..local.displays.len())
        .max_by(|&a, &b| {
            local.displays[a]
                .bounds
                .right()
                .total_cmp(&local.displays[b].bounds.right())
        })
        .unwrap();
    let mut layout = Layout::new(local.clone());
    let peer = Screens {
        displays: vec![Display {
            id: 1,
            bounds: Rect::new(0.0, 0.0, 1512.0, 982.0),
            pixel_scale: 2.0,
            ui_scale: 1.0,
            primary: true,
            name: "peer".into(),
        }],
        native_per_desk: 1.0,
    };
    assert!(layout.place_next_to_local(PEER, peer, rightmost, Side::Right, Align::Center, 0.0));
    let (tx, rx) = mpsc::channel();
    let capture = Capture::start(
        Controller::new(ControllerConfig::default(), layout),
        CaptureOptions {
            accept_injected: true,
        },
        move |action| {
            let _ = tx.send(action);
        },
        |_| {},
    )
    .unwrap();

    let (w, h) = (640, 360);
    let (x, y) = (
        primary.center().x as i32 - w / 2,
        primary.center().y as i32 - h / 2,
    );
    let (cx, cy) = (x + w / 2, y + h / 2);
    // What went to the peer since the last call: (entered, clicks, keys).
    let sent = || {
        std::thread::sleep(Duration::from_millis(100));
        let actions: Vec<Action> = rx.try_iter().collect();
        let count = |m: &dyn Fn(&Control) -> bool| {
            actions
                .iter()
                .filter(|a| matches!(a, Action::Send { to: PEER, msg } if m(msg)))
                .count()
        };
        (
            count(&|m| matches!(m, Control::Enter { .. })),
            count(&|m| matches!(m, Control::Button { .. })),
            count(&|m| matches!(m, Control::Key { usage: 0x68, .. })),
        )
    };
    let press = || {
        key(0x64, true); // F13
        key(0x64, false);
    };
    let mut failures = Vec::new();
    for (focused, busy_ms) in [(false, 0), (true, 0), (false, 40), (true, 40)] {
        let viewer = viewer::Viewer::open(x, y, w, h, Duration::from_millis(busy_ms));
        let has_focus = focused && viewer.focus();
        let under = window_at(cx, cy, viewer.hwnd);
        let case = format!(
            "viewer focused: {has_focus} (asked {focused}), its thread busy {busy_ms} ms per \
             message, WindowFromPoint says: {under}"
        );
        eprintln!("{case}");
        if under != "the viewer" {
            // Some runner images show a window above everything (a sign-in prompt).
            eprintln!("  skipped: something else covers the viewer here");
            continue;
        }
        let mut check = |what: &str, got: (usize, usize, usize), want: (usize, usize, usize)| {
            let line = format!(
                "  {what}: entered {}, clicks {}, keys {}",
                got.0, got.1, got.2
            );
            eprintln!("{line}");
            if got != want {
                failures.push(format!("{case}\n{line}, wanted {want:?}"));
            }
        };

        // The viewer appears under a resting pointer.
        unsafe { SetCursorPos(cx, cy).unwrap() };
        std::thread::sleep(Duration::from_millis(50));
        capture.send(Command::SetPortal(Some(Portal {
            peer: PEER,
            remote: Rect::new(1512.0, 0.0, 1920.0, 1080.0),
            window: viewer.hwnd.0 as usize as u64,
        })));
        let _ = sent();
        press();
        check("typing before any motion", sent(), (1, 0, 2));

        mouse_move(3, 0);
        mouse_move(-3, 0);
        click(true);
        click(false);
        press();
        check("moving, clicking and typing", sent(), (0, 2, 2));

        // The Mac's own trackpad or keyboard was used, so it took its cursor back.
        capture.send(Command::Event(Event::PeerYield(PEER)));
        let _ = sent();
        click(true);
        click(false);
        press();
        check(
            "clicking after the Mac took over, without moving",
            sent(),
            (1, 2, 2),
        );

        capture.send(Command::Event(Event::PeerYield(PEER)));
        let _ = sent();
        press();
        check(
            "typing after the Mac took over, without moving",
            sent(),
            (1, 0, 2),
        );

        // Off the picture before the next case.
        unsafe { SetCursorPos(x - 50, y - 50).unwrap() };
        mouse_move(-3, 0);
        capture.send(Command::SetPortal(None));
        let _ = sent();
        drop(viewer);
    }

    // Full screen, like the user's: off the picture, the Windows key opens Start (and the
    // taskbar over the viewer); then Start is closed in a few ways, and typing on the
    // picture should reach the Mac each time.
    let (fx, fy, fw, fh) = (
        primary.x as i32,
        primary.y as i32,
        primary.width as i32,
        primary.height as i32,
    );
    let (mx, my) = (fx + fw / 2, fy + fh / 3);
    let taskbar = unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};
        let mut r = windows::Win32::Foundation::RECT::default();
        FindWindowW(windows::core::w!("Shell_TrayWnd"), None)
            .ok()
            .and_then(|t| GetWindowRect(t, &mut r).ok())
            .map(|()| r)
    };
    eprintln!("taskbar at {taskbar:?}");
    for close in ["Escape", "a click outside Start", "a click on the taskbar"] {
        let viewer = viewer::Viewer::open(fx, fy, fw, fh, Duration::ZERO);
        capture.send(Command::SetPortal(Some(Portal {
            peer: PEER,
            remote: Rect::new(1512.0, 0.0, 1920.0, 1080.0),
            window: viewer.hwnd.0 as usize as u64,
        })));
        let before = window_at(mx, my, viewer.hwnd);
        if before != "the viewer" {
            eprintln!("[{close}] skipped: something else covers the viewer: {before}");
            capture.send(Command::SetPortal(None));
            continue;
        }
        // Off the picture: onto a taskbar, or anywhere, then back.
        unsafe { SetCursorPos(fx + fw - 2, fy + 2).unwrap() };
        std::thread::sleep(Duration::from_millis(50));
        capture.send(Command::Event(Event::PeerYield(PEER)));
        let _ = sent();
        windows_key(true);
        windows_key(false);
        std::thread::sleep(Duration::from_millis(1500));
        eprintln!(
            "[{close}] with Start open, at the middle: {}",
            window_at(mx, my, viewer.hwnd)
        );
        match close {
            "Escape" => {
                key(0x01, true);
                key(0x01, false);
            }
            "a click outside Start" => {
                unsafe { SetCursorPos(fx + 20, fy + 20).unwrap() };
                click(true);
                click(false);
            }
            _ => {
                if let Some(t) = taskbar {
                    unsafe {
                        SetCursorPos(t.left + (t.right - t.left) / 4, (t.top + t.bottom) / 2)
                            .unwrap()
                    };
                    click(true);
                    click(false);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1000));
        for (name, (px, py)) in [
            ("the middle", (mx, my)),
            ("top left", (fx + 40, fy + 40)),
            ("bottom middle", (fx + fw / 2, fy + fh - 80)),
        ] {
            eprintln!(
                "[{close}] with Start closed, at {name}: {}",
                window_at(px, py, viewer.hwnd)
            );
        }
        let _ = sent();
        unsafe { SetCursorPos(mx, my).unwrap() };
        mouse_move(3, 0);
        mouse_move(-3, 0);
        press();
        let got = sent();
        let line = format!(
            "[{close}]   moving onto the picture and typing: entered {}, clicks {}, keys {}",
            got.0, got.1, got.2
        );
        eprintln!("{line}");
        if got != (1, 0, 2) {
            failures.push(format!(
                "after closing Start with {close}: {line}, wanted (1, 0, 2)"
            ));
        }
        capture.send(Command::SetPortal(None));
        drop(viewer);
        std::thread::sleep(Duration::from_millis(300));
    }
    drop(capture);
    assert!(failures.is_empty(), "{failures:#?}");
}
