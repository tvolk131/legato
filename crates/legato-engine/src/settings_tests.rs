use std::sync::mpsc;

use super::*;

#[tokio::test]
async fn concurrent_settings_edits_keep_every_change_and_match_disk() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::start(Some(dir.path().into()), "test")
        .await
        .unwrap();
    let before = engine.config().extend.bitrate_mbps;
    let mut changes = engine.config_updates();
    let barrier = std::sync::Barrier::new(12);
    std::thread::scope(|scope| {
        for _ in 0..12 {
            let engine = &engine;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..3 {
                    engine
                        .update_config(|c| c.extend.bitrate_mbps += 1)
                        .unwrap();
                }
            });
        }
    });
    assert_eq!(engine.config().extend.bitrate_mbps, before + 36);
    assert!(changes.has_changed().unwrap());
    assert_eq!(*changes.borrow_and_update(), engine.config());
    assert_eq!(config::load(dir.path()).unwrap(), engine.config());
}

#[tokio::test]
async fn old_peer_policies_cannot_replace_newer_committed_settings() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::start(Some(dir.path().into()), "test")
        .await
        .unwrap();
    let mode = |controller: &str, updated_at| legato_proto::ControlMode {
        controller: Some(controller.into()),
        updated_at,
    };
    engine.apply_control_mode(mode("first", 10)).unwrap();
    // The sharing loop could still have the old snapshot while a local edit commits.
    engine
        .update_config(|c| {
            c.control.controller = Some("local".into());
            c.control.updated_at = 30;
        })
        .unwrap();
    let changes = engine.config_updates();
    engine.apply_control_mode(mode("stale", 20)).unwrap();
    assert!(!changes.has_changed().unwrap());
    assert_eq!(engine.config().control.controller.as_deref(), Some("local"));
    engine.apply_control_mode(mode("newest", 40)).unwrap();
    assert!(changes.has_changed().unwrap());
    assert_eq!(
        engine.config().control.controller.as_deref(),
        Some("newest")
    );
    assert_eq!(config::load(dir.path()).unwrap(), engine.config());
}

#[tokio::test]
async fn a_failed_save_does_not_publish_or_change_the_committed_config() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::start(Some(dir.path().into()), "test")
        .await
        .unwrap();
    engine
        .update_config(|c| c.scrolling.invert_wheel = true)
        .unwrap();
    let before = engine.config();
    let changes = engine.config_updates();
    let path = config::path(dir.path());
    // A directory at the destination forces persist to fail on every supported OS.
    let old = dir.path().join("previous.toml");
    std::fs::rename(&path, &old).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep"), "untouched").unwrap();
    assert!(
        engine
            .update_config(|c| c.clipboard.enabled = false)
            .is_err()
    );
    assert_eq!(engine.config(), before);
    assert!(!changes.has_changed().unwrap());
    assert_eq!(
        std::fs::read_to_string(path.join("keep")).unwrap(),
        "untouched"
    );
    std::fs::remove_dir_all(&path).unwrap();
    std::fs::rename(old, path).unwrap();
    assert_eq!(config::load(dir.path()).unwrap(), before);
    assert!(
        !std::fs::read_dir(dir.path()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp"))
    );
}

#[test]
fn replacing_settings_preserves_open_readers_and_publishes_the_new_file() {
    use std::io::Read;

    let dir = tempfile::tempdir().unwrap();
    let before = Config::default();
    config::save(dir.path(), &before).unwrap();
    let mut reader = std::fs::File::open(config::path(dir.path())).unwrap();
    let mut after = before.clone();
    after.extend.bitrate_mbps += 1;
    // Keep the old file open throughout the replacement, not just opportunistically
    // as in the concurrent-reader test. Old and new readers see complete versions.
    config::save(dir.path(), &after).unwrap();
    assert_eq!(config::load(dir.path()).unwrap(), after);
    let mut old_text = String::new();
    reader.read_to_string(&mut old_text).unwrap();
    assert_eq!(toml::from_str::<Config>(&old_text).unwrap(), before);
    drop(reader);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
        let attrs = std::fs::metadata(config::path(dir.path()))
            .unwrap()
            .file_attributes();
        assert_eq!(attrs & FILE_ATTRIBUTE_TEMPORARY, 0);
    }
}

#[test]
fn readers_only_see_complete_settings_during_atomic_replacements() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.control.controller = Some("x".repeat(16_384));
    let initial = config.clone();
    config::save(dir.path(), &config).unwrap();
    let path = dir.path();
    std::thread::scope(|scope| {
        // Channel disconnection releases either side if the other panics. A barrier
        // and stop flag can leave scope waiting forever, hiding the actual failure.
        let (started, ready) = mpsc::channel();
        let (stop, stopping) = mpsc::channel::<()>();
        let reader = scope.spawn(move || {
            assert_eq!(config::load(path).unwrap(), initial);
            started.send(()).unwrap();
            while matches!(stopping.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                let snapshot = config::load(path).unwrap();
                assert!(
                    snapshot
                        .control
                        .controller
                        .is_some_and(|s| s.len() == 16_384)
                );
            }
        });
        ready.recv().unwrap();
        for n in 0..20 {
            config.control.updated_at = n;
            config.control.controller = Some("x".repeat(16_384));
            config::save(dir.path(), &config).unwrap();
        }
        drop(stop);
        reader.join().unwrap();
    });
    assert_eq!(config::load(dir.path()).unwrap(), config);
}
