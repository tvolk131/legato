use super::*;
use std::time::Duration;

fn directory_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link).unwrap();
}

fn file_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, link).unwrap();
}

#[test]
fn internal_links_keep_their_names_and_repeated_targets_are_not_cycles() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("selected");
    fs::create_dir_all(root.join("folder")).unwrap();
    fs::write(root.join("folder/file.txt"), "contents").unwrap();
    directory_link(&root.join("folder"), &root.join("alias"));
    directory_link(&root.join("folder"), &root.join("another"));
    file_link(&root.join("folder/file.txt"), &root.join("file-link.txt"));
    let files = collect(std::slice::from_ref(&root)).unwrap();
    let names: Vec<_> = files.iter().map(|(_, name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "selected/alias/file.txt",
            "selected/another/file.txt",
            "selected/file-link.txt",
            "selected/folder/file.txt"
        ]
    );
    for (path, _, size) in files {
        assert_eq!(fs::read(path).unwrap(), b"contents");
        assert_eq!(size, 8);
    }
    // Selecting an alias itself is an explicit selection of that target.
    let link = temp.path().join("explicit");
    directory_link(&root, &link);
    assert!(
        collect(&[link])
            .unwrap()
            .iter()
            .all(|(_, name, _)| name.starts_with("explicit/"))
    );
    let link = temp.path().join("explicit-file.txt");
    file_link(&root.join("folder/file.txt"), &link);
    assert_eq!(collect(&[link]).unwrap()[0].1, "explicit-file.txt");
}

#[test]
fn ancestor_cycles_and_links_outside_the_selected_folder_fail_preflight() {
    for scenario in ["cycle", "directory", "file", "dangling"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("selected");
        let outside = temp.path().join("outside");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(root.join("safe.txt"), "safe").unwrap();
        fs::write(outside.join("private.txt"), "outside").unwrap();
        let link = root.join("nested/link");
        match scenario {
            "cycle" => directory_link(&root, &link),
            "directory" => directory_link(&outside, &link),
            "file" => file_link(&outside.join("private.txt"), &link),
            "dangling" => file_link(&outside.join("missing.txt"), &link),
            _ => unreachable!(),
        }
        let error = format!("{:#}", collect(&[root]).unwrap_err());
        assert!(error.contains("link"), "{error}");
        match scenario {
            "cycle" => assert!(error.contains("cycle"), "{error}"),
            "directory" | "file" => {
                assert!(error.contains("outside the selected folder"), "{error}")
            }
            "dangling" => assert!(error.contains("resolving"), "{error}"),
            _ => unreachable!(),
        }
    }
}

#[test]
fn changing_a_link_after_scanning_cannot_redirect_its_manifest_entry() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("selected");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("safe.txt"), "safe").unwrap();
    fs::write(temp.path().join("outside.txt"), "outside").unwrap();
    let link = root.join("alias.txt");
    file_link(&root.join("safe.txt"), &link);
    let files = collect(std::slice::from_ref(&root)).unwrap();
    fs::remove_file(&link).unwrap();
    file_link(&temp.path().join("outside.txt"), &link);
    let (path, _, _) = files
        .iter()
        .find(|(_, name, _)| name == "selected/alias.txt")
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), b"safe");
    assert!(collect(&[root]).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn a_blocked_folder_scan_does_not_block_the_async_runtime() {
    let (started, waiting) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let scan = prepare(FilePurpose::Send, move || {
        started.send(()).unwrap();
        // Bounded even if a regression runs this closure on the current-thread runtime.
        blocked
            .recv_timeout(Duration::from_secs(2))
            .context("async runtime was blocked by folder scanning")?;
        Ok(vec![(PathBuf::from("file"), "file".into(), 1)])
    });
    let input = async {
        waiting.await.unwrap();
        tokio::task::yield_now().await;
        release.send(()).is_ok()
    };
    let (files, progressed) = tokio::join!(scan, input);
    assert!(progressed);
    assert_eq!(files.unwrap().len(), 1);
}

#[tokio::test]
async fn clipboard_limits_apply_to_the_manifest_before_sending() {
    let scan = |sizes: Vec<u64>| {
        move || {
            Ok(sizes
                .into_iter()
                .map(|size| (PathBuf::from("file"), "file".into(), size))
                .collect())
        }
    };
    assert!(
        prepare(FilePurpose::Clipboard, scan(vec![MAX_CLIPBOARD_BATCH]))
            .await
            .is_ok()
    );
    let error = prepare(FilePurpose::Clipboard, scan(vec![MAX_CLIPBOARD_BATCH, 1]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("too big"));
    assert!(
        prepare(FilePurpose::Send, scan(vec![MAX_CLIPBOARD_BATCH, 1]))
            .await
            .is_ok()
    );
    assert!(
        prepare(FilePurpose::Drop, scan(vec![MAX_CLIPBOARD_BATCH, 1]))
            .await
            .is_ok()
    );
    assert!(
        prepare(FilePurpose::Clipboard, scan(vec![u64::MAX, 1]))
            .await
            .is_err()
    );
}
