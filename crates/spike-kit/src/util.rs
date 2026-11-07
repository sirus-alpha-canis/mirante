use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn append_jsonl(path: &Path, value: &Value) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    // One write_all of the whole line: concurrent O_APPEND writers cannot interleave mid-line.
    let line = serde_json::to_string(value)? + "\n";
    f.write_all(line.as_bytes())?;
    Ok(())
}

pub fn read_jsonl(path: &Path) -> anyhow::Result<Vec<Value>> {
    let (v, skipped) = parse_jsonl_counted(&fs::read_to_string(path)?);
    if skipped > 0 {
        eprintln!("warning: skipped {skipped} unparseable lines in {}", path.display());
    }
    Ok(v)
}

pub fn parse_jsonl(text: &str) -> Vec<Value> {
    parse_jsonl_counted(text).0
}

/// Parsed values plus the number of non-blank lines that failed to parse.
pub fn parse_jsonl_counted(text: &str) -> (Vec<Value>, usize) {
    let mut skipped = 0;
    let mut out = Vec::new();
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str(l) {
            Ok(v) => out.push(v),
            Err(_) => skipped += 1,
        }
    }
    (out, skipped)
}

/// Inherited variables that make a child `claude` believe it is nested inside another
/// Claude Code session (or a terminal wrapper's integration). Auth variables are kept.
pub const NESTED_SESSION_ENV: [&str; 13] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    "CMUX_CLAUDE_PID",
];

/// Names of the variables to remove before spawning a spike `claude`: the nested-session
/// list plus any terminal-wrapper (cmux) integration variables present in this process.
pub fn env_to_scrub() -> Vec<String> {
    let mut keys: Vec<String> = NESTED_SESSION_ENV.iter().map(|k| k.to_string()).collect();
    keys.extend(std::env::vars().map(|(k, _)| k).filter(|k| k.starts_with("CMUX_CLAUDE_") || k.starts_with("CMUX_AGENT_")));
    keys
}

/// The `claude` binary for spikes: `MIRANTE_CLAUDE_BIN` or `claude` on PATH.
pub fn claude_bin() -> String {
    std::env::var("MIRANTE_CLAUDE_BIN").unwrap_or_else(|_| "claude".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jsonl_skips_blank_and_invalid_lines() {
        let v = parse_jsonl("{\"a\":1}\n\nnot json\n{\"a\":2}\n");
        assert_eq!(v.len(), 2);
        assert_eq!(v[1]["a"], 2);
    }

    #[test]
    fn parse_jsonl_counted_reports_skipped_lines() {
        let (v, skipped) = parse_jsonl_counted("{\"a\":1}\n\nnot json\n{broken\n{\"a\":2}\n");
        assert_eq!(v.len(), 2);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn append_then_read_roundtrips() {
        let path = std::env::temp_dir().join(format!("mirante-util-{}.jsonl", now_ms()));
        append_jsonl(&path, &serde_json::json!({"x": 1})).unwrap();
        append_jsonl(&path, &serde_json::json!({"x": 2})).unwrap();
        let v = read_jsonl(&path).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["x"], 1);
        let _ = std::fs::remove_file(path);
    }
}
