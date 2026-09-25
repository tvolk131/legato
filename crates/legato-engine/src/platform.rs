//! The per-OS pieces the sharing loop needs, behind one small interface.

use anyhow::{Context, Result};
use legato_core::controller::{Action, CaptureCommand, Controller};
use legato_core::{Inject, LocalInput, ReceiverConfig};

#[cfg(target_os = "macos")]
mod imp {
    pub use legato_macos::{Capture as RawCapture, Injector as RawInjector};

    pub fn start(
        controller: super::Controller,
        sink: impl FnMut(super::Action) + Send + 'static,
        local: impl FnMut(super::LocalInput) + Send + 'static,
    ) -> anyhow::Result<RawCapture> {
        RawCapture::start(controller, sink, local).map_err(anyhow::Error::msg)
    }

    pub fn injector() -> Option<RawInjector> {
        RawInjector::new()
    }

    pub fn check_permissions() -> anyhow::Result<()> {
        let permissions = legato_macos::Permissions::check();
        if permissions.all_granted() {
            return Ok(());
        }
        legato_macos::request_missing(&permissions);
        let missing = permissions.missing().join(" and ");
        anyhow::bail!(
            "Legato needs {missing} access. Allow it in System Settings → Privacy & Security \
             → {missing}, then start sharing again. If Legato is already listed there and \
             switched on, it's for an earlier version: remove it with − and add Legato again \
             with +."
        );
    }

    pub use legato_macos::{clipboard_change_count, receiver_config};
}

#[cfg(windows)]
mod imp {
    pub use legato_windows::{Capture as RawCapture, Injector as RawInjector};

    pub fn start(
        controller: super::Controller,
        sink: impl FnMut(super::Action) + Send + 'static,
        local: impl FnMut(super::LocalInput) + Send + 'static,
    ) -> anyhow::Result<RawCapture> {
        Ok(RawCapture::start(
            controller,
            legato_windows::CaptureOptions::default(),
            sink,
            local,
        )?)
    }

    pub fn injector() -> Option<RawInjector> {
        Some(RawInjector::new())
    }

    pub fn check_permissions() -> anyhow::Result<()> {
        Ok(())
    }

    pub use legato_windows::{clipboard_change_count, receiver_config};
}

pub fn check_permissions() -> Result<()> {
    imp::check_permissions()
}

pub fn receiver_config() -> ReceiverConfig {
    imp::receiver_config()
}

pub fn clipboard_change_count() -> u64 {
    imp::clipboard_change_count()
}

/// Captures this machine's keyboard and mouse. Stops when dropped.
pub struct Capture(imp::RawCapture);

impl Capture {
    pub fn start(
        controller: Controller,
        sink: impl FnMut(Action) + Send + 'static,
        local_input: impl FnMut(LocalInput) + Send + 'static,
    ) -> Result<Self> {
        imp::start(controller, sink, local_input)
            .map(Self)
            .context("capturing the keyboard and mouse")
    }

    pub fn send(&self, command: CaptureCommand) {
        self.0.send(command);
    }
}

/// Injects input from a peer.
pub struct Injector(imp::RawInjector);

impl Injector {
    pub fn new() -> Option<Self> {
        imp::injector().map(Self)
    }

    pub fn apply(&mut self, action: &Inject) {
        self.0.apply(action);
    }

    pub fn set_invert_wheel(&mut self, invert: bool) {
        self.0.invert_wheel = invert;
    }
}
