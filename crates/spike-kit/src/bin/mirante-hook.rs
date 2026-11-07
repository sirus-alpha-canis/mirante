//! Called by Claude Code for every hook event. Appends one JSON line to
//! $MIRANTE_HOOK_LOG (default ~/.mirante-spike/hooks.jsonl). Never fails, never blocks.
use std::io::Read;
use std::path::PathBuf;

use spike_kit::{hook, util};

fn main() {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let log = std::env::var("MIRANTE_HOOK_LOG").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".mirante-spike/hooks.jsonl")
    });
    let task = std::env::var("MIRANTE_TASK_ID").ok();
    let _ = util::append_jsonl(&log, &hook::logged_from_stdin(&raw, task, util::now_ms()));
}
