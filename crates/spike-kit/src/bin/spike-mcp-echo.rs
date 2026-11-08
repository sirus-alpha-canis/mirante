//! A stdio MCP server for ACP spikes. Logs `{argv, cwd, env subset, env names}` at start,
//! then every message it receives, to the JSONL file named by its first argument. Serves
//! `echo_read` (`readOnlyHint: true`) and `echo_write` (see `spike_kit::acp::mcp_echo_reply`).
//!
//! Env values are logged only for `MIRANTE_*`, `HOME`, `PWD` and `XDG_*`; every other
//! variable is logged by name only, so a key in the agent's env never lands in the log.
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use spike_kit::{acp, util};

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    let log = PathBuf::from(argv.get(1).context("usage: spike-mcp-echo <log.jsonl>")?);
    let mut values = Map::new();
    let mut names: Vec<String> = Vec::new();
    for (k, v) in std::env::vars() {
        if k.starts_with("MIRANTE_") || k.starts_with("XDG_") || k == "HOME" || k == "PWD" {
            values.insert(k.clone(), Value::String(v));
        }
        names.push(k);
    }
    names.sort();
    util::append_jsonl(
        &log,
        &json!({"ts": util::now_ms(), "kind": "start", "pid": std::process::id(), "argv": argv,
                "cwd": std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default(),
                "env": values, "env_names": names}),
    )?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines().map_while(std::result::Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        util::append_jsonl(&log, &json!({"ts": util::now_ms(), "kind": "recv", "msg": msg}))?;
        if let Some(reply) = acp::mcp_echo_reply(&msg) {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    util::append_jsonl(&log, &json!({"ts": util::now_ms(), "kind": "eof"}))?;
    Ok(())
}
