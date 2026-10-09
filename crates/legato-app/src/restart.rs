//! Starting Legato again after a crash, so sharing carries on by itself: Legato runs in
//! the background, where nobody may notice it's gone. A graphics failure is the likely
//! cause (a PC waking from sleep, short on memory, failing to resize a window's
//! surface), and those usually pass.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Set in a restarted Legato: the process it replaces, to wait for.
const AFTER_ENV: &str = "LEGATO_RESTART_AFTER";
/// Set in a restarted Legato (and passed on): when it was restarted, in seconds since the
/// Unix epoch.
const AT_ENV: &str = "LEGATO_RESTARTED_AT";
/// No more than one restart in this long: something that keeps crashing should stop.
const AGAIN_AFTER: Duration = Duration::from_secs(60);
/// The longest to wait for the crashed Legato to finish exiting.
const WAIT: Duration = Duration::from_secs(15);

/// Whether to restart now, given when Legato last restarted itself (seconds).
fn should_restart(now: u64, last: Option<u64>) -> bool {
    last.is_none_or(|last| now.saturating_sub(last) >= AGAIN_AFTER.as_secs())
}

/// After a crash on the main thread (which ends the app): starts Legato again, unless it
/// already restarted itself within the last minute. Called from the panic hook.
pub fn after_crash() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let last = std::env::var(AT_ENV).ok().and_then(|s| s.parse().ok());
    if !should_restart(now, last) {
        tracing::error!(
            "Not starting Legato again: it already restarted after a crash a moment ago."
        );
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // Once, after a crash: not the redraw path the lint guards.
    #[allow(clippy::disallowed_methods)]
    let started = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(AFTER_ENV, std::process::id().to_string())
        .env(AT_ENV, now.to_string())
        .spawn();
    match started {
        Ok(_) => tracing::error!("Starting Legato again after the crash."),
        Err(e) => tracing::error!("Couldn't start Legato again after the crash: {e}"),
    }
}

/// In a Legato started by [`after_crash`]: waits for the crashed one to exit, so its
/// lock and log are free. Call first thing.
pub fn wait_for_crashed_instance() {
    let Some(pid) = std::env::var(AFTER_ENV).ok().and_then(|s| s.parse().ok()) else {
        return;
    };
    // SAFETY: called first thing in `main`, before any other thread exists.
    unsafe { std::env::remove_var(AFTER_ENV) };
    wait_for_exit(pid, WAIT);
}

/// Waits up to `timeout` for process `pid` to exit; whether it has.
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        // SAFETY: a process handle, closed below; a gone process just fails to open.
        unsafe {
            let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
                return true;
            };
            let done = WaitForSingleObject(process, timeout.as_millis() as u32) == WAIT_OBJECT_0;
            let _ = CloseHandle(process);
            done
        }
    }
    #[cfg(not(windows))]
    {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            // SAFETY: signal 0 only checks that the process exists.
            if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_at_most_once_a_minute() {
        assert!(should_restart(1_000, None), "the first crash");
        assert!(!should_restart(1_030, Some(1_000)), "again within a minute");
        assert!(should_restart(1_060, Some(1_000)));
    }

    #[test]
    fn waits_for_a_process_to_exit() {
        // This test binary, listing no tests: exits at once.
        #[allow(clippy::disallowed_methods)]
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--list", "--exact", "no such test"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        // Reaped by `wait` only after, as the crashed Legato is by nobody.
        std::thread::sleep(Duration::from_millis(500));
        let _ = child.wait();
        assert!(wait_for_exit(pid, Duration::from_secs(5)));
        // This process doesn't exit in the meantime.
        let started = std::time::Instant::now();
        assert!(!wait_for_exit(
            std::process::id(),
            Duration::from_millis(200)
        ));
        assert!(started.elapsed() >= Duration::from_millis(150));
    }
}
