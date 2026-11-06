//! Watchdog. `qivo-player.exe` started without arguments is the supervisor: it runs
//! the real player as a child process (`--player`) and restarts it when it
//!   * crashes or exits (except when a technician chose "Exit player"),
//!   * hangs: the player page stops sending heartbeats for 2 minutes,
//!   * reaches the daily restart time (default 04:00), which clears slow leaks.
//!
//! Restarts back off when the player keeps failing, so a broken setup doesn't spin the CPU.

use std::{
    net::TcpListener,
    process::{Child, Command},
    thread::sleep,
    time::{Duration, Instant},
};

use crate::config::{self, Config};

/// Exit code meaning "a technician closed the player on purpose: don't restart".
pub const EXIT_QUIT: i32 = 43;
/// Exit code meaning "restart me" (remote "Restart player" command).
pub const EXIT_RESTART: i32 = 42;
/// Held for the supervisor's lifetime so a second copy exits immediately.
const LOCK_PORT: u16 = 47_811;
/// No heartbeat for this long = frozen. `QIVO_WATCHDOG_HANG_SECS` overrides (testing).
fn hang_after() -> Duration {
    Duration::from_secs(std::env::var("QIVO_WATCHDOG_HANG_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(120))
}
/// Time a fresh start gets before heartbeats are required (slow boots, first download).
fn startup_grace() -> Duration {
    Duration::from_secs(std::env::var("QIVO_WATCHDOG_GRACE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(180))
}

pub fn run(extra_args: &[String]) -> i32 {
    let Ok(_lock) = TcpListener::bind(("127.0.0.1", LOCK_PORT)) else {
        config::log("supervisor: another copy is already running, exiting");
        return 0;
    };
    #[cfg(any(windows, target_os = "linux"))]
    crate::native::set_autostart(Config::load().autostart);
    config::log(&format!("supervisor: started (version {})", env!("CARGO_PKG_VERSION")));

    let mut backoff = Duration::from_secs(1);
    let mut last_daily: Option<chrono::NaiveDate> = None;
    loop {
        config::clear_heartbeat();
        let started = Instant::now();
        let mut child = match spawn(extra_args) {
            Ok(c) => c,
            Err(e) => {
                config::log(&format!("supervisor: could not start the player: {e}"));
                sleep(Duration::from_secs(10));
                continue;
            }
        };
        let reason = watch(&mut child, started, &mut last_daily);
        match reason {
            Ended::Quit => {
                config::log("supervisor: player closed by a technician, stopping");
                return 0;
            }
            Ended::Exited(code) => config::log(&format!("supervisor: player exited (code {code:?}), restarting")),
            Ended::Hung => config::log("supervisor: player stopped responding, restarting"),
            Ended::Daily => config::log("supervisor: daily restart"),
        }
        // Quick repeated failures back off up to a minute; a healthy run resets it.
        if started.elapsed() < Duration::from_secs(30) && !matches!(reason, Ended::Exited(Some(EXIT_RESTART)) | Ended::Daily) {
            backoff = (backoff * 2).min(Duration::from_secs(60));
        } else {
            backoff = Duration::from_secs(1);
        }
        sleep(backoff);
    }
}

enum Ended {
    Quit,
    Exited(Option<i32>),
    Hung,
    Daily,
}

fn spawn(extra_args: &[String]) -> std::io::Result<Child> {
    let exe = std::env::current_exe()?;
    Command::new(exe).arg("--player").args(extra_args).spawn()
}

fn watch(child: &mut Child, started: Instant, last_daily: &mut Option<chrono::NaiveDate>) -> Ended {
    loop {
        sleep(Duration::from_secs(2));
        match child.try_wait() {
            Ok(Some(status)) if status.code() == Some(EXIT_QUIT) => return Ended::Quit,
            Ok(Some(status)) => return Ended::Exited(status.code()),
            Ok(None) => {}
            Err(_) => return Ended::Exited(None),
        }
        if started.elapsed() > startup_grace() {
            let stale = config::heartbeat_age().map(|s| s > hang_after().as_secs()).unwrap_or(true);
            if stale {
                let _ = child.kill();
                let _ = child.wait();
                return Ended::Hung;
            }
        }
        if due_daily(started, last_daily) {
            let _ = child.kill();
            let _ = child.wait();
            return Ended::Daily;
        }
    }
}

/// True once per day at the configured local time (and never within the first hour of a run).
fn due_daily(started: Instant, last: &mut Option<chrono::NaiveDate>) -> bool {
    let cfg = Config::load();
    let Ok(at) = chrono::NaiveTime::parse_from_str(cfg.restart_at.trim(), "%H:%M") else { return false };
    let now = chrono::Local::now();
    let today = now.date_naive();
    let window_end = at + chrono::Duration::minutes(10);
    let in_window = now.time() >= at && now.time() < window_end;
    if in_window && *last != Some(today) && started.elapsed() > Duration::from_secs(3600) {
        *last = Some(today);
        return true;
    }
    false
}
