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
    writeln!(f, "{value}")?;
    Ok(())
}

pub fn read_jsonl(path: &Path) -> anyhow::Result<Vec<Value>> {
    Ok(parse_jsonl(&fs::read_to_string(path)?))
}

pub fn parse_jsonl(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
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
