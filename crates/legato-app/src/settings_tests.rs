use iced::futures::StreamExt;
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
