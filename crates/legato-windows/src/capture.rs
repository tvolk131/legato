//! Capturing the local keyboard and mouse with low-level hooks.
//!
//! A dedicated thread installs `WH_MOUSE_LL` and `WH_KEYBOARD_LL` hooks and runs the
//! [`Controller`] inside the hook callbacks, which must decide synchronously whether each
//! event reaches the rest of the system. Everything in the callbacks is quick: network
//! sends don't block. (Windows silently removes hooks that take longer than ~1 s.)
//!
//! While the cursor is on another machine, the local cursor is pinned in the middle of the
//! primary display under a tiny, invisible, never-activated window that hides it, and each
//! swallowed mouse move is reported as its offset from the pin.

use std::cell::RefCell;
use std::sync::mpsc;
use std::time::Instant;

use legato_core::controller::{Action, Controller, Event, Verdict};
use legato_core::keymap;
use legato_core::{KeyRemap, Layout, MachineId};
use legato_proto::{Button, Point, Rect, Scroll};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, VK_RSHIFT};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    HHOOK, HWND_TOPMOST, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED,
    LWA_ALPHA, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, RegisterClassW, SW_HIDE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetCursor, SetCursorPos, SetLayeredWindowAttributes, SetWindowPos,
    SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WINDOW_EX_STYLE, WM_APP, WM_INPUT, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETCURSOR, WM_SYSKEYDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, XBUTTON1,
};
use windows::core::w;

/// Sent to the capture thread from other threads.
#[derive(Debug)]
pub enum Command {
    /// A peer yielded or disconnected.
    Event(Event),
    SetLayout(Layout),
    SetRemap(MachineId, KeyRemap),
    Stop,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureOptions {
    /// Treat injected input (e.g. from `SendInput`) like real input. For tests.
    pub accept_injected: bool,
}

/// A running capture thread. Stops when dropped.
pub struct Capture {
    thread_id: u32,
    commands: mpsc::Sender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    /// Starts capturing. `sink` receives the controller's network actions (`Send` and
    /// `Datagram`) on the capture thread; it must not block.
    pub fn start(
        controller: Controller,
        options: CaptureOptions,
        sink: impl FnMut(Action) + Send + 'static,
    ) -> windows::core::Result<Self> {
        let (commands, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("legato-capture".into())
            .spawn(move || run(controller, options, Box::new(sink), command_rx, ready_tx))
            .expect("spawning the capture thread");
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                commands,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(windows::core::Error::from_thread()),
        }
    }

