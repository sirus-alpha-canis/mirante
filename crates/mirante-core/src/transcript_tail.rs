//! Incremental reader for an append-only JSONL transcript. Each `poll` returns the complete
//! lines appended since the last one; a partial last line stays buffered until its newline
//! arrives. `offset()` is the byte position after the last complete line: persist it and
//! pass it back to `TranscriptTail::new` to resume after a restart.
//!
//! Memory is bounded: a poll reads at most `DEFAULT_CHUNK` (1 MiB) bytes, and a single line
//! longer than `DEFAULT_MAX_PARTIAL` (64 MiB) is discarded (counted in `skipped`) instead of
//! buffered. Call `poll` again while the batch's `more` is set to catch up. Do not loop on
//! `offset() < file length`: a partial last line keeps the offset behind the length until
//! its newline arrives, so that loop would spin.
//!
//! Restart in the middle of a discarded oversized line: the persisted offset then points
//! inside that line (discarded bytes advance it). A new tail from that offset does not know
//! it was discarding, so it reads the line's remainder as one line, which is not valid JSON
//! and is counted once in `skipped` (or discarded again if the remainder is itself over the
//! cap). No valid line is lost or duplicated.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::Result;

/// Most bytes read from the file per `poll`.
pub const DEFAULT_CHUNK: usize = 1 << 20;
/// Longest unterminated line kept in memory before it is discarded.
pub const DEFAULT_MAX_PARTIAL: usize = 64 << 20;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TailBatch {
    pub lines: Vec<Value>,
    /// Complete, non-blank lines that were not valid JSON (counted, never silently dropped).
    pub skipped: usize,
    /// True when the file shrank below the saved offset and was re-read from the start.
    pub reset: bool,
    /// True when this poll read a full chunk: more bytes may be waiting, poll again.
    pub more: bool,
}

#[derive(Debug, Clone)]
pub struct TranscriptTail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    chunk: usize,
    max_partial: usize,
    /// Inside an oversized line: bytes are dropped until its newline arrives.
    discarding: bool,
}

impl TranscriptTail {
    pub fn new(path: &Path, offset: u64) -> Self {
        TranscriptTail::with_limits(path, offset, DEFAULT_CHUNK, DEFAULT_MAX_PARTIAL)
    }

    /// Like `new` with explicit per-poll read size and unterminated-line cap (both in bytes).
    pub fn with_limits(path: &Path, offset: u64, chunk: usize, max_partial: usize) -> Self {
        TranscriptTail {
            path: path.to_path_buf(),
            offset,
            partial: Vec::new(),
            chunk: chunk.max(1),
            max_partial,
            discarding: false,
        }
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
            self.discarding = false;
            batch.reset = true;
        }
        file.seek(SeekFrom::Start(self.offset + self.partial.len() as u64))?;
        let mut fresh = Vec::new();
        file.take(self.chunk as u64).read_to_end(&mut fresh)?;
        batch.more = fresh.len() >= self.chunk;

        if self.discarding {
            match fresh.iter().position(|b| *b == b'\n') {
                None => {
                    self.offset += fresh.len() as u64;
                    return Ok(batch);
                }
                Some(i) => {
                    self.offset += i as u64 + 1;
                    fresh.drain(..=i);
                    self.discarding = false;
                }
            }
        }
        self.partial.extend_from_slice(&fresh);

