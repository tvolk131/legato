//! End to end on the Mac: the real `legato run` binary, driven by a fake controller over
//! iroh. Moves the cursor (and puts it back), so it's ignored by default:
//! `cargo test -p legato-cli --test receive_e2e -- --ignored`.
#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::time::Duration;

use legato_net::{Net, NetConfig, SessionEvent, Store};
use legato_proto::{Control, Datagram, Hello, Os, Point, Screens};
use tokio::time::timeout;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("legato-e2e-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[tokio::test]
#[ignore = "moves the cursor"]
async fn legato_run_injects_what_the_controller_sends() {
    assert!(
        legato_macos::Permissions::check().all_granted(),
        "needs Accessibility access"
    );
    let (mac_dir, pc_dir) = (temp_dir("mac"), temp_dir("pc"));
    // Pair the two identities by writing each other into their stores.
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
    assert_eq!(session.remote.os, Os::MacOs);
    assert_eq!(session.remote.screens, legato_macos::screens());

    let original = legato_macos::cursor_position();
    let main = legato_macos::screens()
        .displays
        .into_iter()
        .find(|d| d.primary)
        .unwrap()
        .bounds;
    let start = Point::new(main.x + 200.0, main.y + 200.0);
    assert!(session.send(Control::Enter { seq: 1, pos: start }));
    for i in 1..=10u32 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        session.send_datagram(&Datagram::Motion {
            seq: 1 + i,
            pos: Point::new(start.x + 10.0 * i as f64, start.y),
        });
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let landed = legato_macos::cursor_position();

    // Put the cursor back and let go before asserting.
    session.send(Control::Enter {
        seq: 100,
        pos: original,
    });
    session.send(Control::Leave);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = child.start_kill();
    pc.shutdown().await;

    assert!(
        (landed.x - (start.x + 100.0)).abs() < 1.0 && (landed.y - start.y).abs() < 1.0,
        "cursor at {landed:?}, expected {:?}",
        Point::new(start.x + 100.0, start.y)
    );
}
