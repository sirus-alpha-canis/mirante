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
    let hub = Arc::new(hub);
    let report = hub.recover()?;
    eprintln!("mirante-daemon: recovered {report:?}");
    hub.spawn_pump(rx);
    let listener = server::bind(&paths.socket())?;
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
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mirante-daemon: {e}");
            ExitCode::FAILURE
        }
    }
}
