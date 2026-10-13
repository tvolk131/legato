use iced::futures::StreamExt;
use std::assert_matches;
use std::time::Duration;
use tokio::time::timeout;

use super::*;

async fn app() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::start(Some(dir.path().into()), "test")
        .await
        .unwrap();
    (dir, App::new(engine))
}

#[tokio::test]
async fn transfer_failures_survive_navigation_and_success_until_dismissed() {
    let (_dir, mut app) = app().await;
    let peer = iroh::SecretKey::from_bytes(&[11; 32]).public();
    app.engine
        .net()
        .store()
        .add_peer(peer, "Studio PC".into(), legato_proto::Os::Windows)
        .unwrap();
    app.refresh_paired();
    let failure = Status::FileTransferFailed {
        peer,
        sending: true,
        reason: "Disk full".into(),
    };
    for _ in 0..2 {
        let _ = app.update(Message::Status(failure.clone()));
    }
    assert_eq!(app.model.transfer_failures.len(), 1);
    let text = app.model.transfer_failures[0].clone();
    assert!(text.contains("Disk full") && text.contains("Studio PC"));
    let _ = app.update(Message::Status(Status::FileTransferFailed {
        peer,
        sending: false,
        reason: "Permission denied".into(),
    }));
    let _ = app.update(Message::Status(Status::FilesSent {
        to: peer,
        bytes: 42,
    }));
    let _ = app.update(Message::DismissNotice);
    for page in [Page::Devices, Page::Settings, Page::Arrangement] {
        let _ = app.update(Message::Page(page));
        let mut ui = iced_test::simulator(view::root(&app.model));
        assert!(ui.find(text.clone()).unwrap().visible_bounds().is_some());
    }
    let _ = app.update(Message::DismissTransferFailure(text.clone()));
    assert_eq!(app.model.transfer_failures.len(), 1);
    assert!(app.model.transfer_failures[0].contains("receive files from"));
    assert!(app.model.problems.is_empty());
    // A fresh occurrence of an acknowledged failure must be shown again.
    let _ = app.update(Message::Status(failure));
    assert_eq!(app.model.transfer_failures.len(), 2);
    // Immediate send failures use the same persistent feedback.
    let _ = app.update(Message::FilesPicked(peer, vec!["missing-file".into()]));
    assert!(
        app.model
            .transfer_failures
            .iter()
            .any(|p| p.contains("sharing isn't running"))
    );
}

#[tokio::test]
async fn local_settings_preserve_a_newer_peer_policy_before_the_ui_refreshes() {
    let (dir, mut app) = app().await;
    let messages = [
        Message::Remap(false),
        Message::InvertWheel(true),
        Message::Clipboard(false),
        Message::Stats(true),
        Message::SaveSettings,
    ];
    for (n, message) in messages.into_iter().enumerate() {
        app.engine
            .update_config(|c| {
                c.control.controller = Some("peer".into());
                c.control.updated_at = 100 + n as u64;
            })
            .unwrap();
        if matches!(message, Message::SaveSettings) {
            let _ = app.update(Message::PushDistance(55.0));
        }
        let _ = app.update(message);
        let config = app.engine.config();
        assert_eq!(config.control.controller.as_deref(), Some("peer"));
        assert_eq!(config.control.updated_at, 100 + n as u64);
        assert_eq!(app.model.config, config);
        assert_eq!(legato_engine::config::load(dir.path()).unwrap(), config);
    }
    let config = app.engine.config();
    assert_eq!(config.keys.remap, Remap::None);
    assert!(config.scrolling.invert_wheel && config.extend.stats);
    assert!(!config.clipboard.enabled);
    assert_eq!(config.switching.push_distance, 55.0);
}

#[tokio::test]
async fn config_subscription_refreshes_the_ui_without_overwriting_drafts() {
    let (_dir, mut app) = app().await;
    let mut stream = std::pin::pin!(config_stream(&EngineRef(app.engine.clone())));
    let next = timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap();
    let _ = app.update(next);
    let _ = app.update(Message::PushDistance(65.0));
    let mut options =
        model::DisplayOptions::from_config(app.engine.net().id(), &app.model.config.extend);
    options.fps = 120;
    let _ = app.update(Message::DisplayOptionsChanged(options.clone()));
    app.engine
        .update_config(|c| {
            c.control.controller = Some("peer".into());
            c.control.updated_at = 200;
            c.extend.stats = true;
        })
        .unwrap();
    let queued = timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap();
    // A config notification queued before our edit must not roll that edit back.
    let _ = app.update(Message::InvertWheel(true));
    let _ = app.update(queued);
    assert_eq!(app.model.config, app.engine.config());
    assert_eq!(app.model.config.control.controller.as_deref(), Some("peer"));
    assert_eq!(app.model.push_distance, Some(65.0));
    assert_eq!(app.model.display_options, Some(options));
    let _ = app.update(Message::SaveSettings);
    assert_eq!(app.engine.config().switching.push_distance, 65.0);
    assert_eq!(app.model.push_distance, None);
    // Committing the display dialog edits only its fields, even if another setting
    // changed after the dialog was opened. Sharing is off, so no viewer opens here.
    app.engine
        .update_config(|c| {
            c.extend.stats = false;
            c.control.updated_at = 300;
        })
        .unwrap();
    let _ = app.update(Message::DisplayOptionsDone(true));
    assert_eq!(app.engine.config().extend.fps, 120);
    assert!(!app.engine.config().extend.stats);
    assert_eq!(app.engine.config().control.updated_at, 300);
}

