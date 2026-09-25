//! `legato.toml`: where other machines sit, and behaviour tweaks.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use legato_core::{Align, Side};
use serde::{Deserialize, Serialize};

pub const FILE: &str = "legato.toml";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where paired machines sit relative to this one's displays. Only needed on the
    /// machine whose keyboard and mouse you use.
    #[serde(rename = "neighbor", skip_serializing_if = "Vec::is_empty")]
    pub neighbors: Vec<Neighbor>,
    pub switching: Switching,
    pub keys: Keys,
    pub scrolling: Scrolling,
    pub control: Control,
    pub clipboard: Clipboard,
    pub extend: Extend,
}

/// Virtual monitor mode: the extra display a Mac shows on this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extend {
    /// Size in pixels. With `hidpi` the Mac treats it as Retina, so it looks like half
    /// this size (3840×2160 looks like 1920×1080, sharp on a 4K monitor).
    pub width: u32,
    pub height: u32,
    pub hidpi: bool,
    pub fps: u32,
    /// Video quality, in megabits per second.
    pub bitrate_mbps: u32,
    /// Show frame rate, bitrate and delays over the picture.
    pub stats: bool,
}

impl Default for Extend {
    fn default() -> Self {
        Self {
            width: 3840,
            height: 2160,
            hidpi: true,
            fps: 60,
            bitrate_mbps: 40,
            stats: true,
        }
    }
}

impl Extend {
    /// The request sent to the Mac, within what the video pipeline handles.
    pub fn request(&self) -> legato_proto::ExtendRequest {
        legato_proto::ExtendRequest {
            width: self.width.clamp(1280, 7680) & !1,
            height: self.height.clamp(720, 4320) & !1,
            hidpi: self.hidpi,
            fps: self.fps.clamp(10, 120),
            bitrate: self.bitrate_mbps.clamp(2, 200) * 1_000_000,
        }
    }
}

/// Which machines may drive the others. Shared between paired machines: the most recent
/// change wins everywhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Control {
    /// Unset: whichever machine's keyboard and mouse you use takes over. Set: only the
    /// machine with this id (or id prefix) controls the others.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller: Option<String>,
    /// When this was last changed, in seconds since the Unix epoch.
    pub updated_at: u64,
}

impl Control {
    /// Whether the machine with this id may drive others.
    pub fn allows(&self, id: &str) -> bool {
        self.controller
            .as_deref()
            .is_none_or(|c| !c.is_empty() && id.starts_with(c))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Clipboard {
    /// Share copied text and images with paired machines.
    pub enabled: bool,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Neighbor {
    /// The peer's id (or a unique prefix of it).
    pub peer: String,
    /// Which side of `display` the peer sits on.
    pub side: Side,
    /// This machine's display it sits next to: 1-based, counted left to right as listed
    /// by `legato doctor`.
    pub display: usize,
    #[serde(default)]
    pub align: Align,
    /// Shift along the shared edge, in desk units (roughly the OS's scaled pixels).
    #[serde(default)]
    pub nudge: f64,
    /// Exact position of the peer's top-left corner in desk units, relative to this
    /// machine's desktop origin. Set by the arrangement editor; overrides `side`,
    /// `display`, `align` and `nudge`, which then only describe it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<[f64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Switching {
    /// How far to keep pushing past an edge before the cursor crosses, in desk units.
    pub push_distance: f64,
}

impl Default for Switching {
    fn default() -> Self {
        Self {
            push_distance: 30.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Remap {
    /// Swap Alt and the Windows key when a Windows keyboard drives a Mac (so the key next
    /// to the space bar acts as Command), and Command and Control when a Mac keyboard
    /// drives Windows (so Cmd+C copies).
    #[default]
    Auto,
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    pub remap: Remap,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scrolling {
    /// Reverse the direction of mouse wheels driving this machine.
    pub invert_wheel: bool,
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

pub fn load(dir: &Path) -> Result<Config> {
    let path = path(dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(dir: &Path, config: &Config) -> Result<()> {
    let path = path(dir);
    let text = format!(
        "# Legato settings. See `legato layout --help`.\n\n{}",
        toml::to_string_pretty(config)?
    );
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_example() {
        let config: Config = toml::from_str(
            r#"
            [[neighbor]]
            peer = "k3j4h5"
            side = "below"
            display = 2

            [switching]
            push_distance = 20

            [keys]
            remap = "none"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.neighbors,
            [Neighbor {
                peer: "k3j4h5".into(),
                side: Side::Below,
                display: 2,
                align: Align::Center,
                nudge: 0.0,
                offset: None,
            }]
        );
        assert_eq!(config.switching.push_distance, 20.0);
        assert_eq!(config.keys.remap, Remap::None);
        assert!(!config.scrolling.invert_wheel);
    }

    #[test]
    fn round_trips_and_defaults_when_empty() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config, Config::default());
        let mut config = Config::default();
        config.neighbors.push(Neighbor {
            peer: "abc".into(),
            side: Side::Right,
            display: 1,
            align: Align::Start,
            nudge: -12.5,
            offset: Some([416.0, 1440.0]),
        });
        let text = toml::to_string_pretty(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn control_mode_allows_everyone_or_just_the_controller() {
        let mut control = Control::default();
        assert!(control.allows("abcd"));
        control.controller = Some("ab".into());
        assert!(control.allows("abcd"));
        assert!(!control.allows("zzzz"));
    }

    #[test]
    fn typos_are_errors() {
        assert!(toml::from_str::<Config>("[switching]\npush_distanse = 3").is_err());
    }
}
