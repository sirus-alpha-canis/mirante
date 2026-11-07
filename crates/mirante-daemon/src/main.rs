//! `mirante-daemon`: one per `MIRANTE_HOME` (flock), owns the store, serves the socket.
use std::fs::File;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use mirante_core::store::Store;
use mirante_daemon::driver::real::RealFactory;
use mirante_daemon::hub::Hub;
use mirante_daemon::server;
use mirante_ipc::Paths;

fn lock(path: &Path) -> io::Result<File> {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(path)?;
    // SAFETY: valid fd owned by `f`, which the caller keeps alive for the process lifetime.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "another mirante-daemon is running for this home"));
    }
    Ok(f)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let paths = Paths::from_env()?;
    paths.ensure_home()?;
    let _lock = lock(&paths.lock())?;
    let store = Store::open(&paths.database())?;
    let (hub, rx) = Hub::new(store, paths.clone(), Box::new(RealFactory::default()))?;
    let hub = Arc::new(hub);
    let report = hub.recover()?;
    eprintln!("mirante-daemon: recovered {report:?}");
    hub.spawn_pump(rx);
    let listener = server::bind(&paths.socket())?;
    eprintln!("mirante-daemon: pid {} listening on {}", std::process::id(), paths.socket().display());
    let socket = paths.socket();
    server::serve(
        hub,
        listener,
        Arc::new(move || {
            let _ = std::fs::remove_file(&socket);
            std::process::exit(0);
        }),
    )?;
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
