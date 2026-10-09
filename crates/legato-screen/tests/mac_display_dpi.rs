//! Real virtual-display mode selection, including a remembered low-DPI choice.
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    if !std::env::args().any(|arg| arg == "--ignored") {
        eprintln!("virtual display regression ignored: pass --ignored to create displays");
        return;
    }
    if std::env::var_os("LEGATO_DISPLAY_TEST_STEP").is_none() {
        mac::run();
        return;
    }
    // Mode selection changes the session configuration. AppKit must process its
    // notifications on the main thread so subsequent CoreGraphics queries are fresh,
    // as in the app. A plain libtest worker can retain the old mode list.
    let app =
        objc2_app_kit::NSApplication::sharedApplication(objc2::MainThreadMarker::new().unwrap());
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Prohibited);
    std::thread::spawn(|| {
        let result = std::panic::catch_unwind(mac::run);
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    });
    app.run();
}

#[cfg(target_os = "macos")]
mod mac {

    use legato_screen::mac::{Mode, VirtualDisplay};
    use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString};
    use objc2_core_graphics::{
        CGDisplayCopyAllDisplayModes, CGDisplayMode, CGDisplaySetDisplayMode,
        kCGDisplayShowDuplicateLowResolutionModes,
    };

    const PLAIN: Mode = Mode {
        width: 1920,
        height: 1080,
        hidpi: false,
        refresh: 144.0,
    };
    const RETINA: Mode = Mode {
        width: 3840,
        height: 2160,
        hidpi: true,
        refresh: 60.0,
    };

    fn check(display: &VirtualDisplay, mode: Mode) {
        let scale = if mode.hidpi { 2 } else { 1 };
        eprintln!(
            "requested {mode:?}: {:?} pixels, {:?} points at {} Hz",
            display.pixel_size(),
            display.bounds().size,
            display.refresh_rate()
        );
        assert_eq!(
            display.pixel_size(),
            (mode.width as usize, mode.height as usize)
        );
        assert_eq!(
            (display.bounds().size.width, display.bounds().size.height),
            (
                f64::from(mode.width / scale),
                f64::from(mode.height / scale)
            )
        );
        assert_eq!(display.refresh_rate(), mode.refresh);
    }

    fn select_plain_mode(display: &VirtualDisplay, width: usize, height: usize) {
        // SAFETY: documented CFString/CFBoolean option; the returned array owns
        // CGDisplayMode objects, retained through the mode selection call.
        unsafe {
            let options = CFDictionary::<CFString, CFBoolean>::from_slices(
                &[kCGDisplayShowDuplicateLowResolutionModes],
                &[CFBoolean::new(true)],
            );
            let all =
                CGDisplayCopyAllDisplayModes(display.id(), Some(options.as_opaque())).unwrap();
            let all: CFRetained<CFArray<CGDisplayMode>> = CFRetained::cast_unchecked(all);
            let mode = all
                .iter()
                .find(|m| {
                    CGDisplayMode::pixel_width(Some(m)) == width
                        && CGDisplayMode::pixel_height(Some(m)) == height
                        && CGDisplayMode::width(Some(m)) == width
                        && CGDisplayMode::height(Some(m)) == height
                })
                .expect("missing non-Retina mode");
            assert_eq!(
                CGDisplaySetDisplayMode(display.id(), Some(&mode), None).0,
                0
            );
        }
        assert_eq!(display.pixel_size(), (width, height));
    }

    pub fn run() {
        if let Ok(step) = std::env::var("LEGATO_DISPLAY_TEST_STEP") {
            if step == "lifecycle" {
                // Exercise the app's create/arrange/resize/close sequence in one
                // process, including a new display after the old one was removed.
                for first in [RETINA, PLAIN, RETINA, PLAIN] {
                    let display = VirtualDisplay::create("Legato", first).unwrap();
                    display.set_origin(0, -1080).unwrap();
                    for mode in [PLAIN, RETINA] {
                        display.set_mode(mode).unwrap();
                        check(&display, mode);
                    }
                }
                return;
            }
            let first = if step == "retina" || step == "recover" {
                RETINA
            } else {
                PLAIN
            };
            let display = VirtualDisplay::create("Legato", first).unwrap();
            check(&display, first);
            display.set_origin(0, -1080).unwrap();
            match step.as_str() {
                "transition" => {
                    for mode in [
                        RETINA,
                        PLAIN,
                        RETINA,
                        Mode {
                            refresh: 120.0,
                            ..RETINA
                        },
                    ] {
                        display.set_mode(mode).unwrap();
                        check(&display, mode);
                    }
                }
                "recover" => {
                    for (w, h) in [(1920, 1080), (3840, 2160)] {
                        // Simulate macOS choosing either the 1080p duplicate or unscaled
                        // 4K. The latter has the right pixels but the wrong logical size.
                        select_plain_mode(&display, w, h);
                        display.set_mode(RETINA).unwrap();
                        check(&display, RETINA);
                    }
                }
                "plain" | "retina" => {}
                _ => panic!("unexpected test step"),
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
            return;
        }
        // Fresh processes cover reopening the app; lifecycle covers multiple
        // displays in one running AppKit process.
        for step in [
            "plain",
            "retina",
            "plain",
            "retina",
            "transition",
            "recover",
            "lifecycle",
        ] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--ignored")
                .env("LEGATO_DISPLAY_TEST_STEP", step)
                .status()
                .unwrap();
            assert!(status.success(), "display regression failed at {step}");
        }
    }
}