    pub fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            // SAFETY: plain message post to our own thread's queue.
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_APP, WPARAM(0), LPARAM(0)) };
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct State {
    controller: Controller,
    sink: Box<dyn FnMut(Action) + Send>,
    options: CaptureOptions,
    commands: mpsc::Receiver<Command>,
    out: Vec<Action>,
    window: HWND,
    /// Where the cursor is pinned while captured.
    pin: Option<POINT>,
    last_pos: Option<POINT>,
    /// Raw device motion since the last mouse hook event.
    raw: (i32, i32),
    raw_seen: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Side effects that must run outside the state borrow (they can pump messages).
enum Effect {
    Pin(POINT),
    Unpin(POINT),
}

impl State {
    fn handle(&mut self, event: Event) -> (Verdict, Vec<Effect>) {
        let verdict = self.controller.handle(Instant::now(), event, &mut self.out);
        (verdict, self.drain())
    }

    fn drain(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        for action in self.out.drain(..) {
            match action {
                Action::Capture => {
                    let pin = pin_point(&self.controller);
                    self.pin = Some(pin);
                    effects.push(Effect::Pin(pin));
                }
                Action::Release { warp } => {
                    self.pin = None;
                    let to = clamp_to_displays(&self.controller, warp);
                    self.last_pos = Some(to);
                    effects.push(Effect::Unpin(to));
                }
                other => (self.sink)(other),
            }
        }
        effects
    }

    fn on_mouse(&mut self, msg: u32, info: &MSLLHOOKSTRUCT) -> (Verdict, Vec<Effect>) {
        if info.flags & LLMHF_INJECTED != 0 && !self.options.accept_injected {
            return (Verdict::Pass, vec![]);
        }
        let pos = info.pt;
        let wheel = ((info.mouseData >> 16) as u16 as i16) as f64;
        let x_button = if (info.mouseData >> 16) as u16 == XBUTTON1 {
            Button::Back
        } else {
            Button::Forward
        };
        let event = match msg {
            WM_MOUSEMOVE => match self.pin {
                Some(pin) => {
                    let delta = Point::new((pos.x - pin.x) as f64, (pos.y - pin.y) as f64);
                    if delta.x == 0.0 && delta.y == 0.0 {
                        return (Verdict::Swallow, vec![]);
                    }
                    Event::CapturedMotion { delta }
                }
                None => {
                    // Prefer raw device motion: it keeps coming when the cursor is stuck
                    // against a screen edge, which is what push-through measures.
                    let attempted = if self.raw_seen {
                        Point::new(self.raw.0 as f64, self.raw.1 as f64)
                    } else {
                        self.last_pos.map_or(Point::default(), |last| {
                            Point::new((pos.x - last.x) as f64, (pos.y - last.y) as f64)
                        })
                    };
                    self.raw = (0, 0);
                    self.last_pos = Some(pos);
                    Event::LocalMotion {
                        pos: Point::new(pos.x as f64, pos.y as f64),
                        attempted,
                    }
                }
            },
            WM_LBUTTONDOWN => button(Button::Left, true),
            WM_LBUTTONUP => button(Button::Left, false),
            WM_RBUTTONDOWN => button(Button::Right, true),
            WM_RBUTTONUP => button(Button::Right, false),
            WM_MBUTTONDOWN => button(Button::Middle, true),
            WM_MBUTTONUP => button(Button::Middle, false),
            WM_XBUTTONDOWN => button(x_button, true),
            WM_XBUTTONUP => button(x_button, false),
            WM_MOUSEWHEEL => Event::Scroll(Scroll::Wheel { x: 0.0, y: wheel }),
            WM_MOUSEHWHEEL => Event::Scroll(Scroll::Wheel { x: wheel, y: 0.0 }),
            _ => return (Verdict::Pass, vec![]),
        };
        self.handle(event)
    }

    fn on_key(&mut self, msg: u32, info: &KBDLLHOOKSTRUCT) -> (Verdict, Vec<Effect>) {
        if info.flags.contains(LLKHF_INJECTED) && !self.options.accept_injected {
            return (Verdict::Pass, vec![]);
        }
        let Some(usage) = key_usage(info) else {
            return (Verdict::Pass, vec![]);
        };
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        self.handle(Event::Key { usage, down })
    }

    fn on_command(&mut self, command: Command) -> Option<Vec<Effect>> {
        match command {
            Command::Event(event) => Some(self.handle(event).1),
            Command::SetLayout(layout) => {
                self.controller.set_layout(layout, &mut self.out);
                Some(self.drain())
            }
            Command::SetRemap(peer, remap) => {
                self.controller.set_remap(peer, remap);
                Some(vec![])
            }
            Command::Stop => None,
        }
    }
}

fn button(button: Button, down: bool) -> Event {
    Event::Button { button, down }
}

/// HID usage for a low-level keyboard event.
fn key_usage(info: &KBDLLHOOKSTRUCT) -> Option<u16> {
    let mut scancode = info.scanCode as u16;
    if scancode == 0 {
        // Some keys (and some keyboards) report only a virtual key.
        // SAFETY: pure lookup.
        scancode = unsafe { MapVirtualKeyW(info.vkCode, MAPVK_VK_TO_VSC_EX) } as u16;
    } else if info.flags.contains(LLKHF_EXTENDED) {
        scancode |= 0xe000;
    }
    // Right Shift arrives flagged as extended, but its scancode isn't.
    if info.vkCode == u32::from(VK_RSHIFT.0) {
        scancode = 0x36;
    }
    keymap::hid_from_windows_scancode(scancode)
}

/// The middle of the primary display, in physical pixels.
fn pin_point(controller: &Controller) -> POINT {
    let displays = &controller.layout().local().screens.displays;
    let bounds = displays
        .iter()
        .find(|d| d.primary)
        .or(displays.first())
        .map_or(Rect::new(0.0, 0.0, 2.0, 2.0), |d| d.bounds);
    let c = bounds.center();
    POINT {
        x: c.x as i32,
        y: c.y as i32,
    }
}

/// Rounds a native point onto a real pixel of the nearest display.
fn clamp_to_displays(controller: &Controller, p: Point) -> POINT {
    let displays = &controller.layout().local().screens.displays;
    let best = displays
        .iter()
        .map(|d| {
            let b = d.bounds;
            let x = p.x.clamp(b.left(), b.right() - 1.0);
            let y = p.y.clamp(b.top(), b.bottom() - 1.0);
            ((x - p.x).powi(2) + (y - p.y).powi(2), x, y)
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));
    let (x, y) = best.map_or((p.x, p.y), |(_, x, y)| (x, y));
    POINT {
        x: x.round() as i32,
        y: y.round() as i32,
    }
}

fn apply(effects: Vec<Effect>) {
    let window = STATE.with_borrow(|s| s.as_ref().map(|s| s.window));
    let Some(window) = window else { return };
    for effect in effects {
        // SAFETY: plain window and cursor calls on our own window.
        unsafe {
            match effect {
                Effect::Pin(p) => {
                    let _ = SetWindowPos(
                        window,
                        Some(HWND_TOPMOST),
                        p.x - 32,
                        p.y - 32,
                        64,
                        64,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    let _ = SetCursorPos(p.x, p.y);
                }
                Effect::Unpin(p) => {
                    let _ = ShowWindow(window, SW_HIDE);
                    let _ = SetCursorPos(p.x, p.y);
                }
            }
        }
    }
}

/// Runs `f` on the state unless it's already borrowed (a re-entrant callback), in which
/// case the event passes through untouched.
fn with_state(f: impl FnOnce(&mut State) -> (Verdict, Vec<Effect>)) -> Verdict {
    let result = STATE.with(|cell| match cell.try_borrow_mut() {
        Ok(mut state) => state.as_mut().map(f),
        Err(_) => None,
    });
    match result {
        Some((verdict, effects)) => {
            apply(effects);
            verdict
        }
        None => Verdict::Pass,
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_MOUSE_LL with code >= 0, lparam points to an MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if with_state(|s| s.on_mouse(wparam.0 as u32, info)) == Verdict::Swallow {
            return LRESULT(1);
        }
    }
    // SAFETY: forwarding the unchanged hook arguments.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if with_state(|s| s.on_key(wparam.0 as u32, info)) == Verdict::Swallow {
            return LRESULT(1);
        }
    }
    // SAFETY: forwarding the unchanged hook arguments.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Hides the cursor while it's pinned under our window.
        WM_SETCURSOR => {
            // SAFETY: no preconditions.
            unsafe { SetCursor(None) };
            LRESULT(1)
        }
        WM_INPUT => {
            if let Some((dx, dy)) = read_raw_motion(HRAWINPUT(lparam.0 as *mut _)) {
                STATE.with(|cell| {
                    if let Ok(mut state) = cell.try_borrow_mut()
                        && let Some(state) = state.as_mut()
                    {
                        state.raw.0 += dx;
                        state.raw.1 += dy;
                        state.raw_seen = true;
                    }
                });
            }
            // SAFETY: WM_INPUT must still be passed on for cleanup.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        // SAFETY: default handling with the unchanged arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn read_raw_motion(handle: HRAWINPUT) -> Option<(i32, i32)> {
    let mut data = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    // SAFETY: `data` is a buffer of `size` bytes.
    let read = unsafe {
        GetRawInputData(
            handle,
            RID_INPUT,
            Some((&raw mut data).cast()),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if read == u32::MAX || data.header.dwType != RIM_TYPEMOUSE.0 {
        return None;
    }
    // SAFETY: dwType says this is the mouse variant.
    let mouse = unsafe { data.data.mouse };
    if mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 != 0 {
        return None; // tablets, remote desktop: no relative motion
    }
    Some((mouse.lLastX, mouse.lLastY))
}

fn run(
    controller: Controller,
    options: CaptureOptions,
    sink: Box<dyn FnMut(Action) + Send>,
    commands: mpsc::Receiver<Command>,
    ready: mpsc::Sender<windows::core::Result<u32>>,
) {
    crate::init_dpi_awareness();
    // SAFETY: standard window-class registration and creation for this thread.
    let setup = unsafe {
        (|| -> windows::core::Result<(HWND, HHOOK, HHOOK)> {
            let instance: HINSTANCE = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: w!("LegatoCapture"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let window = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WINDOW_EX_STYLE::default(),
                w!("LegatoCapture"),
                w!("Legato"),
                WS_POPUP,
                0,
                0,
                64,
                64,
                None,
                None,
                Some(instance),
                None,
            )?;
            // Alpha 1, not 0: fully transparent windows don't get mouse messages, and it's
            // WM_SETCURSOR on this window that hides the cursor.
            SetLayeredWindowAttributes(window, COLORREF(0), 1, LWA_ALPHA)?;
            RegisterRawInputDevices(
                &[RAWINPUTDEVICE {
                    usUsagePage: 0x01, // generic desktop
                    usUsage: 0x02,     // mouse
                    dwFlags: RIDEV_INPUTSINK,
                    hwndTarget: window,
                }],
                size_of::<RAWINPUTDEVICE>() as u32,
            )?;
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(instance), 0)?;
            let keyboard =
                match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(instance), 0) {
                    Ok(hook) => hook,
                    Err(e) => {
                        let _ = UnhookWindowsHookEx(mouse);
                        return Err(e);
                    }
                };
            Ok((window, mouse, keyboard))
        })()
    };
    let (window, mouse, keyboard) = match setup {
        Ok(handles) => handles,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    STATE.with_borrow_mut(|s| {
        *s = Some(State {
            controller,
            sink,
            options,
            commands,
            out: Vec::new(),
            window,
            pin: None,
            last_pos: None,
            raw: (0, 0),
            raw_seen: false,
        });
    });
    // SAFETY: no preconditions.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));

    let mut msg = MSG::default();
    // SAFETY: standard message loop on this thread.
    'outer: while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        if msg.message == WM_APP {
            loop {
                let next = STATE.with_borrow_mut(|s| {
                    let s = s.as_mut()?;
                    let command = s.commands.try_recv().ok()?;
                    Some(s.on_command(command))
                });
                match next {
                    None => break,
                    Some(None) => break 'outer,
                    Some(Some(effects)) => apply(effects),
                }
            }
            continue;
        }
        // SAFETY: dispatching a message we just received.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Don't leave the cursor pinned and hidden.
    if let Some(pin) = STATE.with_borrow(|s| s.as_ref().and_then(|s| s.pin)) {
        apply(vec![Effect::Unpin(pin)]);
    }
    STATE.with_borrow_mut(|s| *s = None);
    // SAFETY: tearing down what `setup` created.
    unsafe {
        let _ = UnhookWindowsHookEx(keyboard);
        let _ = UnhookWindowsHookEx(mouse);
        let _ = DestroyWindow(window);
    }
}
