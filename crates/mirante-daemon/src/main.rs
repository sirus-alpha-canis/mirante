//! `mirante-daemon`: one per `MIRANTE_HOME` (flock), owns the store, serves the socket.
use std::fs::File;
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use mirante_core::store::Store;
use mirante_daemon::driver::real::RealFactory;
use mirante_daemon::hub::Hub;
use mirante_daemon::server;
use mirante_ipc::{logfile, signals, Paths};

fn lock(path: &Path) -> io::Result<File> {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(path)?;
    // SAFETY: valid fd owned by `f`, which the caller keeps alive for the process lifetime.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "another mirante-daemon is running for this home"));
    }
    Ok(f)
}

/// Keeps `daemon.log` under [`logfile::LOG_CAP`] while the daemon runs: when stderr is that
/// file (as `mirante` starts it) and it grew past the cap, it becomes `daemon.log.1` and
/// stdout/stderr move to a fresh file. A daemon started any other way is left alone.
fn watch_log(path: PathBuf) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(60));
        let Ok(on_disk) = std::fs::metadata(&path) else {
            continue;
        };
        // SAFETY: fstat fills a plain struct for fd 2, which stays open.
        let stderr_is_log = unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            libc::fstat(2, &mut st) == 0 && st.st_dev as u64 == on_disk.dev() && st.st_ino as u64 == on_disk.ino()
        };
        if !stderr_is_log || !matches!(logfile::rotate_if_over(&path, logfile::LOG_CAP), Ok(true)) {
            continue;
        }
        let fresh = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&path);
        if let Ok(f) = fresh {
            // SAFETY: dup2 onto stdout/stderr; `f` is closed after, the copies stay open.
            unsafe {
                libc::dup2(f.as_raw_fd(), 1);
                libc::dup2(f.as_raw_fd(), 2);
            }
        }
    });
}

/// `MIRANTE_CLAUDE_INLINE_ANSWERS`: only an explicit "1" / "true" / "on" turns Claude inline
/// answers on.
fn inline_answers(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("1" | "true" | "on"))
}

/// Exit code when the driver-event pump stops (EX_SOFTWARE).
const PUMP_DIED: i32 = 70;

/// The pump applies every driver event (status, exits, permissions); a daemon without it
/// would serve stale state forever. Should it ever end (a panic past its per-event guard,
/// or its channel closing) the daemon says so and exits non-zero; `mirante` starts a
/// fresh one, whose recovery ends the orphaned sessions.
fn watch_pump(pump: std::thread::JoinHandle<()>, socket: PathBuf) {
    std::thread::spawn(move || {
        let how = match pump.join() {
            Ok(()) => "its channel closed".to_string(),
            Err(p) => {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown panic".into());
                format!("it panicked: {msg}")
            }
        };
        eprintln!("mirante-daemon: FATAL: the driver-event pump stopped ({how}); exiting");
        let _ = std::fs::remove_file(&socket);
        std::process::exit(PUMP_DIED);
    });
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let paths = Paths::from_env()?;
    paths.ensure_home()?;
    let _lock = lock(&paths.lock())?;
    // Before any thread starts: SIGTERM / SIGINT take the same path as `daemon.shutdown`.
    let signals = signals::subscribe(&[signals::SIGTERM, signals::SIGINT])?;
    watch_log(paths.log());
    let store = Store::open(&paths.database())?;
    // `Box::default` builds the factory whether `RealFactory` is a unit struct or has fields.
    let (hub, rx) = Hub::new(store, paths.clone(), Box::<RealFactory>::default())?;
    // Claude inline answers are off unless asked for: screen text can never prove which
    // request a dialog is, so by default Claude cards are answered in the terminal.
    hub.set_claude_inline_answers(inline_answers(std::env::var("MIRANTE_CLAUDE_INLINE_ANSWERS").ok().as_deref()));
    eprintln!("mirante-daemon: claude inline answers {}", if hub.claude_inline_answers() { "on" } else { "off" });
    // Auto-resume: the person's setting, unless `MIRANTE_AUTO_RESUME=off` says no for this
    // daemon (tests that restart a daemon and check what stays ended).
    hub.load_settings();
    if mirante_daemon::hub::resume::env_says_off(std::env::var("MIRANTE_AUTO_RESUME").ok().as_deref()) {
        hub.force_auto_resume_off();
    }
    eprintln!("mirante-daemon: auto-resume {}", if hub.auto_resume_enabled() { "on" } else { "off" });
    let hub = Arc::new(hub);
    let report = hub.recover()?;
    eprintln!("mirante-daemon: recovered {report:?}");
    let pump = hub.spawn_pump(rx);
    hub.spawn_miner();
    hub.spawn_maintenance();
    // The memory guard: measure and act (nothing is limited up front; see hub::memory).
    // `MIRANTE_MEMGUARD=off` disables it (tests that must not depend on this machine's memory).
    let memguard = std::env::var("MIRANTE_MEMGUARD").as_deref() != Ok("off");
    if memguard {
        let sampler = mirante_daemon::memguard::os_sampler();
        let budget = std::env::var("MIRANTE_MEM_BUDGET_MB").ok();
        let idle = std::env::var("MIRANTE_IDLE_HIBERNATE_MIN").ok();
        let cfg = mirante_daemon::hub::memory::config_from(sampler.system().total, budget.as_deref(), idle.as_deref());
        eprintln!("mirante-daemon: memory budget per session {}", mirante_daemon::hub::memory::human(cfg.budget));
        hub.spawn_memguard(sampler, cfg);
    }
    watch_pump(pump, paths.socket());
    let listener = server::bind(&paths.socket())?;
    // Sessions the daemon ended come back, one at a time, while memory allows: after the
    // socket is bound, so their hooks reach this daemon instead of the spool.
    hub.spawn_auto_resume(report.resume.clone(), memguard);
    eprintln!("mirante-daemon: pid {} listening on {}", std::process::id(), paths.socket().display());
    let socket = paths.socket();
    let on_shutdown: server::OnShutdown = Arc::new(move || {
        let _ = std::fs::remove_file(&socket);
        std::process::exit(0);
    });
    {
        let (hub, on_shutdown) = (hub.clone(), on_shutdown.clone());
        std::thread::spawn(move || {
            if let Ok(sig) = signals.recv() {
                eprintln!("mirante-daemon: signal {sig}: stopping sessions and exiting");
                hub.shutdown();
                on_shutdown();
            }
        });
    }
    server::serve(hub, listener, on_shutdown)?;
    Ok(())
}

fn main() -> ExitCode {
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("mirante-daemon {} (build {})", env!("CARGO_PKG_VERSION"), env!("MIRANTE_BUILD"));
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mirante-daemon: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn inline_answers_need_an_explicit_yes() {
        for on in ["1", "true", "on", " 1 "] {
            assert!(super::inline_answers(Some(on)), "{on}");
        }
        for off in [None, Some(""), Some("0"), Some("yes please"), Some("off")] {
            assert!(!super::inline_answers(off), "{off:?}");
        }
    }
}
