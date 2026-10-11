//! Starts the real CLI with a paired, discoverable fake Windows peer.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use legato_net::{Net, NetConfig, Session, SessionEvent, Store};
use legato_proto::{Hello, Os, Screens};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

pub struct Peer {
    pub child: tokio::process::Child,
    pub net: Net,
    pub events: UnboundedReceiver<SessionEvent>,
    pub session: Arc<Session>,
}

pub async fn start(tag: &str) -> Peer {
    let temp_dir = |side: &str| -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("legato-e2e-{}-{tag}-{side}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
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
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_legato"))
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

    Peer {
        child,
        net: pc,
        events,
        session,
    }
}
