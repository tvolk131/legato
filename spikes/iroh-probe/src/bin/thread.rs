use std::time::{Duration, Instant};
use anyhow::Result;
use bytes::Bytes;
use iroh::{Endpoint, endpoint::presets};

const ALPN: &[u8] = b"probe/thread/0";

fn main() -> Result<()> {
    // Background runtime owned by a dedicated thread (as you'd do next to a GUI event loop).
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let (srv, conn) = rt.block_on(async {
        let srv = Endpoint::builder(presets::Minimal).alpns(vec![ALPN.to_vec()]).bind().await?;
        let cli = Endpoint::bind(presets::Minimal).await?;
        let addr = srv.addr();
        let (a, b) = tokio::join!(async { srv.accept().await.unwrap().await }, cli.connect(addr, ALPN));
        let server_conn = a?;
        let client_conn = b?;
        tokio::spawn(async move {
            let mut n = 0;
            let t = Instant::now();
            while let Ok(_d) = server_conn.read_datagram().await { n += 1; if n == 1000 { println!("server got 1000 datagrams in {:?}", t.elapsed()); } }
        });
        std::mem::forget(cli);
        anyhow::Ok((srv, client_conn))
    })?;
    // Plain OS thread (e.g. a CGEventTap / low-level hook thread), no tokio context entered.
    let h = std::thread::spawn(move || {
        for i in 0u32..1000 {
            conn.send_datagram(Bytes::copy_from_slice(&i.to_le_bytes())).expect("send");
            std::thread::sleep(Duration::from_micros(500));
        }
        println!("sender thread done; in tokio ctx: {}", tokio::runtime::Handle::try_current().is_ok());
    });
    h.join().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    drop(srv);
    Ok(())
}
