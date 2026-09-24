//! `legato`: share one keyboard and mouse between machines.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use legato_core::{Align, Side};
use legato_engine::{Engine, Status};
use legato_net::{Net, NetConfig, PairedPeer};
use tokio::sync::broadcast::error::RecvError;

mod commands;

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
    let engine = Engine::start(home.clone(), VERSION).await?;
    if engine.net().paired_peers().is_empty() {
        bail!("no paired devices yet: run `legato pair` on both devices first");
    }
    let mut status = engine.subscribe();
    engine.start_sharing()?;
    tracing::info!(
        "Legato {VERSION} running as \"{}\". Ctrl+C to stop.",
        engine.net().config().name
    );
    let name_of = |id: &legato_net::EndpointId| {
        engine
            .net()
            .store()
            .peer(id)
            .map_or_else(|| id.fmt_short().to_string(), |p| p.name)
    };
    let mut failed = None;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            event = status.recv() => match event {
                Ok(event) => {
                    if let Some(text) = legato_engine::describe(&event, name_of) {
                        match &event {
                            Status::Problem(_) => tracing::warn!("{text}"),
                            Status::Error(e) => {
                                tracing::error!("{text}");
                                failed = Some(e.clone());
                            }
                            _ => tracing::info!("{text}"),
                        }
                    }
                    if event == Status::Sharing(false) {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    }
    engine.stop_sharing().await;
    engine.net().clone().shutdown().await;
    match failed {
        Some(e) => bail!("sharing stopped: {e}"),
        None => Ok(()),
    }
}
