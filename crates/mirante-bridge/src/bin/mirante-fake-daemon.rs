//! `mirante-fake-daemon <socket>`: the scripted fake daemon with demo tasks and a demo repo
//! memory (`dna.*`), for working on the cockpit UI without agents and for the app smoke
//! test. Prints `call <method>` for every request it receives. Not a daemon: it never starts agents or touches git.
use std::path::PathBuf;
use std::time::Duration;

use mirante_bridge::fake::{task_json, FakeDaemon};
use serde_json::json;

fn main() {
    let Some(socket) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: mirante-fake-daemon <socket path>");
        std::process::exit(2);
    };
    let fake = FakeDaemon::start(&socket);
    let mut waiting = task_json("t-codex-1", "/repos/shop", "fix/cart", "codex", "needs_you");
    waiting["needs_you"] = json!(1);
    let mut done = task_json("t-claude-2", "/repos/shop", "feat/search", "claude", "idle");
    done["live"] = json!(false);
    done["boot"] = json!(null);
    fake.set_tasks(vec![task_json("t-claude-1", "/repos/mirante", "feat/login", "claude", "working"), waiting, done]);
    fake.push_event(Some("t-claude-2"), "turn_started", json!({"idx": 1}));
    fake.push_event(Some("t-claude-2"), "turn_ended", json!({"idx": 1}));
    fake.push_event(
        Some("t-codex-1"),
        "permission_requested",
        json!({"permission_id": "p-1", "tool": "command", "detail": "npm install"}),
    );
    fake.push_event(
        Some("t-claude-1"),
        "dna.proposed",
        json!({"id": "p-1", "title": "Login tokens are refreshed by the session layer"}),
    );
    println!("fake daemon on {}", socket.display());
    let mut seen = 0;
    let mut n = 0u64;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let calls = fake.methods();
        for m in &calls[seen..] {
            println!("call {m}");
        }
        seen = calls.len();
        n += 1;
        if n.is_multiple_of(10) {
            let step = json!({"type": "tool", "tool": "Bash", "summary": format!("cargo test (run {})", n / 10), "ok": true, "agent": null});
            fake.push_event(Some("t-claude-1"), "step", step);
        }
    }
}
