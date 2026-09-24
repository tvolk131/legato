use std::time::Duration;

use legato_proto::{Control, Datagram, Hello, Os, Point, Screens};
use tokio::time::timeout;

use super::*;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "legato-net-test-{}-{tag}-{}",
        std::process::id(),
        iroh::SecretKey::generate().public().fmt_short()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A node that only talks over loopback: no relays, no mDNS.
async fn node(name: &str, os: Os) -> Net {
    Net::start(NetConfig {
        store_dir: Some(temp_dir(name)),
        name: name.into(),
        os,
        app_version: "test".into(),
        relays: false,
        mdns: false,
    })
    .await
    .unwrap()
}

fn hello(net: &Net) -> Hello {
    Hello {
        protocol: legato_proto::PROTOCOL_VERSION,
        app_version: "test".into(),
        name: net.config().name.clone(),
        os: net.config().os,
        screens: Screens {
            displays: vec![],
            native_per_desk: 1.0,
        },
    }
}

async fn pair(a: &Net, b: &Net, a_accepts: bool, b_accepts: bool) -> (PairOutcome, PairOutcome) {
    let mut incoming = b.listen_for_pairing();
    let attempt = a.pair(b.addr()).await.unwrap();
    let theirs = timeout(Duration::from_secs(5), incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempt.code, theirs.code, "both sides show the same code");
    assert!(!attempt.incoming && theirs.incoming);
    assert_eq!(theirs.peer.name, a.config().name);
    assert_eq!(attempt.peer.os, b.config().os);
    let (x, y) = tokio::join!(attempt.decide(a_accepts), theirs.decide(b_accepts));
    (x.unwrap(), y.unwrap())
}

#[tokio::test]
async fn pairing_requires_both_sides_to_accept() {
    let (a, b) = (node("a", Os::Windows).await, node("b", Os::MacOs).await);

    let (x, y) = pair(&a, &b, true, false).await;
    assert_eq!(x, PairOutcome::DeclinedThere);
    assert_eq!(y, PairOutcome::DeclinedHere);
    assert!(a.paired_peers().is_empty() && b.paired_peers().is_empty());

    let (x, y) = pair(&a, &b, true, true).await;
    assert!(matches!(x, PairOutcome::Paired(ref p) if p.id == b.id() && p.name == "b"));
    assert!(matches!(y, PairOutcome::Paired(ref p) if p.id == a.id() && p.os == Os::Windows));
    assert_eq!(b.store().peer(&a.id()).unwrap().name, "a");
}

#[tokio::test]
async fn pairing_is_refused_unless_listening() {
    let (a, b) = (node("a", Os::Windows).await, node("b", Os::MacOs).await);
    let err = a.pair(b.addr()).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("not in pairing mode"),
        "{err:#}"
    );
}

#[tokio::test]
async fn sessions_to_unpaired_peers_are_refused_by_both_sides() {
    let (a, b) = (node("a", Os::Windows).await, node("b", Os::MacOs).await);
    let _events = b.start_sessions(hello(&b));

    // Our own hook won't even open a session to a peer we haven't paired with.
    let err = a
        .endpoint()
        .connect(b.addr(), legato_proto::SESSION_ALPN)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("rejected locally"), "{err:#}");

    // And if only one side thinks they're paired, the other side refuses.
    a.shared
        .add_paired(&PeerInfo {
            id: b.id(),
            name: "b".into(),
            os: Os::MacOs,
            app_version: "test".into(),
        })
        .unwrap();
    let conn = a
        .endpoint()
        .connect(b.addr(), legato_proto::SESSION_ALPN)
        .await
        .unwrap();
    let closed = timeout(Duration::from_secs(5), conn.closed())
        .await
        .unwrap();
    assert!(format!("{closed}").contains("not paired"), "{closed}");
}

