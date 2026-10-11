//! End to end on the Mac: the clipboard travels both ways between the real `legato run`
//! and a fake peer. Uses the real clipboard, restoring its text afterwards, so it's ignored
//! by default: `cargo test -p legato-cli --test clipboard_e2e -- --ignored`.
#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use legato_net::SessionEvent;
use legato_proto::{ClipboardContent, blob};
use tokio::time::timeout;

mod support;

#[tokio::test]
#[ignore = "uses the clipboard (and puts its text back)"]
async fn clipboard_is_shared_both_ways() {
    let mut clipboard = arboard::Clipboard::new().unwrap();
    // Only run when nothing would be lost: the clipboard holds text (restored at the end)
    // or nothing at all.
    let original = match clipboard.get_text() {
        Ok(text) => Some(text),
        Err(_) if clipboard.get_image().is_err() && clipboard.get().file_list().is_err() => None,
        Err(_) => {
            eprintln!("the clipboard holds something other than text; not touching it");
            return;
        }
    };

    let support::Peer {
        mut child,
        net: pc,
        mut events,
        session,
    } = support::start("clipboard").await;

    // Copied on the "PC": appears on the Mac's clipboard.
    let from_pc = format!("copied on the PC {}", std::process::id());
    session.send_blob(
        blob::CLIPBOARD,
        postcard::to_stdvec(&ClipboardContent::Text(from_pc.clone())).unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut arrived = false;
    while Instant::now() < deadline {
        if clipboard.get_text().ok().as_deref() == Some(&from_pc) {
            arrived = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Copied on the Mac: arrives at the "PC".
    let from_mac = format!("copied on the Mac {}", std::process::id());
    clipboard.set_text(from_mac.clone()).unwrap();
    let received = timeout(Duration::from_secs(5), async {
        loop {
            if let Some(SessionEvent::Blob {
                tag: blob::CLIPBOARD,
                data,
                ..
            }) = events.recv().await
            {
                break postcard::from_bytes::<ClipboardContent>(&data).ok();
            }
        }
    })
    .await
    .ok()
    .flatten();

    match original {
        Some(text) => clipboard.set_text(text).unwrap(),
        None => clipboard.clear().unwrap(),
    }
    let _ = child.start_kill();
    pc.shutdown().await;

    assert!(arrived, "the PC's copy never reached the Mac's clipboard");
    assert_eq!(received, Some(ClipboardContent::Text(from_mac)));
}
