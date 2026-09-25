//! The one-shot commands: devices, pair, unpair, layout, doctor.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use legato_core::{Align, Side};
use legato_net::{EndpointId, Nearby, NearbyEvent, Net, PairAttempt, PairOutcome};
use legato_proto::{Os, format_pairing_code};
use n0_future::StreamExt;
use tokio::io::{AsyncBufReadExt, BufReader, Lines, Stdin};

use legato_engine::config::{self, Neighbor};
use legato_engine::local_screens;

use crate::{find_paired, start_engine};

fn os_name(os: Option<Os>) -> &'static str {
    match os {
        Some(Os::MacOs) => "macOS",
        Some(Os::Windows) => "Windows",
        None => "unknown OS",
    }
}

pub async fn devices(home: &Option<PathBuf>, watch: bool) -> Result<()> {
    let engine = start_engine(home).await?;
    let net = engine.net();
    println!(
        "This device: \"{}\" ({})",
        net.config().name,
        net.id().fmt_short()
    );
    let paired = net.paired_peers();
    if paired.is_empty() {
        println!("Paired devices: none");
    } else {
        println!("Paired devices:");
        for p in &paired {
            println!(
                "  {}  ({}, {})",
                p.name,
                os_name(Some(p.os)),
                p.id.fmt_short()
            );
        }
    }
    println!("Looking for Legato devices on this network…");
    let mut nearby = net.nearby().await?;
    let mut seen: HashMap<EndpointId, Nearby> = HashMap::new();
    let scan = async {
        while let Some(event) = nearby.next().await {
            match event {
                NearbyEvent::Found(d) => {
                    if seen.insert(d.id, d.clone()).is_none() {
                        let paired = if d.paired { "  [paired]" } else { "" };
                        println!(
                            "  {}  ({}, {}){paired}",
                            d.name,
                            os_name(d.os),
                            d.id.fmt_short()
                        );
                    }
                }
                NearbyEvent::Lost(id) => {
                    if let Some(d) = seen.remove(&id) {
                        println!("  (gone) {}", d.name);
                    }
                }
            }
        }
    };
    if watch {
        tokio::select! {
            _ = scan => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    } else {
        let _ = tokio::time::timeout(Duration::from_secs(6), scan).await;
        if seen.is_empty() {
            println!(
                "  (none found: make sure Legato is running `legato pair` or `legato run` on the other device, and see `legato doctor`)"
            );
        }
    }
    net.clone().shutdown().await;
    Ok(())
}

pub async fn pair(home: &Option<PathBuf>, device: Option<String>) -> Result<()> {
    let engine = start_engine(home).await?;
    let net = engine.net();
    let mut incoming = net.listen_for_pairing();
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    println!("Pairing as \"{}\".", net.config().name);
    println!(
        "Run `legato pair` on the other device too, then pick it from the list on either one."
    );

    let mut nearby = net.nearby().await?;
    let mut choices: Vec<Nearby> = Vec::new();
    let outcome = loop {
        tokio::select! {
            Some(event) = nearby.next() => {
                if let NearbyEvent::Found(d) = event {
                    if choices.iter().any(|c| c.id == d.id) {
                        continue;
                    }
                    let wanted = device.as_deref().is_some_and(|q| {
                        d.name.to_lowercase().contains(&q.to_lowercase()) || d.id.to_string().starts_with(q)
                    });
                    if wanted {
                        println!("Found \"{}\", pairing…", d.name);
                        let attempt = net.pair(d.id).await?;
                        break confirm(attempt, &mut stdin).await?;
                    }
                    choices.push(d.clone());
                    if device.is_none() {
                        let paired = if d.paired { "  (already paired)" } else { "" };
                        println!("  [{}] {}  ({}){paired}", choices.len(), d.name, os_name(d.os));
                    }
                }
            }
            Some(attempt) = incoming.recv() => {
                break confirm(attempt, &mut stdin).await?;
            }
            line = stdin.next_line(), if device.is_none() => {
                let Some(line) = line? else { bail!("cancelled") };
                let Ok(n) = line.trim().parse::<usize>() else {
                    println!("Type the number of a device from the list.");
                    continue;
                };
                let Some(d) = n.checked_sub(1).and_then(|i| choices.get(i)).cloned() else {
                    println!("There's no device {n}.");
                    continue;
                };
                println!("Pairing with \"{}\"…", d.name);
                let attempt = net.pair(d.id).await?;
                break confirm(attempt, &mut stdin).await?;
            }
            _ = tokio::signal::ctrl_c() => bail!("cancelled"),
        }
    };
    match outcome {
        PairOutcome::Paired(peer) => {
            println!("Paired with \"{}\".", peer.name);
            println!(
                "Next: on the machine whose keyboard and mouse you'll use, say where it sits, e.g.\n  legato layout \"{}\" --side below --display 2\nthen run `legato run` on both.",
                peer.name
            );
        }
        PairOutcome::DeclinedHere => println!("Not paired."),
        PairOutcome::DeclinedThere => println!("The other device declined, so they're not paired."),
    }
    net.clone().shutdown().await;
    Ok(())
}

async fn confirm(attempt: PairAttempt, stdin: &mut Lines<BufReader<Stdin>>) -> Result<PairOutcome> {
    let who = if attempt.incoming {
        format!("\"{}\" wants to pair with this device.", attempt.peer.name)
    } else {
        format!("Pairing with \"{}\".", attempt.peer.name)
    };
    println!();
    println!("{who}");
    println!("  Code: {}", format_pairing_code(attempt.code));
    println!("Does the other device show exactly the same code? [y/N]");
    let answer = stdin.next_line().await?.unwrap_or_default();
    let accept = matches!(answer.trim().to_lowercase().as_str(), "y" | "yes");
    if accept {
        println!("Waiting for the other device to confirm…");
    }
    attempt.decide(accept).await
}

pub async fn unpair(home: &Option<PathBuf>, query: &str) -> Result<()> {
    let engine = start_engine(home).await?;
    let net = engine.net();
    let peer = find_paired(net, query)?;
    net.unpair(&peer.id)?;
    let dir = net.store().dir().to_path_buf();
    let mut config = config::load(&dir)?;
    let id = peer.id.to_string();
    let before = config.neighbors.len();
    config.neighbors.retain(|n| !id.starts_with(&n.peer));
    if config.neighbors.len() != before {
        config::save(&dir, &config)?;
    }
    println!("Forgot \"{}\". Unpair on the other device too.", peer.name);
    net.clone().shutdown().await;
    Ok(())
}

pub async fn layout(
    home: &Option<PathBuf>,
    query: &str,
    side: Side,
    display: usize,
    align: Align,
    nudge: f64,
) -> Result<()> {
    let engine = start_engine(home).await?;
    let net = engine.net();
    let peer = find_paired(net, query)?;
    let screens = local_screens();
    if display == 0 || display > screens.displays.len() {
        bail!(
            "this machine has {} display(s); pick one from 1 to {} (see `legato doctor`)",
            screens.displays.len(),
            screens.displays.len()
        );
    }
    let dir = net.store().dir().to_path_buf();
    let mut config = config::load(&dir)?;
    let id = peer.id.to_string();
    config.neighbors.retain(|n| !id.starts_with(&n.peer));
    config.neighbors.push(Neighbor {
        peer: id,
        side,
        display,
        align,
        nudge,
        offset: None,
    });
    config::save(&dir, &config)?;
    let side = match side {
        Side::Left => "left of",
        Side::Right => "right of",
        Side::Above => "above",
        Side::Below => "below",
    };
    println!(
        "\"{}\" now sits {side} display {display} ({}). Saved to {}.",
        peer.name,
        screens.displays[display - 1].name,
        config::path(&dir).display()
    );
    net.clone().shutdown().await;
    Ok(())
}

pub async fn doctor(home: &Option<PathBuf>) -> Result<()> {
    let engine = start_engine(home).await?;
    let net = engine.net();
    println!(
        "Legato {} on {}",
        crate::VERSION,
        os_name(Some(legato_engine::this_os()))
    );
    println!("Device name: \"{}\"", net.config().name);
    println!("Device id:   {}", net.id());
    println!("State:       {}", net.store().dir().display());

    println!("\nDisplays (left to right):");
    let screens = local_screens();
    for (i, d) in screens.displays.iter().enumerate() {
        let b = d.bounds;
        println!(
            "  {}. {:<18} {:>5} × {:<5} at ({}, {})  scale {:.0}%{}",
            i + 1,
            d.name,
            b.width,
            b.height,
            b.x,
            b.y,
            if cfg!(target_os = "macos") {
                d.pixel_scale
            } else {
                d.ui_scale
            } * 100.0,
            if d.primary { "  primary" } else { "" }
        );
    }

    permissions_report();

    println!("\nNetwork:");
    tokio::time::timeout(Duration::from_secs(5), net.endpoint().online())
        .await
        .map_or_else(
            |_| println!("  relay: not connected (no internet? LAN connections still work)"),
            |()| {
                let relays: Vec<String> = net.addr().relay_urls().map(|u| u.to_string()).collect();
                println!("  relay: {}", relays.join(", "));
            },
        );
    for addr in net.addr().ip_addrs() {
        println!("  address: {addr}");
    }

    println!("\nPaired devices:");
    let paired = net.paired_peers();
    if paired.is_empty() {
        println!("  none (run `legato pair`)");
    }
    let config = config::load(net.store().dir())?;
    for p in &paired {
        let id = p.id.to_string();
        let position = config
            .neighbors
            .iter()
            .find(|n| id.starts_with(&n.peer))
            .map_or("no position set".to_string(), |n| {
                format!("{:?} display {} ({:?})", n.side, n.display, n.align).to_lowercase()
            });
        let reach = reachability(net, p.id).await;
        println!(
            "  {} ({}, {}): {position}; {reach}",
            p.name,
            os_name(Some(p.os)),
            p.id.fmt_short()
        );
    }

    println!("\nNearby (5 s scan):");
    match net.nearby().await {
        Err(e) => println!("  discovery unavailable: {e:#}"),
        Ok(mut nearby) => {
            let mut seen = Vec::new();
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(event) = nearby.next().await {
                    if let NearbyEvent::Found(d) = event
                        && !seen.contains(&d.id)
                    {
                        seen.push(d.id);
                        println!("  {} ({}, {})", d.name, os_name(d.os), d.id.fmt_short());
                    }
                }
            })
            .await;
            if seen.is_empty() {
                println!("  none found");
            }
        }
    }
    net.clone().shutdown().await;
    Ok(())
}

