//! End to end on the Mac: the clipboard travels both ways between the real `legato run`
//! and a fake peer. Uses the real clipboard, restoring its text afterwards, so it's ignored
//! by default: `cargo test -p legato-cli --test clipboard_e2e -- --ignored`.
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use legato_net::{Net, NetConfig, SessionEvent, Store};
use legato_proto::{ClipboardContent, Hello, Os, Screens, blob};
use tokio::time::timeout;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("legato-clip-e2e-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

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

    let (mac_dir, pc_dir) = (temp_dir("mac"), temp_dir("pc"));
    let mac_id = Store::open(&mac_dir).unwrap().identity().unwrap().public();
    let pc_id = Store::open(&pc_dir).unwrap().identity().unwrap().public();
    Store::open(&mac_dir)
        .unwrap()
        .add_peer(pc_id, "Fake PC".into(), Os::Windows)
        .unwrap();
    Store::open(&pc_dir)
        .unwrap()
        .add_peer(mac_id, "Mac".into(), Os::MacOs)
        .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_legato"))
        .args(["--home", mac_dir.to_str().unwrap(), "run"])
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pc = Net::start(NetConfig {
        store_dir: Some(pc_dir),
        name: "Fake PC".into(),
        os: Os::Windows,
        app_version: "test".into(),
        relays: false,
        mdns: true,
    })
    .await
    .unwrap();
    let mut events = pc.start_sessions(Hello {
        protocol: legato_proto::PROTOCOL_VERSION,
        app_version: "test".into(),
        name: "Fake PC".into(),
        os: Os::Windows,
        screens: Screens {
            displays: vec![],
            native_per_desk: 1.0,
        },
    });
    let session = loop {
        match timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("no session")
            .unwrap()
        {
            SessionEvent::Connected(s) => break s,
            _ => continue,
        }
    };

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
