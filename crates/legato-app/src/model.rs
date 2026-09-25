//! Everything the views show, as plain data (so views can be tested without an engine).

use std::collections::HashMap;
use std::time::Duration;

use legato_engine::Config;
use legato_engine::config::{Placement, Quality, Resolution};
use legato_net::{EndpointId, PathKind};
use legato_proto::{Os, Screens};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Devices,
    Arrangement,
    Settings,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: EndpointId,
    pub name: String,
    pub os: Option<Os>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub device: Device,
    /// Connection path while connected.
    pub connection: Option<Option<(PathKind, Duration)>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PairingStage {
    /// Showing the code; waiting for this user to confirm.
    Confirm,
    /// This user confirmed; waiting for the other device.
    Waiting,
    /// Connecting to the other device to get a code.
    Connecting,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pairing {
    pub name: String,
    pub code: Option<u32>,
    pub incoming: bool,
    pub stage: PairingStage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub page: Page,
    pub this: Device,
    pub version: String,
    pub state_dir: String,
    pub sharing: bool,
    pub paired: Vec<Paired>,
    pub nearby: Vec<Device>,
    /// The dialog stays mounted while closing, so its content is kept here.
    pub pairing: Option<Pairing>,
    pub pairing_open: bool,
    pub config: Config,
    pub local: Screens,
    pub known: HashMap<EndpointId, Screens>,
    /// Where peers have placed this machine, for mirroring their arrangement.
    pub placed_us: HashMap<EndpointId, legato_proto::Point>,
    /// Which peer this machine is driving, or which peer drives it.
    pub active: Option<EndpointId>,
    pub problems: Vec<String>,
    pub notice: Option<(u64, String)>,
    pub autostart: Option<bool>,
    /// The Mac whose extra display is shown here (virtual monitor mode).
    pub viewing: Option<EndpointId>,
    /// The "Show as display" choices being made. Kept while the dialog closes.
    pub display_options: Option<DisplayOptions>,
    pub display_options_open: bool,
}

/// Where and how to show a Mac's extra display.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayOptions {
    pub peer: EndpointId,
    pub placement: Placement,
    /// For full screen: which of this machine's displays, 1-based, left to right.
    pub display: usize,
    pub resolution: Resolution,
    pub quality: Quality,
    pub fixed: (u32, u32),
    pub fps: u32,
}

impl DisplayOptions {
    pub fn from_config(peer: EndpointId, extend: &legato_engine::config::Extend) -> Self {
        Self {
            peer,
            placement: extend.placement.unwrap_or(Placement::FullScreen),
            display: extend.display.max(1),
            resolution: extend.resolution,
            quality: extend.quality,
            fixed: (extend.width, extend.height),
            fps: extend.fps,
        }
    }

    pub fn save_to(&self, extend: &mut legato_engine::config::Extend) {
        extend.placement = Some(self.placement);
        extend.display = self.display;
        extend.resolution = self.resolution;
        extend.quality = self.quality;
        (extend.width, extend.height) = self.fixed;
        extend.fps = self.fps;
    }
}

impl Model {
    pub fn paired(&self, id: &EndpointId) -> Option<&Paired> {
        self.paired.iter().find(|p| &p.device.id == id)
    }

    pub fn connected(&self) -> impl Iterator<Item = &Paired> {
        self.paired.iter().filter(|p| p.connection.is_some())
    }

    /// Whether this machine can show `peer`'s extra display: only Windows shows a Mac's.
    pub fn can_view(&self, peer: &Paired) -> bool {
        self.this.os == Some(Os::Windows)
            && peer.device.os == Some(Os::MacOs)
            && peer.connection.is_some()
    }

    /// This machine's displays, left to right as `legato doctor` numbers them.
    pub fn displays(&self) -> Vec<legato_proto::Display> {
        let mut displays = self.local.displays.clone();
        displays.sort_by(|a, b| a.bounds.x.total_cmp(&b.bounds.x));
        displays
    }

    pub fn name_of(&self, id: &EndpointId) -> String {
        self.paired(id)
            .map(|p| p.device.name.clone())
            .or_else(|| {
                self.nearby
                    .iter()
                    .find(|n| &n.id == id)
                    .map(|n| n.name.clone())
            })
            .unwrap_or_else(|| id.fmt_short().to_string())
    }
}

pub fn os_name(os: Option<Os>) -> &'static str {
    match os {
        Some(Os::MacOs) => "macOS",
        Some(Os::Windows) => "Windows",
        None => "unknown system",
    }
}
