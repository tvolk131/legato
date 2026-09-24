use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, SecretKey,
    endpoint::{Connection, QuicTransportConfig, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ALPN: &[u8] = b"legato/kvm/1";

#[derive(Debug, Clone)]
struct KvmHandler {
    allowed: Arc<RwLock<HashSet<EndpointId>>>,
}

impl ProtocolHandler for KvmHandler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = conn.remote_id(); // authenticated by TLS 1.3 (raw public key)
        if !self.allowed.read().unwrap().contains(&peer) {
            conn.close(403u32.into(), b"not paired");
            return Ok(());
        }
        // Reliable control stream: echo 8-byte frames.
        let c2 = conn.clone();
        tokio::spawn(async move {
            let (mut send, mut recv) = c2.accept_bi().await?;
            let mut buf = [0u8; 8];
            while recv.read_exact(&mut buf).await.is_ok() {
                send.write_all(&buf).await?;
            }
            anyhow::Ok(())
        });
        // Unreliable datagrams: echo back.
        loop {
            let dg = conn.read_datagram().await?;
            conn.send_datagram(dg).map_err(AcceptError::from_err)?;
        }
    }
}

fn lan_builder(sk: SecretKey) -> iroh::endpoint::Builder {
    let transport = QuicTransportConfig::builder()
        .keep_alive_interval(Duration::from_secs(1))
        .build();
    // presets::Minimal = crypto provider only; Builder::empty() is RelayMode::Disabled, no lookups.
    Endpoint::builder(presets::Minimal)
        .secret_key(sk)
        .transport_config(transport)
        .address_lookup(MdnsAddressLookup::builder().service_name("legato"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let use_mdns = std::env::args().any(|a| a == "--mdns");
    let server_sk = SecretKey::generate();
    let client_sk = SecretKey::generate();
    let allowed = Arc::new(RwLock::new(HashSet::from([client_sk.public()])));

    let server_ep = lan_builder(server_sk).alpns(vec![ALPN.to_vec()]).bind().await?;
    let router = Router::builder(server_ep.clone())
        .accept(ALPN, KvmHandler { allowed })
        .spawn();
    println!("server id {}", server_ep.id());
    println!("server addr {:?}", server_ep.addr());

    let client_ep = lan_builder(client_sk).bind().await?;

    let target: EndpointAddr = if use_mdns {
        // id only; resolved via mDNS
        tokio::time::sleep(Duration::from_secs(2)).await;
        server_ep.id().into()
    } else {
        server_ep.addr()
    };
    let t0 = Instant::now();
    let conn = tokio::time::timeout(Duration::from_secs(15), client_ep.connect(target, ALPN))
        .await
        .context("connect timeout")??;
    println!("connected in {:?}", t0.elapsed());
    println!("max_datagram_size = {:?}", conn.max_datagram_size());
    for p in conn.paths().iter() {
        println!(
            "path {:?} remote={:?} selected={} ip={} relay={} rtt={:?}",
            p.id(), p.remote_addr(), p.is_selected(), p.is_ip(), p.is_relay(), p.rtt()
        );
    }

    // Datagram ping-pong
    let n = 2000;
    let mut samples = Vec::with_capacity(n);
    for i in 0..n as u64 {
        let t = Instant::now();
        conn.send_datagram(Bytes::copy_from_slice(&i.to_le_bytes()))?;
        let _ = conn.read_datagram().await?;
        samples.push(t.elapsed());
    }
    report("datagram rtt", &mut samples);

    // Stream ping-pong
    let (mut send, mut recv) = conn.open_bi().await?;
    let mut samples = Vec::with_capacity(n);
    let mut buf = [0u8; 8];
    for i in 0..n as u64 {
        let t = Instant::now();
        send.write_all(&i.to_le_bytes()).await?;
        recv.read_exact(&mut buf).await?;
        samples.push(t.elapsed());
    }
    report("stream rtt", &mut samples);

    // Unpaired client is rejected
    let rogue = lan_builder(SecretKey::generate()).bind().await?;
    let rc = rogue.connect(server_ep.addr(), ALPN).await?;
    println!("rogue closed: {:?}", rc.closed().await);

    conn.close(0u32.into(), b"bye");
    client_ep.close().await;
    rogue.close().await;
    router.shutdown().await?;
    Ok(())
}

fn report(name: &str, s: &mut [Duration]) {
    s.sort();
    let p = |q: f64| s[((s.len() as f64 - 1.0) * q) as usize];
    println!("{name}: p50={:?} p99={:?} max={:?}", p(0.5), p(0.99), s[s.len() - 1]);
}
