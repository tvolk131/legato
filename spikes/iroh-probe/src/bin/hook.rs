use std::{collections::HashSet, sync::{Arc, RwLock}};
use anyhow::Result;
use iroh::{Endpoint, EndpointId, SecretKey, endpoint::{AfterHandshakeOutcome, Connection, EndpointHooks, presets}};

const KVM_ALPN: &[u8] = b"legato/1";
const PAIR_ALPN: &[u8] = b"legato/pair/1";

#[derive(Debug, Clone, Default)]
struct PairedOnly(Arc<RwLock<HashSet<EndpointId>>>);

impl EndpointHooks for PairedOnly {
    async fn after_handshake<'a>(&'a self, conn: &'a Connection) -> AfterHandshakeOutcome {
        if conn.alpn() == PAIR_ALPN || self.0.read().unwrap().contains(&conn.remote_id()) {
            AfterHandshakeOutcome::Accept
        } else {
            AfterHandshakeOutcome::Reject { error_code: 403u32.into(), reason: b"not paired".to_vec() }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let paired = PairedOnly::default();
    let srv = Endpoint::builder(presets::Minimal)
        .hooks(paired.clone())
        .alpns(vec![KVM_ALPN.to_vec(), PAIR_ALPN.to_vec()])
        .bind().await?;
    let s2 = srv.clone();
    tokio::spawn(async move { while let Some(i) = s2.accept().await { if let Ok(a) = i.accept() { let _ = a.await; } } });
    let good = SecretKey::generate();
    paired.0.write().unwrap().insert(good.public());
    let good_ep = Endpoint::builder(presets::Minimal).secret_key(good).bind().await?;
    let bad_ep = Endpoint::bind(presets::Minimal).await?;
    println!("good kvm: {:?}", good_ep.connect(srv.addr(), KVM_ALPN).await.map(|c| c.remote_id().fmt_short().to_string()));
    let bad = bad_ep.connect(srv.addr(), KVM_ALPN).await;
    match bad { Ok(c) => println!("bad kvm: connected then {:?}", c.closed().await), Err(e) => println!("bad kvm: err {e}") }
    println!("bad pair: {:?}", bad_ep.connect(srv.addr(), PAIR_ALPN).await.map(|_| "ok"));
    Ok(())
}