async fn reachability(net: &Net, peer: EndpointId) -> String {
    let attempt = tokio::time::timeout(
        Duration::from_secs(8),
        net.endpoint().connect(peer, legato_proto::SESSION_ALPN),
    )
    .await;
    match attempt {
        Err(_) => "not reachable (timed out)".into(),
        Ok(Err(e)) => format!("not reachable ({e})"),
        Ok(Ok(conn)) => {
            let path = conn.paths().iter().find(|p| p.is_selected()).map(|p| {
                let kind = if p.is_relay() { "via relay" } else { "direct" };
                format!("{kind}, {:.1} ms", p.rtt().as_secs_f64() * 1000.0)
            });
            conn.close(0u32.into(), b"doctor");
            format!(
                "reachable ({})",
                path.unwrap_or_else(|| "path unknown".into())
            )
        }
    }
}

fn permissions_report() {
    #[cfg(target_os = "macos")]
    {
        let p = legato_macos::Permissions::check();
        println!("\nPermissions:");
        println!(
            "  Accessibility: {}",
            if p.accessibility {
                "granted"
            } else {
                "MISSING (System Settings → Privacy & Security → Accessibility)"
            }
        );
        println!(
            "  Input Monitoring: {}",
            if p.listen_events {
                "granted"
            } else {
                "MISSING (System Settings → Privacy & Security → Input Monitoring)"
            }
        );
        println!(
            "  Screen Recording: {}",
            if p.screen_recording {
                "granted"
            } else {
                "not granted (only needed to show this Mac as a display on Windows; \
                 allowed on first use)"
            }
        );
    }
    #[cfg(windows)]
    {
        println!("\nNetwork profile:");
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-NetConnectionProfile | ForEach-Object { \"$($_.InterfaceAlias): $($_.NetworkCategory)\" }",
            ])
            .output();
        match out {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    let hint = if line.ends_with("Public") {
                        "  ← discovery may be blocked; consider Private"
                    } else {
                        ""
                    };
                    println!("  {line}{hint}");
                }
            }
            Err(e) => println!("  unknown ({e})"),
        }
    }
}
