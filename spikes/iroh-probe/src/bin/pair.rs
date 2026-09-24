use anyhow::Result;
use iroh::{Endpoint, endpoint::{Connection, presets}};
use iroh_tickets::endpoint::EndpointTicket;

const PAIR_ALPN: &[u8] = b"legato/pair/1";

fn sas(conn: &Connection) -> Result<u32> {
    let mut out = [0u8; 4];
    conn.export_keying_material(&mut out, b"legato pairing SAS v1", b"")
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    Ok(u32::from_le_bytes(out) % 1_000_000)
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = Endpoint::builder(presets::Minimal).alpns(vec![PAIR_ALPN.to_vec()]).bind().await?;
    let b = Endpoint::bind(presets::Minimal).await?;
    let ticket = EndpointTicket::new(a.addr());
    let s = ticket.to_string();
    println!("ticket ({} chars): {s}", s.len());
    let parsed = s.parse::<EndpointTicket>()?;
    let acc = tokio::spawn({
        let a = a.clone();
        async move {
            let conn = a.accept().await.unwrap().await?;
            let code = sas(&conn)?;
            println!("A sees peer {} code {:06}", conn.remote_id().fmt_short(), code);
            conn.closed().await;
            anyhow::Ok(())
        }
    });
    let conn = b.connect(parsed.endpoint_addr().clone(), PAIR_ALPN).await?;
    println!("B sees peer {} code {:06}", conn.remote_id().fmt_short(), sas(&conn)?);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    conn.close(0u32.into(), b"");
    let _ = acc.await;
    Ok(())
}
