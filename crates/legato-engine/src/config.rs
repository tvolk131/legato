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
    /// Swap Alt and the Windows key when a Windows keyboard drives a Mac, so the key next
    /// to the space bar acts as Command.
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
    fn typos_are_errors() {
        assert!(toml::from_str::<Config>("[switching]\npush_distanse = 3").is_err());
    }
}
