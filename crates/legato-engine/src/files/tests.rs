use std::io::Write;
use std::time::Duration;

use legato_net::{Net, NetConfig, Session, SessionEvent};
use legato_proto::{Hello, Os, Screens};
use tokio::time::timeout;

use super::*;

fn peer(n: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[n; 32]).public()
}

fn header(name: &str, batch: u64, count: u32) -> FileHeader {
    FileHeader {
        batch,
        name: name.into(),
        size: 5,
        count,
        index: 0,
        purpose: FilePurpose::Send,
    }
}

fn inbox(
    root: &Path,
) -> (
    Inbox,
    mpsc::UnboundedReceiver<Result<(IncomingInfo, PathBuf)>>,
) {
    Inbox::with_roots(root.join("downloads"), root.join("clipboard"))
}

#[test]
fn refused_names_leave_no_files_or_pending_batch() {
    let root = tempfile::tempdir().unwrap();
    let (mut inbox, _) = inbox(root.path());
    assert!(inbox.reserve(peer(1), &header("../escape", 1, 1)).is_err());
    assert!(inbox.batches.is_empty());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn colliding_files_and_nested_directories_keep_their_contents_and_shape() {
    let root = tempfile::tempdir().unwrap();
    let (mut inbox, _done) = inbox(root.path());
    let names = [
        "a?.txt",
        "a*.txt",
        "folder?/nested?/a?.txt",
        "folder?/nested?/a*.txt",
        "folder?/nested*/b.txt",
        "folder*/nested?/a.txt",
        "Case.txt",
        "case.txt",
    ];
    let mut saved = Vec::new();
    let mut received = None;
    for (n, name) in names.iter().enumerate() {
        let h = header(name, 1, names.len() as u32);
        let mut destination = inbox.reserve(peer(1), &h).unwrap();
        destination.file.write_all(name.as_bytes()).unwrap();
        saved.push(destination.path);
        received = inbox.finished(&IncomingInfo {
            from: peer(1),
            header: h,
        });
        assert_eq!(received.is_some(), n == names.len() - 1);
    }
    for (name, path) in names.iter().zip(&saved) {
        assert_eq!(fs::read_to_string(path).unwrap(), *name);
    }
    assert_ne!(saved[0], saved[1]);
    assert_eq!(
        saved[2].parent(),
        saved[3].parent(),
        "one original directory stays together"
    );
    assert_ne!(
        saved[2].parent(),
        saved[4].parent(),
        "colliding nested directories stay separate"
    );
    assert_ne!(
        saved[2].parent().unwrap().parent(),
        saved[5].parent().unwrap().parent()
    );
    assert_eq!(
        received.unwrap().paths.len(),
        6,
        "four loose files and two folders"
    );
}

#[test]
fn simultaneous_transfers_reserve_distinct_names_without_touching_existing_files() {
    let root = tempfile::tempdir().unwrap();
    let downloads = root.path().join("downloads");
    fs::create_dir(&downloads).unwrap();
    fs::write(downloads.join("report.txt"), "existing").unwrap();
    let barrier = std::sync::Barrier::new(8);
    let saved = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let (mut inbox, _) = inbox(root.path());
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    let mut dest = inbox.reserve(peer(1), &header("report.txt", n, 1)).unwrap();
                    dest.file.write_all(&[n as u8]).unwrap();
                    (dest.path, n as u8)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    for (path, byte) in saved {
        assert_eq!(fs::read(path).unwrap(), [byte]);
    }
    assert_eq!(
        fs::read_to_string(downloads.join("report.txt")).unwrap(),
        "existing"
    );
}

#[test]
fn batch_ids_are_scoped_to_the_sender_for_downloads_and_clipboard() {
    let root = tempfile::tempdir().unwrap();
    for purpose in [FilePurpose::Send, FilePurpose::Clipboard] {
        let (mut inbox, _) = inbox(root.path());
        let h = FileHeader {
            purpose,
            ..header("same.txt", 7, 1)
        };
        let a = inbox.reserve(peer(1), &h).unwrap();
        let b = inbox.reserve(peer(2), &h).unwrap();
        assert_ne!(a.path, b.path);
        for (from, path) in [(peer(1), a.path), (peer(2), b.path)] {
            let received = inbox
                .finished(&IncomingInfo {
                    from,
                    header: h.clone(),
                })
                .unwrap();
            assert_eq!(received.from, from);
            assert_eq!(received.paths, [path]);
        }
    }
}

async fn next(events: &mut mpsc::UnboundedReceiver<SessionEvent>) -> SessionEvent {
    timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap()
}

/// Exercise the real network stream, destination reservation, save and completion path.
#[tokio::test]
async fn receiving_colliding_names_preserves_both_files_and_cleans_up_failed_receives() {
    let root = tempfile::tempdir().unwrap();
    let mut nodes = Vec::new();
    for name in ["sender", "receiver"] {
        nodes.push(
            Net::start(NetConfig {
                store_dir: Some(root.path().join(name)),
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
    let mut listening = b.listen_for_pairing();
    let attempt = a.pair(b.addr()).await.unwrap();
    let incoming = timeout(Duration::from_secs(10), listening.recv())
        .await
        .unwrap()
        .unwrap();
    let (x, y) = tokio::join!(attempt.decide(true), incoming.decide(true));
    x.unwrap();
    y.unwrap();
    a.remember(b.addr());
    b.remember(a.addr());
    let hello = |n: &Net| Hello {
        protocol: legato_proto::PROTOCOL_VERSION,
        app_version: "test".into(),
        name: n.config().name.clone(),
        os: Os::MacOs,
        screens: Screens {
            displays: vec![],
            native_per_desk: 1.0,
        },
    };
    let mut ae = a.start_sessions(hello(a));
    let mut be = b.start_sessions(hello(b));
    let sender: Arc<Session> = match next(&mut ae).await {
        SessionEvent::Connected(s) => s,
        e => panic!("{e:?}"),
    };
    assert!(matches!(next(&mut be).await, SessionEvent::Connected(_)));
    let (mut inbox, mut done) = inbox(root.path());
    let mut received = None;
    for (index, name, contents, size) in [
        (0, "a?.txt", "FIRST", 5),
        (1, "a*.txt", "SECOND", 6),
        (2, "a_.txt", "SHORT", 20),
    ] {
        let h = FileHeader {
            index,
            size,
            ..header(name, 1, 2)
        };
        let session = sender.clone();
        let sending =
            tokio::spawn(async move { session.send_file(h, &mut contents.as_bytes()).await });
        let file = match next(&mut be).await {
            SessionEvent::File { file, .. } => file,
            e => panic!("{e:?}"),
        };
        inbox.receive(a.id(), file);
        let result = timeout(Duration::from_secs(10), done.recv())
            .await
            .unwrap()
            .unwrap();
        if index < 2 {
            let (info, _) = result.unwrap();
            received = inbox.finished(&info);
            sending.await.unwrap().unwrap();
        } else {
            assert!(result.is_err());
            assert!(sending.await.unwrap().is_err());
        }
    }
    let received = received.unwrap();
    assert_eq!(received.paths.len(), 2);
    let mut contents: Vec<_> = received
        .paths
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    contents.sort();
    assert_eq!(contents, ["FIRST", "SECOND"]);
    assert_eq!(
        fs::read_dir(root.path().join("downloads")).unwrap().count(),
        2,
        "only the failed transfer's partial file is removed"
    );
    drop(sender);
    for node in nodes {
        node.shutdown().await;
    }
}
