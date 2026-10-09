//! After a crash, Legato starts itself again (once a minute at most), and the crashed
//! run's log is kept. Runs the real app binary, which crashes on purpose when
//! `LEGATO_TEST_CRASH` is set (debug builds only).

// Tests start the app binary itself; the lint is for the app's redraw path.
#![allow(clippy::disallowed_methods)]

use std::path::Path;
use std::time::{Duration, Instant};

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn a_crash_starts_legato_again_once_and_keeps_the_log() {
    let home = std::env::temp_dir().join(format!("legato-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_legato-app"))
        .env("LEGATO_HOME", &home)
        .env("LEGATO_TEST_CRASH", "1")
        .env_remove("LEGATO_RESTARTED_AT")
        .env_remove("LEGATO_RESTART_AFTER")
        .status()
        .unwrap();
    assert!(!status.success(), "it crashed");

    // The copy it started waits for it to exit, then crashes the same way; within a
    // minute of the first, it doesn't start another.
    let (log, previous) = (
        home.join("legato-app.log"),
        home.join("legato-app.previous.log"),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while !read(&log).contains("Not starting Legato again") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    let (log, previous) = (read(&log), read(&previous));
    eprintln!("previous log:\n{previous}\nlog:\n{log}");
    assert!(
        previous.contains("crashing for the restart test"),
        "the first crash's log is kept"
    );
    assert!(previous.contains("Starting Legato again after the crash."));
    assert!(
        log.contains("crashing for the restart test"),
        "the second run crashed too"
    );
    assert!(
        log.contains("Not starting Legato again"),
        "and didn't start a third"
    );
    let _ = std::fs::remove_dir_all(&home);
}
