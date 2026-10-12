use std::assert_matches;
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
        size: Some(5),
        count,
        index: 0,
        purpose: FilePurpose::Send,
    }
}

fn inbox(root: &Path) -> (Inbox, mpsc::UnboundedReceiver<Completion>) {
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
        let h = FileHeader {
            index: n as u32,
            ..header(name, 1, names.len() as u32)
        };
        let mut destination = inbox.reserve(peer(1), &h).unwrap();
        destination
            .file
            .as_mut()
            .unwrap()
            .write_all(name.as_bytes())
            .unwrap();
        saved.push(destination.path);
        received = inbox.finished(&IncomingInfo {
            from: peer(1),
            generation: inbox.batches[&(peer(1), h.batch)].generation,
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
                    dest.file.as_mut().unwrap().write_all(&[n as u8]).unwrap();
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
                    generation: inbox.batches[&(from, h.batch)].generation,
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

async fn connected(
    root: &Path,
) -> (
    Vec<Net>,
    Arc<Session>,
    mpsc::UnboundedReceiver<SessionEvent>,
) {
    let mut nodes = Vec::new();
    for name in ["sender", "receiver"] {
        nodes.push(
            Net::start(NetConfig {
                store_dir: Some(root.join(name)),
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
    assert_matches!(next(&mut be).await, SessionEvent::Connected(_));
    (nodes, sender, be)
}

/// Exercise the real network stream, destination reservation, save and completion path.
#[tokio::test]
async fn receiving_colliding_names_preserves_both_files_and_cleans_up_failed_receives() {
    let root = tempfile::tempdir().unwrap();
    let (nodes, sender, mut be) = connected(root.path()).await;
    let a = &nodes[0];
    let (mut inbox, mut done) = inbox(root.path());
    let mut received = None;
    for (index, name, contents, size) in [
        (0, "a?.txt", "FIRST", 5),
        (1, "a*.txt", "SECOND", 6),
        (2, "a_.txt", "SHORT", 20),
    ] {
        let h = FileHeader {
            index: if index < 2 { index } else { 0 },
            size: Some(size),
            ..header(
                name,
                if index < 2 { 1 } else { 2 },
                if index < 2 { 2 } else { 1 },
            )
        };
        let session = sender.clone();
        let sending =
            tokio::spawn(async move { session.send_file(h, &mut contents.as_bytes()).await });
        let file = match next(&mut be).await {
            SessionEvent::File { file, .. } => file,
            e => panic!("{e:?}"),
        };
        inbox.receive(a.id(), file);
        let (info, result) = timeout(Duration::from_secs(10), done.recv())
            .await
            .unwrap()
            .unwrap();
        if index < 2 {
            result.unwrap();
            received = inbox.finished(&info);
            sending.await.unwrap().unwrap();
        } else {
            assert!(result.is_err());
            inbox.failed(&info);
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
    assert!(inbox.batches.is_empty());
    drop(sender);
    for node in nodes {
        node.shutdown().await;
    }
}

struct FailingWriter {
    on_flush: bool,
}

impl tokio::io::AsyncWrite for FailingWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::task::Poll::Ready(if self.on_flush {
            Ok(bytes.len())
        } else {
            Err(io::Error::other("disk write failed"))
        })
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(if self.on_flush {
            Err(io::Error::other("disk flush failed"))
        } else {
            Ok(())
        })
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn failed_writes_flushes_reservations_and_missing_acknowledgments_never_report_sent() {
    let root = tempfile::tempdir().unwrap();
    let (nodes, sender, mut events) = connected(root.path()).await;
    for mode in 0..5 {
        let session = sender.clone();
        let sending = tokio::spawn(async move {
            session
                .send_file(header("test.txt", mode, 1), &mut b"hello".as_slice())
                .await
        });
        let file = match next(&mut events).await {
            SessionEvent::File { file, .. } => file,
            e => panic!("{e:?}"),
        };
        match mode {
            0 | 1 => {
                let (_keep, mut cancel) = watch::channel(());
                let result = receive_into(
                    &file,
                    &mut FailingWriter {
                        on_flush: mode == 1,
                    },
                    &mut cancel,
                )
                .await;
                assert!(result.is_err());
                file.complete(&result).await.unwrap();
            }
            2 => {
                let (mut inbox, mut done) = Inbox::with_roots(
                    root.path().join("not-a-directory"),
                    root.path().join("clipboard"),
                );
                fs::write(&inbox.downloads, "existing file").unwrap();
                inbox.receive(nodes[0].id(), file.clone());
                let (info, result) = timeout(Duration::from_secs(5), done.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(result.is_err());
                inbox.failed(&info);
                assert!(inbox.batches.is_empty());
                assert_eq!(
                    fs::read_to_string(&inbox.downloads).unwrap(),
                    "existing file"
                );
            }
            3 => {
                file.recv(&mut Vec::new()).await.unwrap();
                drop(file); // Bytes arrived, but the application never confirms the save.
            }
            4 => sender.close(),
            _ => unreachable!(),
        }
        let result = timeout(Duration::from_secs(5), sending)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err(), "mode {mode} must not report sent");
        if mode == 1 {
            assert!(format!("{:#}", result.unwrap_err()).contains("disk flush failed"));
        }
    }
    drop(sender);
    for node in nodes {
        node.shutdown().await;
    }
}

#[tokio::test]
async fn cancelling_or_disconnecting_a_batch_removes_partials_and_preserves_saved_files() {
    let root = tempfile::tempdir().unwrap();
    let (nodes, sender, mut events) = connected(root.path()).await;
    let from = nodes[0].id();
    for disconnect in [false, true] {
        let (mut inbox, mut done) = inbox(root.path());
        let batch = u64::from(disconnect) + 20;
        let session = sender.clone();
        let sending = tokio::spawn(async move {
            session
                .send_file(header("saved.txt", batch, 2), &mut b"hello".as_slice())
                .await
        });
        let file = match next(&mut events).await {
            SessionEvent::File { file, .. } => file,
            e => panic!("{e:?}"),
        };
        inbox.receive(from, file);
        let (saved_info, saved) = timeout(Duration::from_secs(5), done.recv())
            .await
            .unwrap()
            .unwrap();
        let saved = saved.unwrap();
        assert!(inbox.finished(&saved_info).is_none());
        sending.await.unwrap().unwrap();

        // Start a second stream, then stall its source partway through the file.
        let (mut writer, mut reader) = tokio::io::duplex(8);
        tokio::io::AsyncWriteExt::write_all(&mut writer, b"h")
            .await
            .unwrap();
        let session = sender.clone();
        let sending = tokio::spawn(async move {
            session
                .send_file(
                    FileHeader {
                        index: 1,
                        ..header("partial.txt", batch, 2)
                    },
                    &mut reader,
                )
                .await
        });
        let file = match next(&mut events).await {
            SessionEvent::File { file, .. } => file,
            e => panic!("{e:?}"),
        };
        inbox.receive(from, file);
        assert!(inbox.downloads.join("partial.txt").exists());
        if disconnect {
            inbox.disconnected(from);
        } else {
            inbox.cancel(from, batch);
        }
        let (info, result) = timeout(Duration::from_secs(5), done.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert!(
            inbox.finished(&info).is_none(),
            "never publish incomplete clipboard batches"
        );
        assert!(inbox.batches.is_empty());
        assert!(!inbox.downloads.join("partial.txt").exists());
        assert_eq!(fs::read(saved).unwrap(), b"hello");
        assert!(
            timeout(Duration::from_secs(5), sending)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        if !disconnect {
            assert!(
                inbox.reserve(from, &header("late.txt", batch, 2)).is_err(),
                "a late header cannot resurrect a cancelled batch"
            );
        }
        drop(writer);
    }
    drop(sender);
    for node in nodes {
        node.shutdown().await;
    }
}

#[tokio::test]
async fn a_source_error_after_one_saved_file_cancels_the_unfinished_batch() {
    let root = tempfile::tempdir().unwrap();
    let (nodes, sender, mut events) = connected(root.path()).await;
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let first = source.join("a.txt");
    let second = source.join("b.txt");
    fs::write(&first, "keep me").unwrap();
    fs::write(&second, "will disappear").unwrap();
    let session = sender.clone();
    let paths = vec![first, second.clone()];
    let sending = tokio::spawn(async move { send(&session, &paths, FilePurpose::Clipboard).await });
    let file = match next(&mut events).await {
        SessionEvent::File { file, .. } => file,
        e => panic!("{e:?}"),
    };
    fs::remove_file(second).unwrap(); // Manifest already made; sender is waiting for this save.
    let (mut inbox, mut done) = inbox(root.path());
    inbox.receive(nodes[0].id(), file);
    let (info, result) = timeout(Duration::from_secs(5), done.recv())
        .await
        .unwrap()
        .unwrap();
    let saved = result.unwrap();
    assert!(inbox.finished(&info).is_none());
    assert!(
        timeout(Duration::from_secs(5), sending)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    match next(&mut events).await {
        SessionEvent::Control {
            peer,
            msg: legato_proto::Control::FileCancel { batch },
        } => inbox.cancel(peer, batch),
        e => panic!("{e:?}"),
    }
    assert!(inbox.batches.is_empty());
    assert_eq!(fs::read_to_string(saved).unwrap(), "keep me");
    assert!(
        inbox.finished(&info).is_none(),
        "late completion must not publish a failed clipboard batch"
    );
    drop(sender);
    for node in nodes {
        node.shutdown().await;
    }
}

#[test]
fn a_reconnected_batch_ignores_old_completions_and_refuses_duplicate_indices() {
    let root = tempfile::tempdir().unwrap();
    let (mut inbox, _) = inbox(root.path());
    let from = peer(1);
    let h = header("a.txt", 1, 2);
    drop(inbox.reserve(from, &h).unwrap());
    let old = IncomingInfo {
        from,
        header: h.clone(),
        generation: inbox.batches[&(from, 1)].generation,
    };
    inbox.disconnected(from);
    drop(inbox.reserve(from, &h).unwrap());
    assert!(inbox.finished(&old).is_none());
    inbox.failed(&old);
    assert_eq!(inbox.batches[&(from, 1)].done, 0);
    assert!(inbox.reserve(from, &h).is_err());
    assert!(
        inbox
            .reserve(
                from,
                &FileHeader {
                    index: 1,
                    count: 3,
                    ..h.clone()
                }
            )
            .is_err()
    );
    assert!(inbox.reserve(from, &FileHeader { index: 2, ..h }).is_err());
    assert_eq!(inbox.batches[&(from, 1)].indices.len(), 1);
}

#[tokio::test]
async fn folders_keep_empty_directories_and_empty_files_for_every_purpose() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir_all(source.join("Mixed/empty/nested")).unwrap();
    fs::create_dir(source.join("Empty")).unwrap();
    fs::write(source.join("Mixed/file.txt"), "contents").unwrap();
    fs::write(source.join("Mixed/zero.txt"), "").unwrap();
    let (nodes, sender, mut events) = connected(root.path()).await;
    let (mut inbox, mut done) = inbox(root.path());
    // Existing destinations must remain separate, even when they are directories.
    fs::create_dir_all(inbox.downloads.join("Mixed")).unwrap();
    fs::write(inbox.downloads.join("Mixed/original.txt"), "original").unwrap();
    fs::create_dir(inbox.downloads.join("Empty")).unwrap();
    for purpose in [FilePurpose::Send, FilePurpose::Drop, FilePurpose::Clipboard] {
        let session = sender.clone();
        let paths = vec![source.join("Mixed"), source.join("Empty")];
        let sending = tokio::spawn(async move { send(&session, &paths, purpose).await });
        let received = loop {
            let file = match next(&mut events).await {
                SessionEvent::File { file, .. } => file,
                other => panic!("{other:?}"),
            };
            let directory = file.header.size.is_none();
            inbox.receive(nodes[0].id(), file);
            let (info, saved) = timeout(Duration::from_secs(5), done.recv())
                .await
                .unwrap()
                .unwrap();
            let path = saved.unwrap();
            assert_eq!(path.is_dir(), directory);
            if let Some(received) = inbox.finished(&info) {
                break received;
            }
        };
        assert_eq!(sending.await.unwrap().unwrap(), 8);
        assert_eq!(received.purpose, purpose);
        assert_eq!(received.paths.len(), 2);
        let mixed = received
            .paths
            .iter()
            .find(|p| p.join("file.txt").exists())
            .unwrap();
        assert!(mixed.join("empty/nested").is_dir());
        assert_eq!(fs::read_dir(mixed.join("empty/nested")).unwrap().count(), 0);
        assert!(mixed.join("zero.txt").is_file());
        assert_eq!(fs::metadata(mixed.join("zero.txt")).unwrap().len(), 0);
        assert_eq!(
            fs::read_to_string(mixed.join("file.txt")).unwrap(),
            "contents"
        );
        let empty = received.paths.iter().find(|p| *p != mixed).unwrap();
        assert!(empty.is_dir());
        assert_eq!(fs::read_dir(empty).unwrap().count(), 0);
        assert_eq!(
            fs::read_to_string(inbox.downloads.join("Mixed/original.txt")).unwrap(),
            "original"
        );
    }
    // An entirely empty selection is a successful, zero-byte batch.
    let session = sender.clone();
    let empty = source.join("Empty");
    let sending = tokio::spawn(async move { send(&session, &[empty], FilePurpose::Send).await });
    let file = match next(&mut events).await {
        SessionEvent::File { file, .. } => file,
        other => panic!("{other:?}"),
    };
    assert_eq!(file.header.size, None);
    assert_eq!(file.header.count, 1);
    inbox.receive(nodes[0].id(), file);
    let (info, saved) = timeout(Duration::from_secs(5), done.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(saved.unwrap().is_dir());
    assert_eq!(inbox.finished(&info).unwrap().paths.len(), 1);
    assert_eq!(sending.await.unwrap().unwrap(), 0);
    drop(sender);
    for node in nodes {
        node.shutdown().await;
    }
}

#[test]
fn directory_entries_share_reserved_parents_and_keep_colliding_names_distinct() {
    let root = tempfile::tempdir().unwrap();
    let (mut inbox, _) = inbox(root.path());
    let mut paths = Vec::new();
    for (index, name, size) in [
        (0, "folder?/child", None), // The parent is reserved implicitly first.
        (1, "folder?", None),
        (2, "folder*", None),
        (3, "folder?/zero", Some(0)),
    ] {
        let h = FileHeader {
            index,
            size,
            ..header(name, 1, 4)
        };
        paths.push(inbox.reserve(peer(1), &h).unwrap().path);
    }
    assert_eq!(paths[0].parent(), Some(paths[1].as_path()));
    assert_eq!(paths[3].parent(), Some(paths[1].as_path()));
    assert_ne!(paths[1], paths[2]);
    assert!(paths[3].is_file());
    assert_eq!(inbox.batches[&(peer(1), 1)].roots.len(), 2);
}
