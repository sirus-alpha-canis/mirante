//! `mirante-hook`: see the library docs. Always exits 0 and prints nothing.
use std::io::Read;

use mirante_core::claude_launch::TASK_ENV;
use mirante_core::model::now_ms;
use mirante_ipc::Paths;

fn main() {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let task = std::env::var(TASK_ENV).ok();
    if let Ok(paths) = Paths::from_env() {
        let _ = mirante_hook::deliver(&paths, task.as_deref(), &raw, now_ms());
    }
}
