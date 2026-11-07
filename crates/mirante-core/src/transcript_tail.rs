//! Incremental reader for an append-only JSONL transcript. Each `poll` returns the complete
//! lines appended since the last one; a partial last line stays buffered until its newline
//! arrives. `offset()` is the byte position after the last complete line: persist it and
//! pass it back to `TranscriptTail::new` to resume after a restart.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::Result;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TailBatch {
    pub lines: Vec<Value>,
    /// Complete, non-blank lines that were not valid JSON (counted, never silently dropped).
    pub skipped: usize,
    /// True when the file shrank below the saved offset and was re-read from the start.
    pub reset: bool,
}

#[derive(Debug, Clone)]
pub struct TranscriptTail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl TranscriptTail {
    pub fn new(path: &Path, offset: u64) -> Self {
        TranscriptTail { path: path.to_path_buf(), offset, partial: Vec::new() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads what was appended since the last poll. A missing file yields an empty batch
    /// (Claude creates the transcript on the first prompt).
    pub fn poll(&mut self) -> Result<TailBatch> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TailBatch::default()),
            Err(e) => return Err(e.into()),
        };
        let mut batch = TailBatch::default();
        let len = file.metadata()?.len();
        let read_from = self.offset + self.partial.len() as u64;
        if len < read_from {
            self.offset = 0;
            self.partial.clear();
            batch.reset = true;
        }
        file.seek(SeekFrom::Start(self.offset + self.partial.len() as u64))?;
        let mut fresh = Vec::new();
        file.read_to_end(&mut fresh)?;
        self.partial.extend_from_slice(&fresh);

        let Some(last_newline) = self.partial.iter().rposition(|b| *b == b'\n') else {
            return Ok(batch);
        };
        let complete: Vec<u8> = self.partial.drain(..=last_newline).collect();
        self.offset += complete.len() as u64;
        for line in complete.split(|b| *b == b'\n') {
            let text = String::from_utf8_lossy(line);
            if text.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => batch.lines.push(v),
                Err(_) => batch.skipped += 1,
            }
        }
        Ok(batch)
    }
}
#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::*;
    use crate::test_support::TempDir;

    fn append(path: &Path, text: &str) {
        let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn missing_file_is_an_empty_batch() {
        let dir = TempDir::new("tail-missing");
        let mut t = TranscriptTail::new(&dir.path().join("none.jsonl"), 0);
        assert_eq!(t.poll().unwrap(), TailBatch::default());
    }

    #[test]
    fn returns_only_new_lines_on_each_poll() {
        let dir = TempDir::new("tail-new");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\n{\"n\":2}\n");
        let mut t = TranscriptTail::new(&path, 0);
        let b = t.poll().unwrap();
        assert_eq!(b.lines.iter().map(|v| v["n"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(t.poll().unwrap().lines.len(), 0);
        append(&path, "{\"n\":3}\n");
        assert_eq!(t.poll().unwrap().lines[0]["n"], 3);
        assert_eq!(t.offset(), std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn partial_last_line_waits_for_its_newline() {
        let dir = TempDir::new("tail-partial");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\n{\"n\":");
        let mut t = TranscriptTail::new(&path, 0);
        let b = t.poll().unwrap();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.skipped, 0);
        assert_eq!(t.offset(), 8);
        append(&path, "2,\"s\":\"ã\"}");
        assert!(t.poll().unwrap().lines.is_empty());
        append(&path, "\n");
        let b = t.poll().unwrap();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.lines[0]["s"], "ã");
    }

    #[test]
    fn resumes_from_a_persisted_offset() {
        let dir = TempDir::new("tail-resume");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\n{\"n\":2}\n{\"n\":");
        let mut first = TranscriptTail::new(&path, 0);
        first.poll().unwrap();
        let saved = first.offset();
        append(&path, "3}\n");
        let mut second = TranscriptTail::new(&path, saved);
        let b = second.poll().unwrap();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.lines[0]["n"], 3);
    }

    #[test]
    fn invalid_lines_are_counted_not_hidden() {
        let dir = TempDir::new("tail-invalid");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\nnot json\n\n{\"n\":2}\n");
        let b = TranscriptTail::new(&path, 0).poll().unwrap();
        assert_eq!(b.lines.len(), 2);
        assert_eq!(b.skipped, 1);
    }

    #[test]
    fn truncated_file_is_reread_from_start() {
        let dir = TempDir::new("tail-trunc");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\n{\"n\":2}\n");
        let mut t = TranscriptTail::new(&path, 0);
        t.poll().unwrap();
        std::fs::write(&path, "{\"n\":9}\n").unwrap();
        let b = t.poll().unwrap();
        assert!(b.reset);
        assert_eq!(b.lines[0]["n"], 9);
    }
}
