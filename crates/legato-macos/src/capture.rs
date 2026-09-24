//! Capturing the Mac's own keyboard and mouse with an active event tap, so it can drive
//! other machines.
//!
//! The tap runs the [`Controller`] synchronously for every event and drops the ones it
//! swallows. While the cursor is on another machine, the Mac's cursor is hidden and held
//! in the middle of the main display; motion comes from the events' delta fields.

use std::ffi::{CStr, c_void};
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Instant;

use legato_core::LocalInput;
use legato_core::controller::{Action, CaptureCommand, Controller, Event, Verdict};
use legato_core::keymap::{self, usage};
use legato_proto::{Button, Point, Scroll};
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFMachPort, CFRetained, CFRunLoop, CFRunLoopTimer,
    CFRunLoopTimerContext, CGPoint, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayHideCursor, CGDisplayShowCursor, CGEvent, CGEventField, CGEventMask,
    CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType,
    CGMainDisplayID, CGWarpMouseCursorPosition,
};

use crate::INJECTED_TAG;

/// A running capture tap. Stops (and shows the cursor again) when dropped.
pub struct Capture {
    commands: mpsc::Sender<CaptureCommand>,
    run_loop: SendRunLoop,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct SendRunLoop(CFRetained<CFRunLoop>);
// SAFETY: we only call CFRunLoopStop and CFRunLoopWakeUp on it, which are thread-safe.
unsafe impl Send for SendRunLoop {}
unsafe impl Sync for SendRunLoop {}

impl Capture {
    /// Starts capturing on its own thread. `sink` receives every controller action (the
    /// cursor side of `Capture` and `Release` is also handled here); it must not block.
    /// `local_input` hears about the user's own input while it isn't being captured.
    pub fn start(
        controller: Controller,
        sink: impl FnMut(Action) + Send + 'static,
        local_input: impl FnMut(LocalInput) + Send + 'static,
    ) -> Result<Self, &'static str> {
        let (commands, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let sink: Box<dyn FnMut(Action) + Send> = Box::new(sink);
        let local_input: Box<dyn FnMut(LocalInput) + Send> = Box::new(local_input);
        let thread = std::thread::Builder::new()
            .name("legato-capture".into())
            .spawn(move || {
                // Built on the tap thread: it holds CF objects that must stay there.
                let state = TapState {
                    controller,
                    sink,
                    local_input,
                    commands: command_rx,
                    out: Vec::new(),
                    port: None,
                    pin: None,
                    hidden: false,
                    flags: 0,
                };
                run(state, ready_tx)
            })
            .map_err(|_| "could not start the capture thread")?;
        match ready_rx.recv() {
            Ok(Ok(run_loop)) => Ok(Self {
                commands,
                run_loop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("the capture thread exited"),
        }
    }

    pub fn send(&self, command: CaptureCommand) {
        if self.commands.send(command).is_ok() {
            self.run_loop.0.wake_up();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.send(CaptureCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct TapState {
    controller: Controller,
    sink: Box<dyn FnMut(Action) + Send>,
    local_input: Box<dyn FnMut(LocalInput) + Send>,
    commands: mpsc::Receiver<CaptureCommand>,
    out: Vec<Action>,
    port: Option<CFRetained<CFMachPort>>,
    /// Where the cursor is held while captured.
    pin: Option<CGPoint>,
    hidden: bool,
    /// Last seen modifier flags, to tell presses from releases.
    flags: u64,
}

impl TapState {
    fn handle(&mut self, event: Event) -> Verdict {
        let verdict = self.controller.handle(Instant::now(), event, &mut self.out);
        self.apply();
        verdict
    }

    fn apply(&mut self) {
        let actions = std::mem::take(&mut self.out);
        for action in actions {
            match &action {
                Action::Capture => {
                    let b = CGDisplayBounds(CGMainDisplayID());
                    let pin = CGPoint::new(
                        b.origin.x + b.size.width / 2.0,
                        b.origin.y + b.size.height / 2.0,
                    );
                    self.pin = Some(pin);
                    warp(pin);
                    self.set_hidden(true);
                }
                Action::Release { warp } => {
                    self.pin = None;
                    self::warp(CGPoint::new(warp.x, warp.y));
                    self.set_hidden(false);
                }
                _ => {}
            }
            (self.sink)(action);
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        if hidden == self.hidden {
            return;
        }
        self.hidden = hidden;
        if hidden {
            allow_hiding_cursor_in_background();
            let _ = CGDisplayHideCursor(CGMainDisplayID());
        } else {
            let _ = CGDisplayShowCursor(CGMainDisplayID());
        }
    }

    /// Returns false to stop.
    fn drain_commands(&mut self) -> bool {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                CaptureCommand::Event(event) => {
                    self.handle(event);
                }
                CaptureCommand::SetLayout(layout) => {
                    self.controller.set_layout(layout, &mut self.out);
                    self.apply();
                }
                CaptureCommand::SetRemap(peer, remap) => self.controller.set_remap(peer, remap),
                CaptureCommand::SetConfig(config) => self.controller.set_config(config),
                CaptureCommand::Stop => return false,
            }
        }
        true
    }

    fn on_event(&mut self, ty: CGEventType, ev: &CGEvent) -> Verdict {
        let int = |field| CGEvent::integer_value_field(Some(ev), field);
        let captured = self.pin.is_some();
        match ty {
            t if t == CGEventType::MouseMoved
                || t == CGEventType::LeftMouseDragged
                || t == CGEventType::RightMouseDragged
                || t == CGEventType::OtherMouseDragged =>
            {
                let (dx, dy) = (
                    int(CGEventField::MouseEventDeltaX) as f64,
                    int(CGEventField::MouseEventDeltaY) as f64,
                );
                if self.pin.is_some() {
                    // Dropped at the HID tap, so the hidden cursor doesn't move.
                    self.handle(Event::CapturedMotion {
                        delta: Point::new(dx, dy),
                    })
                } else {
                    (self.local_input)(LocalInput::Motion { dx, dy });
                    let loc = CGEvent::location(Some(ev));
                    self.handle(Event::LocalMotion {
                        pos: Point::new(loc.x, loc.y),
                        attempted: Point::new(dx, dy),
                    })
                }
            }
            t if t == CGEventType::LeftMouseDown || t == CGEventType::LeftMouseUp => {
                self.button(captured, Button::Left, t == CGEventType::LeftMouseDown)
            }
            t if t == CGEventType::RightMouseDown || t == CGEventType::RightMouseUp => {
                self.button(captured, Button::Right, t == CGEventType::RightMouseDown)
            }
            t if t == CGEventType::OtherMouseDown || t == CGEventType::OtherMouseUp => {
                let button = match int(CGEventField::MouseEventButtonNumber) {
                    3 => Button::Back,
                    4 => Button::Forward,
                    _ => Button::Middle,
                };
                self.button(captured, button, t == CGEventType::OtherMouseDown)
            }
            t if t == CGEventType::KeyDown || t == CGEventType::KeyUp => {
                let Some(hid) =
                    keymap::hid_from_mac_keycode(int(CGEventField::KeyboardEventKeycode) as u16)
                else {
                    return Verdict::Pass;
                };
                if !captured {
                    (self.local_input)(LocalInput::Other);
                }
                self.handle(Event::Key {
                    usage: hid,
                    down: t == CGEventType::KeyDown,
                })
            }
            t if t == CGEventType::FlagsChanged => self.flags_changed(captured, ev),
            t if t == CGEventType::ScrollWheel => {
                if !captured {
                    (self.local_input)(LocalInput::Other);
                }
                let continuous = int(CGEventField::ScrollWheelEventIsContinuous) != 0;
                let scroll = if continuous {
                    Scroll::Pixels {
                        x: int(CGEventField::ScrollWheelEventPointDeltaAxis2) as f64,
                        y: int(CGEventField::ScrollWheelEventPointDeltaAxis1) as f64,
                    }
                } else {
                    Scroll::Wheel {
                        x: -(int(CGEventField::ScrollWheelEventDeltaAxis2) as f64) * 120.0,
                        y: int(CGEventField::ScrollWheelEventDeltaAxis1) as f64 * 120.0,
                    }
                };
                self.handle(Event::Scroll(scroll))
            }
            _ => Verdict::Pass,
        }
    }

    fn button(&mut self, captured: bool, button: Button, down: bool) -> Verdict {
        if !captured {
            (self.local_input)(LocalInput::Other);
        }
        self.handle(Event::Button { button, down })
    }

    /// Modifier keys arrive as flag changes; work out which key went down or up.
    fn flags_changed(&mut self, captured: bool, ev: &CGEvent) -> Verdict {
        let keycode =
            CGEvent::integer_value_field(Some(ev), CGEventField::KeyboardEventKeycode) as u16;
        let flags = CGEvent::flags(Some(ev)).0;
        let previous = std::mem::replace(&mut self.flags, flags);
        if !captured {
            (self.local_input)(LocalInput::Other);
        }
        if keycode == CAPS_LOCK_KEYCODE {
            // Caps Lock reports each press as a toggle: forward a full press.
            self.handle(Event::Key {
                usage: usage::CAPS_LOCK,
                down: true,
            });
            return self.handle(Event::Key {
                usage: usage::CAPS_LOCK,
                down: false,
            });
        }
        let Some((hid, bit)) = modifier(keycode) else {
            return Verdict::Pass;
        };
        let down = flags & bit != 0;
        if down == (previous & bit != 0) {
            return Verdict::Pass;
        }
        self.handle(Event::Key { usage: hid, down })
    }
}

const CAPS_LOCK_KEYCODE: u16 = 57;

/// Moves the cursor without the default quarter-second freeze of the user's own input
/// that normally follows a warp.
fn warp(to: CGPoint) {
    use objc2_core_graphics::{CGEventSource, CGEventSourceStateID};
    if let Some(source) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) {
        CGEventSource::set_local_events_suppression_interval(Some(&source), 0.0);
    }
    let _ = CGWarpMouseCursorPosition(to);
}

/// HID usage and device-specific flag bit (IOKit's NX_DEVICE*KEYMASK) for a modifier key.
fn modifier(keycode: u16) -> Option<(u16, u64)> {
    Some(match keycode {
        59 => (usage::LEFT_CTRL, 0x0001),
        56 => (usage::LEFT_SHIFT, 0x0002),
        60 => (usage::RIGHT_SHIFT, 0x0004),
        55 => (usage::LEFT_GUI, 0x0008),
        54 => (usage::RIGHT_GUI, 0x0010),
        58 => (usage::LEFT_ALT, 0x0020),
        61 => (usage::RIGHT_ALT, 0x0040),
        62 => (usage::RIGHT_CTRL, 0x2000),
        _ => return None,
    })
}

/// Lets a background app hide the cursor (a private window-server connection property
/// also used by Barrier and lan-mouse). Looked up at runtime so a missing symbol only
/// means the cursor stays visible.
fn allow_hiding_cursor_in_background() {
    type DefaultConnection = unsafe extern "C" fn() -> i32;
    type SetProperty = unsafe extern "C" fn(i32, i32, *const c_void, *const c_void) -> i32;
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
        static kCFBooleanTrue: *const c_void;
    }
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    let lookup = |name: &CStr| {
        // SAFETY: dlsym with a valid C string; RTLD_DEFAULT searches loaded images.
        unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) }
    };
    let connection = lookup(c"_CGSDefaultConnection");
    let set = lookup(c"CGSSetConnectionProperty");
    if connection.is_null() || set.is_null() {
        tracing::debug!("can't hide the cursor in the background on this macOS");
        return;
    }
    let key = objc2_core_foundation::CFString::from_static_str("SetsCursorInBackground");
    // SAFETY: the symbols have these signatures; the key and value are valid CF objects.
    unsafe {
        let connection: DefaultConnection = std::mem::transmute(connection);
        let set: SetProperty = std::mem::transmute(set);
        let cid = connection();
        set(
            cid,
            cid,
            CFRetained::as_ptr(&key).as_ptr().cast(),
            kCFBooleanTrue,
        );
    }
}

fn mask(types: &[CGEventType]) -> CGEventMask {
    types.iter().fold(0, |m, t| m | (1u64 << t.0))
}

fn run(state: TapState, ready: mpsc::Sender<Result<SendRunLoop, &'static str>>) {
    let state = Box::into_raw(Box::new(state));
    let events = mask(&[
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
        CGEventType::ScrollWheel,
    ]);
    // SAFETY: `state` outlives the tap: it's freed after the run loop exits.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            events,
            Some(tap_callback),
            state.cast(),
        )
    };
    let Some(port) = port else {
        // SAFETY: never shared.
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err(
            "could not capture input: grant Accessibility access in System Settings",
        ));
        return;
    };
    // SAFETY: only this thread touches `state` (the callbacks run here too).
    unsafe { (*state).port = Some(port.clone()) };
    let (Some(source), Some(run_loop)) = (
        CFMachPort::new_run_loop_source(None, Some(&port), 0),
        CFRunLoop::current(),
    ) else {
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err("could not attach the event tap"));
        return;
    };
    // Commands (layout changes, yields) are picked up by a short timer, and by every event.
    let mut context = CFRunLoopTimerContext {
        version: 0,
        info: state.cast(),
        retain: None,
        release: None,
        copyDescription: None,
    };
    // SAFETY: the context is copied by CFRunLoopTimerCreate; `state` outlives the timer.
    let timer = unsafe {
        CFRunLoopTimer::new(
            None,
            CFAbsoluteTimeGetCurrent() + 0.02,
            0.02,
            0,
            0,
            Some(timer_callback),
            &mut context,
        )
    };
    // SAFETY: the mode constant is a valid static CFString.
    let modes = unsafe { kCFRunLoopCommonModes };
    run_loop.add_source(Some(&source), modes);
    if let Some(timer) = &timer {
        run_loop.add_timer(Some(timer), modes);
    }
    CGEvent::tap_enable(&port, true);
    let _ = ready.send(Ok(SendRunLoop(run_loop)));

    CFRunLoop::run();

    CGEvent::tap_enable(&port, false);
    if let Some(timer) = timer {
        timer.invalidate();
    }
    // SAFETY: the run loop has exited, so no callback can run any more.
    let mut state = unsafe { Box::from_raw(state) };
    state.set_hidden(false);
}