#[tokio::test]
async fn failed_ui_saves_restore_committed_values_and_show_the_error() {
    let (dir, mut app) = app().await;
    let before = app.engine.config();
    std::fs::create_dir(legato_engine::config::path(dir.path())).unwrap();
    let _ = app.update(Message::InvertWheel(!before.scrolling.invert_wheel));
    assert_eq!(app.engine.config(), before);
    assert_eq!(app.model.config, before);
    assert!(
        app.model
            .notice
            .as_ref()
            .unwrap()
            .1
            .contains("Couldn't save settings")
    );
    let _ = app.update(Message::DisplayOptions(app.engine.net().id()));
    let options = app.model.display_options.clone();
    let _ = app.update(Message::DisplayOptionsDone(true));
    assert!(
        app.model.display_options_open,
        "failed saves keep the dialog editable"
    );
    assert_eq!(app.model.display_options, options);
    assert_eq!(app.engine.config(), before);
}

#[tokio::test]
async fn choosing_a_controller_always_advances_the_latest_policy() {
    let (_dir, mut app) = app().await;
    let future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 100;
    app.engine
        .update_config(|c| c.control.updated_at = future)
        .unwrap();
    let _ = app.update(Message::ControlMode(Some("local".into())));
    assert_eq!(app.engine.config().control.updated_at, future + 1);
    let _ = app.update(Message::ControlMode(None));
    assert_eq!(app.engine.config().control.updated_at, future + 2);
    assert_eq!(app.engine.config().control.controller, None);
}

#[tokio::test]
async fn failed_unpair_keeps_the_device_and_its_arrangement_for_retry() {
    let (dir, mut app) = app().await;
    let id = iroh::SecretKey::from_bytes(&[7; 32]).public();
    app.engine
        .net()
        .store()
        .add_peer(id, "Peer".into(), legato_proto::Os::MacOs)
        .unwrap();
    app.engine
        .update_config(|c| {
            c.neighbors.push(legato_engine::config::Neighbor {
                peer: id.to_string(),
                side: legato_core::Side::Below,
                display: 1,
                align: Default::default(),
                nudge: 0.0,
                offset: None,
            })
        })
        .unwrap();
    app.refresh_paired();
    let before = app.engine.config();
    let blocker = dir.path().join("peers.tmp");
    std::fs::create_dir(&blocker).unwrap();
    let _ = app.update(Message::Unpair(id));
    let _ = app.update(Message::ConfirmUnpair(true));
    assert!(app.model.paired(&id).is_some());
    assert_eq!(app.engine.config(), before);
    assert!(
        app.model
            .notice
            .as_ref()
            .unwrap()
            .1
            .contains("Couldn't unpair")
    );
    std::fs::remove_dir(blocker).unwrap();
    let _ = app.update(Message::Unpair(id));
    let _ = app.update(Message::ConfirmUnpair(true));
    assert!(app.model.paired(&id).is_none());
    assert!(app.engine.config().neighbors.is_empty());
}

#[tokio::test]
async fn unpair_requires_confirmation_and_removes_only_the_selected_peer() {
    let (dir, mut app) = app().await;
    let peers = [7, 8].map(|n| iroh::SecretKey::from_bytes(&[n; 32]).public());
    for id in peers {
        app.engine
            .net()
            .store()
            .add_peer(id, format!("Peer {id}"), legato_proto::Os::MacOs)
            .unwrap();
        app.engine
            .update_config(|c| {
                c.neighbors.push(legato_engine::config::Neighbor {
                    peer: id.to_string(),
                    side: legato_core::Side::Below,
                    display: 1,
                    align: Default::default(),
                    nudge: 0.0,
                    offset: None,
                })
            })
            .unwrap();
    }
    app.refresh_paired();
    let before = app.engine.config();
    let _ = app.update(Message::Unpair(peers[0]));
    assert!(app.model.unpair_open);
    assert_eq!(app.model.paired.len(), 2);
    let _ = app.update(Message::ConfirmUnpair(false));
    assert!(!app.model.unpair_open);
    // A stale confirm after cancellation must not remove anything.
    let _ = app.update(Message::ConfirmUnpair(true));
    assert_eq!(app.model.paired.len(), 2);
    assert_eq!(app.engine.config(), before);
    let _ = app.update(Message::Unpair(peers[1]));
    let _ = app.update(Message::ConfirmUnpair(true));
    assert!(!app.model.unpair_open);
    assert!(app.model.paired(&peers[0]).is_some());
    assert!(app.model.paired(&peers[1]).is_none());
    let config = legato_engine::config::load(dir.path()).unwrap();
    assert_eq!(config.neighbors.len(), 1);
    assert_eq!(config.neighbors[0].peer, peers[0].to_string());
    let _ = app.update(Message::Unpair(peers[1]));
    assert!(!app.model.unpair_open);
}

#[tokio::test]
async fn watch_subscription_delivers_initial_and_latest_values_then_closes() {
    let (tx, rx) = tokio::sync::watch::channel(false);
    let mut stream = std::pin::pin!(watch_stream(rx, |value| Message::ViewerShowing(*value)));
    assert_matches!(stream.next().await, Some(Message::ViewerShowing(false)));
    // These updates must neither deadlock on a retained read guard nor replay every
    // intermediate value when the UI next polls the subscription.
    tx.send_replace(true);
    tx.send_replace(false);
    tx.send_replace(true);
    assert_matches!(stream.next().await, Some(Message::ViewerShowing(true)));
    drop(tx);
    assert!(stream.next().await.is_none());
}
