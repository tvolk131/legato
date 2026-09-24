//! `legato`: share one keyboard and mouse between machines.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use legato_core::{Align, Side};
use legato_net::{Net, NetConfig, PairedPeer};
use legato_proto::{Os, Screens};

#[cfg_attr(not(windows), allow(dead_code))]
mod arrange;
mod commands;
mod config;
#[cfg(target_os = "macos")]
mod receive;
#[cfg(windows)]
mod share;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "legato",
    version,
    about = "Use one keyboard and mouse across your computers"
)]
struct Cli {
    /// State directory (identity, paired devices, settings). Defaults to the per-user app
    /// data directory; `LEGATO_HOME` also works.
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,
    /// More logging (-v debug, -vv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List Legato devices on this network, and paired devices.
    Devices {
        /// Keep watching until Ctrl+C.
        #[arg(long)]
        watch: bool,
    },
    /// Pair with a device on this network. Run it on both devices.
    Pair {
        /// Name (or id prefix) of the device to pair with. Leave out to choose from a list,
        /// or to wait for the other device to start pairing.
        device: Option<String>,
    },
    /// Forget a paired device.
    Unpair { device: String },
    /// Say where a paired device sits, e.g. `legato layout MacBook --side below --display 2`.
    Layout {
        device: String,
        #[arg(long, value_enum)]
        side: SideArg,
        /// This machine's display it sits next to, counted from 1, left to right (see
        /// `legato doctor`).
        #[arg(long)]
        display: usize,
        #[arg(long, value_enum, default_value = "center")]
        align: AlignArg,
        /// Shift along the shared edge, in scaled pixels.
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        nudge: f64,
    },
    /// Start sharing: control paired devices from this machine's keyboard and mouse, or
    /// let them control this one.
    Run,
    /// Show displays, permissions, network status and paired devices, for bug reports.
    Doctor,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum SideArg {
    Left,
    Right,
    Above,
    Below,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum AlignArg {
    Start,
    Center,
    End,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    #[cfg(windows)]
    legato_windows::init_dpi_awareness();

    let result = match cli.command {
        Command::Devices { watch } => commands::devices(&cli.home, watch).await,
        Command::Pair { device } => commands::pair(&cli.home, device).await,
        Command::Unpair { device } => commands::unpair(&cli.home, &device).await,
        Command::Layout {
            device,
            side,
            display,
            align,
            nudge,
        } => {
            commands::layout(
                &cli.home,
                &device,
                side.into(),
                display,
                align.into(),
                nudge,
            )
            .await
        }
        Command::Run => run(&cli.home).await,
        Command::Doctor => commands::doctor(&cli.home).await,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn init_logging(verbose: u8) {
    let ours = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let default =
        format!("error,legato={ours},legato_net={ours},legato_macos={ours},legato_windows={ours}");
    let filter = tracing_subscriber::EnvFilter::try_from_env("LEGATO_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(verbose > 0)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_writer(std::io::stderr)
        .init();
}

impl From<SideArg> for Side {
    fn from(s: SideArg) -> Self {
        match s {
            SideArg::Left => Side::Left,
            SideArg::Right => Side::Right,
            SideArg::Above => Side::Above,
            SideArg::Below => Side::Below,
        }
    }
}

impl From<AlignArg> for Align {
    fn from(a: AlignArg) -> Self {
        match a {
            AlignArg::Start => Align::Start,
            AlignArg::Center => Align::Center,
            AlignArg::End => Align::End,
        }
    }
}

pub(crate) async fn start_net(home: &Option<PathBuf>) -> Result<Net> {
    let mut config = NetConfig::for_this_machine(VERSION);
    config.store_dir = home.clone();
    Net::start(config).await
}

/// This machine's displays.
pub(crate) fn local_screens() -> Screens {
    #[cfg(target_os = "macos")]
    return legato_macos::screens();
    #[cfg(windows)]
    return legato_windows::screens();
    #[cfg(not(any(target_os = "macos", windows)))]
    Screens {
        displays: vec![],
        native_per_desk: 1.0,
    }
}

pub(crate) fn this_os() -> Os {
    if cfg!(target_os = "macos") {
        Os::MacOs
    } else {
        Os::Windows
    }
}

/// Finds a paired device by id prefix or (case-insensitive) name.
pub(crate) fn find_paired(net: &Net, query: &str) -> Result<PairedPeer> {
    let q = query.to_lowercase();
    let peers = net.paired_peers();
    let exact: Vec<_> = peers
        .iter()
        .filter(|p| p.id.to_string().starts_with(&q) || p.name.to_lowercase() == q)
        .collect();
    let matches = if exact.is_empty() {
        peers
            .iter()
            .filter(|p| p.name.to_lowercase().contains(&q))
            .collect()
    } else {
        exact
    };
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("no paired device matches \"{query}\" (see `legato devices`)"),
        many => bail!(
            "\"{query}\" matches several devices: {}",
            many.iter()
                .map(|p| format!("{} ({})", p.name, p.id.fmt_short()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

async fn run(home: &Option<PathBuf>) -> Result<()> {
    let net = start_net(home).await?;
    if net.paired_peers().is_empty() {
        bail!("no paired devices yet: run `legato pair` on both devices first");
    }
    let config = config::load(net.store().dir())?;
    let screens = local_screens();
    let hello = legato_proto::Hello {
        protocol: legato_proto::PROTOCOL_VERSION,
        app_version: VERSION.into(),
        name: net.config().name.clone(),
        os: this_os(),
        screens: screens.clone(),
    };
    let events = net.start_sessions(hello);
    tracing::info!(
        "Legato {VERSION} running as \"{}\"; waiting for paired devices. Ctrl+C to stop.",
        net.config().name
    );

    #[cfg(windows)]
    let result = share::run(&net, &config, screens, events).await;
    #[cfg(target_os = "macos")]
    let result = receive::run(&net, &config, events).await;
    #[cfg(not(any(target_os = "macos", windows)))]
    let result: Result<()> = {
        let _ = (config, events);
        Err(anyhow::anyhow!("this platform isn't supported"))
    };

    net.shutdown().await;
    result.context("sharing stopped")
}

/// " (direct, 1.2 ms)" or " (via relay, 60 ms: …)", for log lines.
pub(crate) fn describe_path(session: &legato_net::Session) -> String {
    match session.path() {
        Some((legato_net::PathKind::Direct, rtt)) => {
            format!(" (direct, {:.1} ms)", rtt.as_secs_f64() * 1000.0)
        }
        Some((legato_net::PathKind::Relay, rtt)) => format!(
            " (via relay, {:.0} ms: expect lag until a direct path is found)",
            rtt.as_secs_f64() * 1000.0
        ),
        None => String::new(),
    }
}
