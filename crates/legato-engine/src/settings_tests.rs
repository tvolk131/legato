use std::sync::atomic::{AtomicBool, Ordering};

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
fn readers_only_see_complete_settings_during_atomic_replacements() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.control.controller = Some("x".repeat(16_384));
    let initial = config.clone();
    config::save(dir.path(), &config).unwrap();
    let stop = AtomicBool::new(false);
    let started = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            assert_eq!(config::load(dir.path()).unwrap(), initial);
            started.wait();
            while !stop.load(Ordering::Acquire) {
                let snapshot = config::load(dir.path()).unwrap();
                assert!(
                    snapshot
                        .control
                        .controller
                        .is_some_and(|s| s.len() == 16_384)
                );
            }
        });
        started.wait();
        for n in 0..20 {
            config.control.updated_at = n;
            config.control.controller = Some("x".repeat(16_384));
            config::save(dir.path(), &config).unwrap();
        }
        stop.store(true, Ordering::Release);
        reader.join().unwrap();
    });
    assert_eq!(config::load(dir.path()).unwrap(), config);
}