unsafe extern "C-unwind" fn timer_callback(_timer: *mut CFRunLoopTimer, info: *mut c_void) {
    // SAFETY: `info` is the TapState, used only on this thread.
    let state = unsafe { &mut *info.cast::<TapState>() };
    if !state.drain_commands() {
        state.set_hidden(false);
        if let Some(rl) = CFRunLoop::current() {
            rl.stop();
        }
    }
}

unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    ty: CGEventType,
    event: NonNull<CGEvent>,
    info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `info` is the TapState, used only on this thread.
    let state = unsafe { &mut *info.cast::<TapState>() };
    // SAFETY: Quartz passes a valid event for the duration of the callback.
    let ev = unsafe { event.as_ref() };
    if ty == CGEventType::TapDisabledByTimeout || ty == CGEventType::TapDisabledByUserInput {
        if let Some(port) = &state.port {
            CGEvent::tap_enable(port, true);
        }
        return event.as_ptr();
    }
    if CGEvent::integer_value_field(Some(ev), CGEventField::EventSourceUserData) == INJECTED_TAG {
        return event.as_ptr();
    }
    if !state.drain_commands() {
        if let Some(rl) = CFRunLoop::current() {
            rl.stop();
        }
        return event.as_ptr();
    }
    match state.on_event(ty, ev) {
        Verdict::Pass => event.as_ptr(),
        Verdict::Swallow => std::ptr::null_mut(),
    }
}
