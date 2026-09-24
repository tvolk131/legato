//! Length-prefixed postcard frames over QUIC streams.

use anyhow::{Context, Result};
use iroh::endpoint::{RecvStream, SendStream};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub async fn write_frame<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    send.write_all(&legato_proto::encode_frame(msg))
        .await
        .context("writing frame")
}

/// Reads one frame. Returns `None` if the peer finished the stream cleanly between frames.
pub async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    let mut filled = 0;
    while filled < len.len() {
        match recv
            .read(&mut len[filled..])
            .await
            .context("reading frame")?
        {
            None if filled == 0 => return Ok(None),
            None => anyhow::bail!("stream ended mid-frame"),
            Some(n) => filled += n,
        }
    }
    let len = legato_proto::check_frame_len(u32::from_le_bytes(len))?;
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload)
        .await
        .context("stream ended mid-frame")?;
    Ok(Some(legato_proto::decode_frame(&payload)?))
}

/// Reads one frame, treating the end of the stream as an error.
pub async fn expect_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T> {
    read_frame(recv).await?.context("peer closed the stream")
}
