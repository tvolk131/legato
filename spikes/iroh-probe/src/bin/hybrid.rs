//! Hybrid discovery: mDNS for the LAN plus n0 DNS/pkarr + relays for everything else.
//! A "same LAN" peer (mDNS) and a "remote" peer (no mDNS) both dial the server by
//! EndpointId only. This needs internet access, because it uses n0's public relay and DNS.
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use iroh::{
    Endpoint, EndpointId,
    endpoint::{Connection, PathEvent, PortmapperConfig, presets},
};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use n0_future::StreamExt;

const ALPN: &[u8] = b"probe/hybrid/0";

fn mdns() -> iroh_mdns_address_lookup::MdnsAddressLookupBuilder {
    MdnsAddressLookup::builder().service_name("legato-probe")
}

async fn watch(label: &'static str, conn: Connection, t0: Instant) {
    for p in conn.paths().iter() {
        println!(
            "[{label}] t+{:?} initial path {:?} selected={} relay={}",
            t0.elapsed(), p.remote_addr(), p.is_selected(), p.is_relay()
        );
    }
    let mut events = conn.path_events();
    let deadline = tokio::time::sleep(Duration::from_secs(6));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            ev = events.next() => match ev {
                Some(PathEvent::Selected { remote_addr, .. }) =>
                    println!("[{label}] t+{:?} SELECTED {remote_addr:?}", t0.elapsed()),
                Some(_) => {}
                None => break,
            },
            _ = &mut deadline => break,
        }
    }
    for p in conn.paths().iter().filter(|p| p.is_selected()) {
        println!("[{label}] final selected {:?} relay={} rtt={:?}", p.remote_addr(), p.is_relay(), p.rtt());
    }
}

async fn dial(label: &'static str, ep: &Endpoint, id: EndpointId) -> Result<()> {
    let t0 = Instant::now();
    let conn = tokio::time::timeout(Duration::from_secs(20), ep.connect(id, ALPN))
        .await
        .context("connect timeout")??;
    println!("[{label}] connected by id in {:?}", t0.elapsed());
    watch(label, conn.clone(), t0).await;
    conn.close(0u32.into(), b"");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let t_bind = Instant::now();
    let server = Endpoint::builder(presets::N0)
        .address_lookup(mdns())
        .portmapper_config(PortmapperConfig::Disabled)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await?;
    println!("server bound in {:?} (not waiting for online)", t_bind.elapsed());
    let srv = server.clone();
    tokio::spawn(async move {
        while let Some(inc) = srv.accept().await {
            if let Ok(connecting) = inc.accept() {
                tokio::spawn(async move {
                    if let Ok(c) = connecting.await {
                        c.closed().await;
                    }
                });
            }
        }
    });

    // Same-LAN peer: same config as the server.
    let lan = Endpoint::builder(presets::N0)
        .address_lookup(mdns())
        .portmapper_config(PortmapperConfig::Disabled)
        .bind()
        .await?;
    dial("lan  ", &lan, server.id()).await?;

    // "Remote" peer: no mDNS, so it must use the pkarr DNS record and the relay.
    let t_online = Instant::now();
    server.online().await;
    println!("server online (relay + pkarr) after {:?}", t_online.elapsed());
    tokio::time::sleep(Duration::from_secs(2)).await; // let the pkarr record publish
    let remote = Endpoint::builder(presets::N0)
        .portmapper_config(PortmapperConfig::Disabled)
        .bind()
        .await?;
    dial("remote", &remote, server.id()).await?;

    lan.close().await;
    remote.close().await;
    server.close().await;
    Ok(())
}
