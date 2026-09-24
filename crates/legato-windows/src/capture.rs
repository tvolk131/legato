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

use legato_core::LocalInput;
use legato_core::controller::{Action, CaptureCommand, Controller, Event, Verdict};
use legato_core::keymap;
use legato_proto::{Button, Point, Rect, Scroll};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, VK_RSHIFT};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIDEV_INPUTSINK, RIM_TYPEMOUSE, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GA_ROOT,
    GetAncestor, GetClientRect, GetMessageW, HHOOK, HWND_TOPMOST, KBDLLHOOKSTRUCT, LLKHF_EXTENDED,
    LLKHF_INJECTED, LLMHF_INJECTED, LWA_ALPHA, MSG, MSLLHOOKSTRUCT, PostThreadMessageW,
    RegisterClassW, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetCursor, SetCursorPos,
    SetLayeredWindowAttributes, SetWindowPos, SetWindowsHookExW, ShowWindow, TranslateMessage,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE, WM_APP, WM_INPUT,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SYSKEYDOWN,
    WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WindowFromPoint, XBUTTON1,
};
use windows::core::w;

/// Sent to the capture thread from other threads.
pub type Command = CaptureCommand;

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
    /// Starts capturing. `sink` receives every controller action on the capture thread
    /// (`Capture` and `Release` are also carried out here); it must not block.
    /// `local_input` hears about the user's own input while it isn't being captured (for
    /// handing control back when this machine is being driven).
    pub fn start(
        controller: Controller,
        options: CaptureOptions,
        sink: impl FnMut(Action) + Send + 'static,
        local_input: impl FnMut(LocalInput) + Send + 'static,
    ) -> windows::core::Result<Self> {
        let (commands, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("legato-capture".into())
            .spawn(move || {
                run(
                    controller,
                    options,
                    Box::new(sink),
                    Box::new(local_input),
                    command_rx,
                    ready_tx,
                )
            })
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
    local_input: Box<dyn FnMut(LocalInput) + Send>,
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
    /// The user's left button is down.
    left_down: bool,
    /// The drop catcher is under the cursor.
    catching: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Side effects that must run outside the state borrow (they can pump messages).
enum Effect {
    Pin(POINT),
    Unpin(POINT),
    /// Put the drop catcher under the cursor, or take it away.
    Catch(POINT),
    StopCatching,
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
                    self.catching = false;
                    effects.push(Effect::Pin(pin));
                    (self.sink)(Action::Capture);
                }
                Action::Release { warp } => {
                    self.pin = None;
                    let to = clamp_to_displays(&self.controller, warp);
                    self.last_pos = Some(to);
                    effects.push(Effect::Unpin(to));
                    (self.sink)(Action::Release { warp });
                }
                other => (self.sink)(other),
            }
        }
        effects
    }

    fn on_mouse(&mut self, msg: u32, info: &MSLLHOOKSTRUCT) -> (Verdict, Vec<Effect>) {
        if info.dwExtraInfo == crate::INJECTED_TAG
            || (info.flags & LLMHF_INJECTED != 0 && !self.options.accept_injected)
        {
            return (Verdict::Pass, vec![]);
        }
        if self.pin.is_none() {
            let input = match (msg, self.last_pos) {
                (WM_MOUSEMOVE, Some(last)) => LocalInput::Motion {
                    dx: (info.pt.x - last.x) as f64,
                    dy: (info.pt.y - last.y) as f64,
                },
                (WM_MOUSEMOVE, None) => LocalInput::Motion { dx: 0.0, dy: 0.0 },
                _ => LocalInput::Other,
            };
            (self.local_input)(input);
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
                    if let Some(at) = self.portal_hit(pos) {
                        return self.handle(Event::PortalMotion { at });
                    }
                    let at = Point::new(pos.x as f64, pos.y as f64);
                    // Dragging against an edge that leads somewhere: offer to catch files.
                    let catch = self.left_down
                        && !self.controller.is_carrying()
                        && self.controller.neighbor_at_edge(at).is_some();
                    let (verdict, mut effects) =
                        self.handle(Event::LocalMotion { pos: at, attempted });
                    if catch && self.pin.is_none() {
                        self.catching = true;
                        effects.push(Effect::Catch(pos));
                    } else if self.catching && self.pin.is_none() && !catch {
                        self.catching = false;
                        effects.push(Effect::StopCatching);
                    }
                    return (verdict, effects);
                }
            },
            WM_LBUTTONDOWN => {
                self.left_down = true;
                button(Button::Left, true)
            }
            WM_LBUTTONUP => {
                self.left_down = false;
                let (verdict, mut effects) = self.handle(button(Button::Left, false));
                if self.catching {
                    self.catching = false;
                    effects.push(Effect::StopCatching);
                }
                return (verdict, effects);
            }
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
        if info.dwExtraInfo == crate::INJECTED_TAG
            || (info.flags.contains(LLKHF_INJECTED) && !self.options.accept_injected)
        {
            return (Verdict::Pass, vec![]);
        }
        if self.pin.is_none() {
            (self.local_input)(LocalInput::Other);
        }
        let Some(usage) = key_usage(info) else {
            return (Verdict::Pass, vec![]);
        };
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        self.handle(Event::Key { usage, down })
    }

    /// Where on the portal's picture `pos` is (0..1 each way), if it's over it and the
    /// portal window isn't covered there.
    fn portal_hit(&self, pos: POINT) -> Option<Point> {
        let portal = self.controller.portal()?;
        let window = HWND(portal.window as usize as *mut core::ffi::c_void);
        // SAFETY: window queries; a stale handle just fails them.
        unsafe {
            let under = WindowFromPoint(pos);
            if under.is_invalid() || GetAncestor(under, GA_ROOT) != window {
                return None;
            }
            let mut client = RECT::default();
            GetClientRect(window, &mut client).ok()?;
            let mut origin = POINT::default();
            if !ClientToScreen(window, &mut origin).as_bool() {
                return None;
            }
            let area = Rect::new(
                origin.x as f64,
                origin.y as f64,
                (client.right - client.left) as f64,
                (client.bottom - client.top) as f64,
            );
            let picture = legato_core::controller::fit_picture(
                (portal.remote.width, portal.remote.height),
                area,
            );
            let p = Point::new(pos.x as f64, pos.y as f64);
            picture.contains(p).then(|| {
                Point::new(
                    (p.x - picture.x) / picture.width,
                    (p.y - picture.y) / picture.height,
                )
            })
        }
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
            Command::SetConfig(config) => {
                self.controller.set_config(config);
                Some(vec![])
            }
            Command::SetPortal(portal) => {
                self.controller.set_portal(portal, &mut self.out);
                Some(self.drain())
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
                Effect::Catch(p) => {
                    let _ = SetWindowPos(
                        window,
                        Some(HWND_TOPMOST),
                        p.x - 24,
                        p.y - 24,
                        48,
                        48,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
                Effect::StopCatching => {
                    let _ = ShowWindow(window, SW_HIDE);
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
        // Hides the cursor while it's pinned under our window (not while catching drops).
        WM_SETCURSOR => {
            let pinned = STATE.with(|cell| {
                cell.try_borrow()
                    .map(|s| s.as_ref().is_some_and(|s| s.pin.is_some()))
                    .unwrap_or(true)
            });
            if pinned {
                // SAFETY: no preconditions.
                unsafe { SetCursor(None) };
                LRESULT(1)
            } else {
                // SAFETY: default handling with the unchanged arguments.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
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

fn ole_init() {
    // SAFETY: once per thread, before any OLE use; balanced by OleUninitialize.
    if let Err(e) = unsafe { windows::Win32::System::Ole::OleInitialize(None) } {
        tracing::warn!("OLE unavailable, so file drags can't cross: {e}");
    }
}

fn run(
    controller: Controller,
    options: CaptureOptions,
    sink: Box<dyn FnMut(Action) + Send>,
    local_input: Box<dyn FnMut(LocalInput) + Send>,
    commands: mpsc::Receiver<Command>,
    ready: mpsc::Sender<windows::core::Result<u32>>,
) {
    crate::init_dpi_awareness();
    ole_init();
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
            local_input,
            options,
            commands,
            out: Vec::new(),
            window,
            pin: None,
            last_pos: None,
            raw: (0, 0),
            raw_seen: false,
            left_down: false,
            catching: false,
        });
    });
    if let Err(e) = crate::drop::register(window) {
        tracing::warn!("file drags can't cross: {e}");
    }
    // SAFETY: no preconditions.
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));

    let mut msg = MSG::default();
    // SAFETY: standard message loop on this thread.
    'outer: while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        if msg.message == crate::drop::WM_FILES_ENTERED {
            if let Some(files) = crate::drop::ENTERED.with_borrow_mut(Option::take) {
                let effects = STATE.with_borrow_mut(|s| {
                    s.as_mut()
                        .map(|s| s.handle(Event::Carrying(Some(files))).1)
                        .unwrap_or_default()
                });
                apply(effects);
            }
            continue;
        }
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
    crate::drop::unregister(window);
    // SAFETY: tearing down what `setup` created.
    unsafe {
        let _ = UnhookWindowsHookEx(keyboard);
        let _ = UnhookWindowsHookEx(mouse);
        let _ = DestroyWindow(window);
        windows::Win32::System::Ole::OleUninitialize();
    }
}
