use std::time::{Duration, Instant};
use anyhow::Result;
use bytes::Bytes;
use iroh::{Endpoint, EndpointAddr, TransportAddr, endpoint::{PathEvent, presets}};
use n0_future::StreamExt;

const ALPN: &[u8] = b"probe/upgrade/0";

#[tokio::main]
async fn main() -> Result<()> {
    let t_bind = Instant::now();
    let server = Endpoint::builder(presets::N0).alpns(vec![ALPN.to_vec()]).bind().await?;
    server.online().await;
    println!("server bind+online: {:?}", t_bind.elapsed());
    let relay = server.addr().relay_urls().next().cloned().expect("relay");
    println!("home relay: {relay}");
    let srv = server.clone();
    tokio::spawn(async move {
        while let Some(inc) = srv.accept().await {
            let conn = inc.accept()?.await?;
            tokio::spawn(async move {
                while let Ok(d) = conn.read_datagram().await { let _ = conn.send_datagram(d); }
            });
        }
        anyhow::Ok(())
    });

    let client = Endpoint::bind(presets::N0).await?;
    client.online().await;
    // Relay-only address: forces the connection to start on the relay path.
    let addr = EndpointAddr::from_parts(server.id(), [TransportAddr::Relay(relay)]);
    let t0 = Instant::now();
    let conn = client.connect(addr, ALPN).await?;
    println!("connected (via relay) in {:?}", t0.elapsed());
    let mut events = conn.path_events();
    let c2 = conn.clone();
    let pinger = tokio::spawn(async move {
        let mut i = 0u64;
        loop {
            let t = Instant::now();
            if c2.send_datagram(Bytes::copy_from_slice(&i.to_le_bytes())).is_err() { break; }
            match tokio::time::timeout(Duration::from_millis(500), c2.read_datagram()).await {
                Ok(Ok(_)) => { if i % 10 == 0 { println!("  t+{:?} dgram rtt {:?}", t0.elapsed(), t.elapsed()); } }
                _ => println!("  t+{:?} dgram lost/timeout", t0.elapsed()),
            }
            i += 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let deadline = tokio::time::sleep(Duration::from_secs(8));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            ev = events.next() => match ev {
                Some(PathEvent::Selected { remote_addr, .. }) => println!("t+{:?} SELECTED {remote_addr:?}", t0.elapsed()),
                Some(PathEvent::Opened { remote_addr, .. }) => println!("t+{:?} opened {remote_addr:?}", t0.elapsed()),
                Some(PathEvent::Closed { remote_addr, .. }) => println!("t+{:?} closed {remote_addr:?}", t0.elapsed()),
                Some(other) => println!("t+{:?} {other:?}", t0.elapsed()),
                None => break,
            },
            _ = &mut deadline => break,
        }
    }
    pinger.abort();
    for p in conn.paths().iter() {
        println!("final path {:?} selected={} rtt={:?}", p.remote_addr(), p.is_selected(), p.rtt());
    }
    conn.close(0u32.into(), b"");
    client.close().await;
    server.close().await;
    Ok(())
}