        if let Some(last_newline) = self.partial.iter().rposition(|b| *b == b'\n') {
            let complete: Vec<u8> = self.partial.drain(..=last_newline).collect();
            self.offset += complete.len() as u64;
            for line in complete.split(|b| *b == b'\n') {
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match serde_json::from_slice::<Value>(line) {
                    Ok(v) => batch.lines.push(v),
                    Err(_) => batch.skipped += 1,
                }
            }
        }
        if self.partial.len() > self.max_partial {
            self.offset += self.partial.len() as u64;
            self.partial.clear();
            self.discarding = true;
            batch.skipped += 1;
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

    /// Polls while `more`, as the module docs prescribe.
    fn drain(t: &mut TranscriptTail, path: &Path) -> (Vec<Value>, usize) {
        let len = std::fs::metadata(path).unwrap().len();
        let (mut lines, mut skipped) = (Vec::new(), 0);
        for _ in 0..10_000 {
            let b = t.poll().unwrap();
            lines.extend(b.lines);
            skipped += b.skipped;
            if !b.more {
                break;
            }
        }
        assert_eq!(t.offset(), len, "caught up once `more` clears");
        (lines, skipped)
    }

    #[test]
    fn reads_in_bounded_chunks_without_losing_lines() {
        let dir = TempDir::new("tail-chunks");
        let path = dir.path().join("t.jsonl");
        let text: String = (0..50).map(|n| format!("{{\"n\":{n},\"s\":\"ã\"}}\n")).collect();
        append(&path, &text);
        let mut t = TranscriptTail::with_limits(&path, 0, 16, 1 << 20);
        let first = t.poll().unwrap();
        assert!(t.offset() <= 16, "one poll reads at most one chunk");
        assert!(first.lines.len() <= 1);
        let (rest, skipped) = drain(&mut t, &path);
        assert_eq!(first.lines.len() + rest.len(), 50);
        assert_eq!(skipped, 0);
        assert_eq!(rest.last().unwrap()["n"], 49);
    }

    #[test]
    fn oversized_line_is_discarded_and_counted_then_tailing_resumes() {
        let dir = TempDir::new("tail-cap");
        let path = dir.path().join("t.jsonl");
        let big = format!("{{\"x\":\"{}\"}}\n", "a".repeat(200));
        append(&path, &format!("{{\"n\":1}}\n{big}{{\"n\":2}}\n"));
        let mut t = TranscriptTail::with_limits(&path, 0, 16, 32);
        let (lines, skipped) = drain(&mut t, &path);
        assert_eq!(lines.iter().map(|v| v["n"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(skipped, 1);
        assert_eq!(t.offset(), std::fs::metadata(&path).unwrap().len());
        assert!(t.partial.len() <= 32 + 16);
        append(&path, "{\"n\":3}\n");
        assert_eq!(t.poll().unwrap().lines[0]["n"], 3);
    }

    #[test]
    fn invalid_utf8_line_is_skipped_not_lossily_decoded() {
        let dir = TempDir::new("tail-utf8");
        let path = dir.path().join("t.jsonl");
        let mut bytes = b"{\"s\":\"".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(b"\"}\n{\"n\":1}\n");
        std::fs::write(&path, bytes).unwrap();
        let b = TranscriptTail::new(&path, 0).poll().unwrap();
        assert_eq!((b.lines.len(), b.skipped), (1, 1));
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

#[cfg(test)]
mod more_tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::*;
    use crate::test_support::TempDir;

    fn append(path: &Path, text: &str) {
        let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    /// Loop while `more`: it is set when a poll hit the chunk limit, and clear once the
    /// reader is caught up, even with a partial last line (offset stays behind len then).
    #[test]
    fn more_flags_a_full_chunk_and_clears_when_caught_up() {
        let dir = TempDir::new("tail-more");
        let path = dir.path().join("t.jsonl");
        let text: String = (0..20).map(|n| format!("{{\"n\":{n}}}\n")).collect();
        append(&path, &text);
        append(&path, "{\"n\":");
        let mut t = TranscriptTail::with_limits(&path, 0, 16, 1 << 20);
        let mut lines = Vec::new();
        let mut polls = 0;
        loop {
            let b = t.poll().unwrap();
            polls += 1;
            lines.extend(b.lines);
            if !b.more {
                break;
            }
            assert!(polls < 1000);
        }
        assert!(polls > 1);
        assert_eq!(lines.len(), 20);
        assert!(t.offset() < std::fs::metadata(&path).unwrap().len(), "partial line stays unread");
        assert!(!t.poll().unwrap().more);
        let small = TranscriptTail::new(&path, 0).poll().unwrap();
        assert!(!small.more);
    }

    /// Restart mid-discard: the saved offset points inside the oversized line. The new tail
    /// reads the line's remainder as one invalid line (counted in `skipped`), then resumes.
    #[test]
    fn restart_inside_a_discarded_line_skips_its_remainder_once() {
        let dir = TempDir::new("tail-restart-discard");
        let path = dir.path().join("t.jsonl");
        append(&path, &format!("{{\"n\":1}}\n{{\"x\":\"{}", "a".repeat(100)));
        let mut t = TranscriptTail::with_limits(&path, 0, 1 << 20, 32);
        let b = t.poll().unwrap();
        assert_eq!((b.lines.len(), b.skipped), (1, 1));
        let saved = t.offset();
        append(&path, "aaaa\"}\n{\"n\":2}\n");
        let mut t2 = TranscriptTail::with_limits(&path, saved, 1 << 20, 32);
        let b = t2.poll().unwrap();
        assert_eq!(b.lines.iter().map(|v| v["n"].as_i64().unwrap()).collect::<Vec<_>>(), vec![2]);
        assert_eq!(b.skipped, 1);
    }
}
