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
