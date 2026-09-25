//! Small platform integrations: the Dock icon, launching at login, revealing files.

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

/// Shows each new frame at the next screen refresh, replacing any frame still waiting
/// ("mailbox"), instead of queueing behind it as vsync does. It never tears. DirectX 12
/// supports it on every Windows 10+ GPU, so it's used when there's a DX12 adapter;
/// otherwise iced keeps its vsync default. Must run before any other thread starts.
pub fn prefer_low_latency_presentation() {
    #[cfg(windows)]
    {
        use iced::wgpu;
        if std::env::var_os("ICED_PRESENT_MODE").is_some()
            || std::env::var_os("WGPU_BACKEND").is_some()
        {
            return;
        }
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..Default::default()
        });
        if instance.enumerate_adapters(wgpu::Backends::DX12).is_empty() {
            return;
        }
        // SAFETY: called first thing in `main`, before any other thread exists.
        unsafe {
            std::env::set_var("WGPU_BACKEND", "dx12");
            std::env::set_var("ICED_PRESENT_MODE", "mailbox");
        }
    }
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

/// Shows files in Finder or Explorer.
#[allow(
    clippy::disallowed_methods,
    reason = "once per drop, and Explorer is a GUI program so no console flashes"
)]
pub fn reveal(paths: &[std::path::PathBuf]) {
    let Some(first) = paths.first() else { return };
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(first)
        .spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", first.display()))
        .spawn();
}