async fn next_connected(events: &mut mpsc::UnboundedReceiver<SessionEvent>) -> Arc<Session> {
    match timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap()
    {
        SessionEvent::Connected(s) => s,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn paired_peers_connect_and_exchange_input() {
    let (a, b) = (node("a", Os::Windows).await, node("b", Os::MacOs).await);
    pair(&a, &b, true, true).await;
    a.remember(b.addr());
    b.remember(a.addr());

    let mut a_events = a.start_sessions(hello(&a));
    let mut b_events = b.start_sessions(hello(&b));
    let a_session = next_connected(&mut a_events).await;
    let b_session = next_connected(&mut b_events).await;
    assert_eq!(a_session.peer, b.id());
    assert_eq!(b_session.remote.name, "a");
    assert_eq!(a_session.path().map(|(k, _)| k), Some(PathKind::Direct));

    // Reliable control messages arrive in order.
    let msgs = [
        Control::Enter {
            seq: 1,
            pos: Point::new(864.0, 0.5),
        },
        Control::Key {
            usage: 0x04,
            down: true,
        },
        Control::Key {
            usage: 0x04,
            down: false,
        },
        Control::Leave,
    ];
    for msg in &msgs {
        assert!(a_session.send(msg.clone()));
    }
    for expected in &msgs {
        match timeout(Duration::from_secs(5), b_events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            SessionEvent::Control { peer, msg } => {
                assert_eq!(peer, a.id());
                assert_eq!(&msg, expected);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // Datagrams: loopback doesn't lose them, but they may still be reordered.
    let motion = Datagram::Motion {
        seq: 2,
        pos: Point::new(10.0, 20.0),
    };
    assert!(b_session.send_datagram(&motion));
    match timeout(Duration::from_secs(5), a_events.recv())
        .await
        .unwrap()
        .unwrap()
    {
        SessionEvent::Datagram { msg, .. } => assert_eq!(msg, motion),
        other => panic!("unexpected {other:?}"),
    }

    // Unpairing drops the session on both sides.
    assert!(a.unpair(&b.id()).unwrap());
    for events in [&mut a_events, &mut b_events] {
        match timeout(Duration::from_secs(10), events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            SessionEvent::Disconnected { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn adverts_round_trip_and_fit() {
    let mut config = NetConfig::for_this_machine("test");
    config.os = Os::MacOs;
    config.name = "Tommy's MacBook Pro | work".into();
    let text = advert(&config);
    assert_eq!(
        parse_advert(&text),
        Some((Some(Os::MacOs), "Tommy's MacBook Pro | work".into()))
    );
    config.name = "é".repeat(500);
    assert!(advert(&config).len() <= 245);
    assert_eq!(parse_advert("irohv1|mac|x"), None);
}

#[tokio::test]
async fn blobs_and_files_arrive_intact() {
    let (a, b) = (node("a", Os::Windows).await, node("b", Os::MacOs).await);
    pair(&a, &b, true, true).await;
    a.remember(b.addr());
    b.remember(a.addr());
    let _a_events = a.start_sessions(hello(&a));
    let mut b_events = b.start_sessions(hello(&b));
    let mut a_events = _a_events;
    let a_session = next_connected(&mut a_events).await;
    let _b_session = next_connected(&mut b_events).await;

    let blob: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
    a_session.send_blob(legato_proto::blob::CLIPBOARD, blob.clone());
    match timeout(Duration::from_secs(10), b_events.recv())
        .await
        .unwrap()
        .unwrap()
    {
        SessionEvent::Blob { tag, data, peer } => {
            assert_eq!(peer, a.id());
            assert_eq!(tag, legato_proto::blob::CLIPBOARD);
            assert_eq!(data, blob);
        }
        other => panic!("unexpected {other:?}"),
    }

    let contents: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7) as u8).collect();
    let header = legato_proto::FileHeader {
        batch: 1,
        name: "folder/photo.jpg".into(),
        size: contents.len() as u64,
        count: 1,
        index: 0,
        purpose: legato_proto::FilePurpose::Send,
    };
    let sender = {
        let session = a_session.clone();
        let header = header.clone();
        let contents = contents.clone();
        tokio::spawn(async move { session.send_file(header, &mut contents.as_slice()).await })
    };
    let file = match timeout(Duration::from_secs(10), b_events.recv())
        .await
        .unwrap()
        .unwrap()
    {
        SessionEvent::File { file, .. } => file,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(file.header, header);
    let mut received = Vec::new();
    assert_eq!(
        file.recv(&mut received).await.unwrap(),
        contents.len() as u64
    );
    assert_eq!(received, contents);
    sender.await.unwrap().unwrap();
}

#[tokio::test]
async fn video_frames_arrive_in_order() {
    let (a, b) = (node("a", Os::MacOs).await, node("b", Os::Windows).await);
    pair(&a, &b, true, true).await;
    a.remember(b.addr());
    b.remember(a.addr());
    let mut a_events = a.start_sessions(hello(&a));
    let mut b_events = b.start_sessions(hello(&b));
    let a_session = next_connected(&mut a_events).await;
    let _b_session = next_connected(&mut b_events).await;

    let frames: Vec<(Vec<u8>, bool)> = (0..20u32)
        .map(|i| (vec![i as u8; 1000 + i as usize * 5000], i % 10 == 0))
        .collect();
    let sender = {
        let frames = frames.clone();
        tokio::spawn(async move {
            let mut video = a_session.open_video().await.unwrap();
            for (data, keyframe) in &frames {
                video.send(data, *keyframe).await.unwrap();
            }
            video.finish();
        })
    };
    let video = match timeout(Duration::from_secs(10), b_events.recv())
        .await
        .unwrap()
        .unwrap()
    {
        SessionEvent::Video { peer, video } => {
            assert_eq!(peer, a.id());
            video
        }
        other => panic!("unexpected {other:?}"),
    };
    for expected in &frames {
        let got = timeout(Duration::from_secs(10), video.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(&got, expected);
    }
    assert_eq!(video.next().await.unwrap(), None, "the stream ends cleanly");
    sender.await.unwrap();
}
