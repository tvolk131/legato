//! Small platform integrations: the Dock icon, dark mode, launching at login.

use anyhow::{Context, Result};

pub fn init() {
    // A menu bar app: no Dock icon until a window is open.
    show_in_dock(false);
}

/// On macOS, shows the app in the Dock and app switcher (while a window is open) or hides
/// it (tray only). Does nothing elsewhere.
pub fn show_in_dock(visible: bool) {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let policy = if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        app.setActivationPolicy(policy);
        if visible {
            app.activate();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = visible;
}

/// Whether the system is in dark mode.
pub fn dark_mode() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "Dark")
    }
    #[cfg(windows)]
    {
        // AppsUseLightTheme = 0 means dark.
        std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                "/v",
                "AppsUseLightTheme",
            ])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("0x0"))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    false
}

fn launcher() -> Result<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().context("finding this program")?;
    let exe = exe.to_str().context("program path isn't valid UTF-8")?;
    Ok(auto_launch::AutoLaunchBuilder::new()
        .set_app_name("Legato")
        .set_app_path(exe)
        .set_args(&["--hidden"])
        .set_macos_launch_mode(auto_launch::MacOSLaunchMode::LaunchAgent)
        .build()?)
}

/// Whether Legato opens at login, or `None` if that can't be determined.
pub fn autostart_enabled() -> Option<bool> {
    launcher().ok()?.is_enabled().ok()
}

pub fn set_autostart(on: bool) -> Result<()> {
    let launcher = launcher()?;
    if on {
        launcher.enable()?;
    } else {
        launcher.disable()?;
    }
    Ok(())
}
