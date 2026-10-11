//! End to end on the Mac: the real `legato run` binary, driven by a fake controller over
//! iroh. Moves the cursor (and puts it back), so it's ignored by default:
//! `cargo test -p legato-cli --test receive_e2e -- --ignored`.
#![cfg(target_os = "macos")]

use std::time::Duration;

use legato_proto::{Control, Datagram, Os, Point};

mod support;

#[tokio::test]
#[ignore = "moves the cursor"]
async fn legato_run_injects_what_the_controller_sends() {
    assert!(
        legato_macos::Permissions::check().all_granted(),
        "needs Accessibility access"
    );
    let support::Peer {
        mut child,
        net: pc,
        events: _events,
        session,
    } = support::start("receive").await;
    assert_eq!(session.remote.os, Os::MacOs);
    assert_eq!(session.remote.screens, legato_macos::screens());

    let original = legato_macos::cursor_position();
    let main = legato_macos::screens()
        .displays
        .into_iter()
        .find(|d| d.primary)
        .unwrap()
        .bounds;
    let start = Point::new(main.x + 200.0, main.y + 200.0);
    assert!(session.send(Control::Enter { seq: 1, pos: start }));
    for i in 1..=10u32 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        session.send_datagram(&Datagram::Motion {
            seq: 1 + i,
            pos: Point::new(start.x + 10.0 * i as f64, start.y),
        });
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let landed = legato_macos::cursor_position();

    // Put the cursor back and let go before asserting.
    session.send(Control::Enter {
        seq: 100,
        pos: original,
    });
    session.send(Control::Leave);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = child.start_kill();
    pc.shutdown().await;

    assert!(
        (landed.x - (start.x + 100.0)).abs() < 1.0 && (landed.y - start.y).abs() < 1.0,
        "cursor at {landed:?}, expected {:?}",
        Point::new(start.x + 100.0, start.y)
    );
}
