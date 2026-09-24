//! Noticing the user's own trackpad, mouse and keyboard, so a remote controller can hand
//! control back.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFMachPort, CFRetained, CFRunLoop, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};

use crate::INJECTED_TAG;

/// Pointer travel (points) that counts as deliberate movement rather than a brushed
/// trackpad.
const MOVE_THRESHOLD: f64 = 12.0;
/// Movement pauses longer than this start the count over.
const MOVE_WINDOW: Duration = Duration::from_millis(300);

/// Watches for physical (not injected) input with a listen-only event tap, and calls a
/// callback when the user deliberately uses this machine's own keyboard or pointer.
pub struct ActivityMonitor {
    run_loop: SendRunLoop,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct SendRunLoop(CFRetained<CFRunLoop>);
// SAFETY: CFRunLoopStop may be called from any thread.
unsafe impl Send for SendRunLoop {}

struct TapState {
    port: Option<CFRetained<CFMachPort>>,
    on_activity: Box<dyn FnMut() + Send>,
    moved: f64,
    last_move: Option<Instant>,
}

impl ActivityMonitor {
    /// Starts the monitor on its own thread. Fails if the tap can't be created, which
    /// usually means Accessibility access hasn't been granted.
    pub fn start(on_activity: impl FnMut() + Send + 'static) -> Result<Self, &'static str> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let on_activity: Box<dyn FnMut() + Send> = Box::new(on_activity);
        let thread = std::thread::Builder::new()
            .name("legato-activity".into())
            .spawn(move || run_tap(on_activity, ready_tx))
            .map_err(|_| "could not start the activity thread")?;
        match ready_rx.recv() {
            Ok(Ok(run_loop)) => Ok(Self {
                run_loop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("the activity thread exited"),
        }
    }
}

impl Drop for ActivityMonitor {
    fn drop(&mut self) {
        self.run_loop.0.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn mask(types: &[CGEventType]) -> CGEventMask {
    types.iter().fold(0, |m, t| m | (1u64 << t.0))
}

fn run_tap(
    on_activity: Box<dyn FnMut() + Send>,
    ready: mpsc::Sender<Result<SendRunLoop, &'static str>>,
) {
    let state = Box::into_raw(Box::new(TapState {
        port: None,
        on_activity,
        moved: 0.0,
        last_move: None,
    }));
    let events = mask(&[
        CGEventType::LeftMouseDown,
        CGEventType::RightMouseDown,
        CGEventType::OtherMouseDown,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
        CGEventType::KeyDown,
        CGEventType::FlagsChanged,
        CGEventType::ScrollWheel,
    ]);
    // SAFETY: the callback matches the required signature and `state` stays valid until
    // after the run loop (the only caller of the callback) has exited.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            events,
            Some(callback),
            state.cast(),
        )
    };
    let Some(port) = port else {
        // SAFETY: the tap was never created, so nothing else references `state`.
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err(
            "could not watch input: grant Accessibility access in System Settings",
        ));
        return;
    };
    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err("could not create a run loop source"));
        return;
    };
    // SAFETY: only this thread touches `state` from here on (the callback runs here too).
    unsafe { (*state).port = Some(port.clone()) };
    let Some(run_loop) = CFRunLoop::current() else {
        drop(unsafe { Box::from_raw(state) });
        let _ = ready.send(Err("no run loop"));
        return;
    };
    // SAFETY: the mode constant is a valid static CFString.
    run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    CGEvent::tap_enable(&port, true);
    let _ = ready.send(Ok(SendRunLoop(run_loop)));

    CFRunLoop::run();

    CGEvent::tap_enable(&port, false);
    // SAFETY: the run loop has exited, so the callback can no longer run.
    drop(unsafe { Box::from_raw(state) });
}

unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    ty: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `user_info` is the `TapState` owned by the tap thread, which is the thread
    // this callback runs on.
    let state = unsafe { &mut *user_info.cast::<TapState>() };
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

    let deliberate = if ty == CGEventType::MouseMoved
        || ty == CGEventType::LeftMouseDragged
        || ty == CGEventType::RightMouseDragged
        || ty == CGEventType::OtherMouseDragged
    {
        let now = Instant::now();
        if state
            .last_move
            .is_none_or(|t| now.duration_since(t) > MOVE_WINDOW)
        {
            state.moved = 0.0;
        }
        state.last_move = Some(now);
        let dx = CGEvent::integer_value_field(Some(ev), CGEventField::MouseEventDeltaX) as f64;
        let dy = CGEvent::integer_value_field(Some(ev), CGEventField::MouseEventDeltaY) as f64;
        state.moved += dx.abs() + dy.abs();
        if state.moved >= MOVE_THRESHOLD {
            state.moved = 0.0;
            true
        } else {
            false
        }
    } else {
        true
    };
    if deliberate {
        (state.on_activity)();
    }
    event.as_ptr()
}
