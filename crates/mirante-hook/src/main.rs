//! `mirante-hook`: see the library docs. Always exits 0 and prints nothing on stdout.
use mirante_core::claude_launch::{SESSION_ENV, TASK_ENV};
use mirante_core::model::now_ms;
use mirante_ipc::Paths;

fn run() {
    let received = now_ms();
    let (raw, truncated) = mirante_hook::read_stdin(mirante_hook::STDIN_TIMEOUT, mirante_hook::STDIN_CAP);
    let task = std::env::var(TASK_ENV).ok();
    let session = std::env::var(SESSION_ENV).ok();
    if let Ok(paths) = Paths::from_env() {
        let _ = mirante_hook::deliver_session(&paths, task.as_deref(), session.as_deref(), &raw, received, truncated);
    }
}

fn main() {
    // A panic must never change Claude's behavior: swallow it and exit 0.
    let _ = std::panic::catch_unwind(run);
    std::process::exit(0);
}
