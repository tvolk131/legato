use super::*;
use serde_json::json;

#[tokio::test]
async fn cancel_pairing_aborts_network_work_and_invalidates_stale_completion() {
    for stage in [Stage::Connecting, Stage::Confirm, Stage::Waiting] {
        let task = tokio::spawn(std::future::pending::<()>());
        let mut state = State {
            pairing: Some(Pairing {
                id: "peer".into(),
                name: "Peer".into(),
                code: String::new(),
                incoming: false,
                stage,
            }),
            pairing_generation: 4,
            pairing_task: Some(task.abort_handle()),
            ..State::default()
        };
        assert!(state.cancel_pairing().is_none());
        assert!(state.pairing.is_none());
        assert!(state.pairing_task.is_none());
        assert_eq!(state.pairing_generation, 5);
        assert!(task.await.unwrap_err().is_cancelled());
    }
}

#[test]
fn settings_edit_only_the_requested_field_and_reject_invalid_values() {
    let mut c = Config::default();
    c.control.controller = Some("peer".into());
    setting(&mut c, "clipboard", json!(false)).unwrap();
    assert!(!c.clipboard.enabled);
    assert_eq!(c.control.controller.as_deref(), Some("peer"));
    assert!(setting(&mut c, "push_distance", json!(-1)).is_err());
    assert!(setting(&mut c, "remap", json!("auto")).is_err());
    setting(&mut c, "remap", json!(false)).unwrap();
    assert_eq!(c.keys.remap, Remap::None);
    setting(&mut c, "controller", Value::Null).unwrap();
    let before = c.control.updated_at;
    setting(&mut c, "controller", json!("local")).unwrap();
    assert!(c.control.updated_at > before);
}

#[test]
fn rejects_unknown_commands_and_null_input() {
    assert!(
        serde_json::from_str::<Command>(
            r#"{"action":"setting","key":"clipboard","value":true,"typo":1}"#
        )
        .is_err()
    );
    // SAFETY: null is explicitly supported and returns an error object.
    let p = unsafe { legato_command(std::ptr::null()) };
    // SAFETY: p is an owned C string returned by the bridge.
    let value: Value =
        serde_json::from_str(unsafe { CStr::from_ptr(p) }.to_str().unwrap()).unwrap();
    assert_eq!(value["ok"], false);
    // SAFETY: returns this allocation exactly once.
    unsafe { legato_free(p) };
}

#[test]
fn notices_keep_the_latest_and_failures_are_not_repeated() {
    let mut s = State::default();
    for i in 0..MAX_NOTICES + 5 {
        s.notify(format!("notice {i}"));
    }
    assert_eq!(s.notices.len(), MAX_NOTICES);
    assert_eq!(s.notices.last().unwrap().id, MAX_NOTICES as u64 + 5);
    s.failed("couldn't send".into());
    s.failed("couldn't send".into());
    assert_eq!(s.failures.len(), 1);
}

/// The JSON contract with Swift. `native/macos` decodes this exact file in its tests, so
/// a change here that Swift can't read fails there. Regenerate it with
/// `LEGATO_UPDATE_FIXTURES=1 cargo test -p legato-swift-bridge`.
#[test]
fn snapshot_fixture_matches_the_swift_contract() {
    let snapshot = Snapshot {
        ready: true,
        error: None,
        version: "0.3.0-alpha.30".into(),
        id: "mac".into(),
        name: "MacBook Pro".into(),
        state_dir: "/tmp/legato-fixture".into(),
        sharing: true,
        settings: settings(&Config::default()),
        paired: vec![
            Device {
                id: "pc".into(),
                name: "Windows PC".into(),
                os: Some(os_name(Os::Windows)),
                connected: true,
                path: Some("direct · 2 ms".into()),
                active: true,
            },
            Device {
                id: "laptop".into(),
                name: "Travel laptop".into(),
                os: Some(os_name(Os::MacOs)),
                connected: false,
                path: None,
                active: false,
            },
        ],
        nearby: vec![Device {
            id: "new".into(),
            name: "Nearby PC".into(),
            os: None,
            connected: false,
            path: None,
            active: false,
        }],
        pairing: Some(Pairing {
            id: "new".into(),
            name: "Nearby PC".into(),
            code: "042916".into(),
            incoming: true,
            stage: Stage::Confirm,
        }),
        notice: Some(Notice {
            id: 3,
            text: "Sent 2 MB to \"Windows PC\".".into(),
        }),
        failures: vec!["Couldn't send files to \"Windows PC\": closed".into()],
        reveal: Some(Reveal {
            sequence: 1,
            paths: vec!["/Users/me/Downloads/Legato/a.txt".into()],
        }),
        arrangement: Arrangement {
            local: vec![Rect::new(0.0, 0.0, 1728.0, 1117.0)],
            local_primary: Some(0),
            peers: vec![ArrangedPeer {
                id: "pc".into(),
                name: "Windows PC".into(),
                placed: true,
                displays: vec![Rect::new(-416.0, -1440.0, 2560.0, 1440.0)],
                primary: Some(0),
            }],
            problems: vec![
                "Waiting to connect to \"Travel laptop\" once to learn its displays.".into(),
            ],
        },
    };
    let json = serde_json::to_string_pretty(&snapshot).unwrap() + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../native/macos/Tests/LegatoTests/Fixtures/snapshot.json");
    if std::env::var_os("LEGATO_UPDATE_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &json).unwrap();
    }
    let saved = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        saved, json,
        "the Swift fixture is out of date; regenerate it (see this test's docs)"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pairing_requests_are_accepted_only_while_adding_a_device() {
    use legato_net::{Net, NetConfig};
    use legato_proto::Os;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let mut nodes = Vec::new();
    for name in ["a", "b"] {
        nodes.push(
            Net::start(NetConfig {
                store_dir: Some(dir.path().join(name)),
                name: name.into(),
                os: Os::MacOs,
                app_version: "test".into(),
                relays: false,
                mdns: false,
            })
            .await
            .unwrap(),
        );
    }
    let (a, b) = (&nodes[0], &nodes[1]);
    let state = Shared::default();
    let refused = async || {
        let err = a.pair(b.addr()).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("not in pairing mode"),
            "{err:#}"
        );
    };

    // Closed until the sheet opens.
    refused().await;

    listen_for_pairing(&state, b, true);
    listen_for_pairing(&state, b, true); // Opening twice keeps one listener.
    let theirs = a.pair(b.addr()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while lock(&state).attempt.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pairing = lock(&state).pairing.clone().unwrap();
    assert!(pairing.incoming && pairing.name == "a");

    // Closing the sheet refuses new requests but keeps the one being confirmed.
    listen_for_pairing(&state, b, false);
    assert!(lock(&state).listening.is_none());
    assert!(lock(&state).attempt.is_some());
    drop(lock(&state).cancel_pairing());
    drop(theirs);
    refused().await;
}
