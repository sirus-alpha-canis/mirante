//! Terminal queries in the agent's output, answered by the daemon. A TUI asks its terminal
//! for the cursor position (CPR `ESC[6n`), device attributes (DA1 `ESC[c`), status (DSR
//! `ESC[5n`) and colors (OSC 10/11 `?`); Claude Code stalls until CPR is answered (Week-1
//! spike). The daemon always answers (attached or not) and strips the queries from what
//! attached clients get, so a client terminal never answers a second time (a second reply
//! would arrive as typed input).
//!
//! Two more kinds of request are stripped from the stream without an answer, because a
//! client terminal would answer them as typed input: OSC 52 clipboard reads
//! (`ESC]52;<sel>;?`, the reply carries the user's clipboard) and DCS requests (`ESC P $ q`
//! DECRQSS, `ESC P + q` XTGETTCAP). Clipboard writes and other DCS strings pass through.
//!
//! The parser keeps state across reads: a query split between two chunks (`ESC[` then
//! `6n`) is held back and completed with the next chunk. Only bytes that are still a prefix
//! of a known query are held (a few dozen at most); a DCS request body is dropped as it
//! streams, up to its terminator. Everything else passes through byte for byte.

/// A query the agent sent to its terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// `ESC[6n`: where is the cursor?
    CursorPosition,
    /// `ESC[c` / `ESC[0c`: what kind of terminal is this?
    DeviceAttributes,
    /// `ESC[5n`: are you OK?
    Status,
    /// `ESC]10;?`: the foreground color. `bel`: terminated by BEL (else by ST).
    Foreground { bel: bool },
    /// `ESC]11;?`: the background color.
    Background { bel: bool },
}

/// Output split into what passes through and the queries, in stream order (a CPR answer
/// needs the cursor as it is after the bytes before the query).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Bytes(Vec<u8>),
    Query(Query),
}

const QUERIES: [(&[u8], Query); 8] = [
    (b"\x1b[6n", Query::CursorPosition),
    (b"\x1b[c", Query::DeviceAttributes),
    (b"\x1b[0c", Query::DeviceAttributes),
    (b"\x1b[5n", Query::Status),
    (b"\x1b]10;?\x07", Query::Foreground { bel: true }),
    (b"\x1b]10;?\x1b\\", Query::Foreground { bel: false }),
    (b"\x1b]11;?\x07", Query::Background { bel: true }),
    (b"\x1b]11;?\x1b\\", Query::Background { bel: false }),
];

/// What a partial match at the end of the input could still become.
enum Match {
    Full(Query, usize),
    /// A request to drop whole (an OSC 52 clipboard read) of this length.
    Strip(usize),
    /// A DCS request: drop this many bytes, then its body up to the terminator.
    SkipBody(usize),
    Prefix,
    No,
}

const DCS_REQUESTS: [&[u8]; 2] = [b"\x1bP$q", b"\x1bP+q"];
const OSC52: &[u8] = b"\x1b]52;";
/// Longest OSC 52 selection parameter considered (`c`, `p`, `s`, `0`..`7`, combinations).
const OSC52_SELECTION: usize = 16;

/// `ESC]52;<sel>;?` + BEL / ST at the start of `buf`.
fn osc52_read(buf: &[u8]) -> Match {
    if buf.len() < OSC52.len() {
        return if OSC52.starts_with(buf) { Match::Prefix } else { Match::No };
    }
    if !buf.starts_with(OSC52) {
        return Match::No;
    }
    let mut j = OSC52.len();
    while j < buf.len() && buf[j].is_ascii_alphanumeric() && j - OSC52.len() < OSC52_SELECTION {
        j += 1;
    }
    let rest = &buf[j..];
    let want: [&[u8]; 2] = [b";?\x07", b";?\x1b\\"];
    for w in want {
        if rest.starts_with(w) {
            return Match::Strip(j + w.len());
        }
    }
    if want.iter().any(|w| w.starts_with(rest)) {
        Match::Prefix
    } else {
        Match::No
    }
}

fn match_at(buf: &[u8]) -> Match {
    let mut prefix = false;
    for (pat, q) in QUERIES {
        if buf.len() >= pat.len() {
            if &buf[..pat.len()] == pat {
                return Match::Full(q, pat.len());
            }
        } else if pat.starts_with(buf) {
            prefix = true;
        }
    }
    for pat in DCS_REQUESTS {
        if buf.starts_with(pat) {
            return Match::SkipBody(pat.len());
        }
        if buf.len() < pat.len() && pat.starts_with(buf) {
            prefix = true;
        }
    }
    match osc52_read(buf) {
        Match::Strip(n) => return Match::Strip(n),
        Match::Prefix => prefix = true,
        _ => {}
    }
    if prefix {
        Match::Prefix
    } else {
        Match::No
    }
}

/// Where the parser is inside a DCS request body being dropped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Skip {
    #[default]
    No,
    Body,
    /// Saw ESC inside the body: `\` ends it.
    Esc,
}

#[derive(Debug, Default)]
pub struct TermQueries {
    held: Vec<u8>,
    skip: Skip,
}

impl TermQueries {
    /// Splits one chunk of output (plus whatever was held from the previous one).
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Piece> {
        let mut buf = std::mem::take(&mut self.held);
        buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut plain = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            match self.skip {
                Skip::No => {}
                Skip::Body => {
                    self.skip = match buf[i] {
                        0x07 => Skip::No,
                        0x1b => Skip::Esc,
                        _ => Skip::Body,
                    };
                    i += 1;
                    continue;
                }
                Skip::Esc => {
                    // ESC \ is the terminator; ESC anything else aborts the string too.
                    self.skip = Skip::No;
                    i += 1;
                    continue;
                }
            }
            if buf[i] != 0x1b {
                plain.push(buf[i]);
                i += 1;
                continue;
            }
            match match_at(&buf[i..]) {
                Match::Full(q, len) => {
                    if !plain.is_empty() {
                        out.push(Piece::Bytes(std::mem::take(&mut plain)));
                    }
                    out.push(Piece::Query(q));
                    i += len;
                }
                Match::Strip(len) => i += len,
                Match::SkipBody(len) => {
                    self.skip = Skip::Body;
                    i += len;
                }
                Match::Prefix => {
                    // Only reachable at the end of the input: wait for the next chunk.
                    self.held = buf[i..].to_vec();
                    break;
                }
                Match::No => {
                    plain.push(0x1b);
                    i += 1;
                }
            }
        }
        if !plain.is_empty() {
            out.push(Piece::Bytes(plain));
        }
        out
    }

    /// Bytes held back at the end of the stream (an unfinished query prefix), as-is.
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }
}

/// The reply a terminal would send. `cursor` is (row, col), 0-based.
pub fn answer(q: Query, cursor: (u16, u16)) -> Vec<u8> {
    let st = |bel: bool| if bel { "\x07" } else { "\x1b\\" };
    match q {
        Query::CursorPosition => format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1).into_bytes(),
        // A VT100 with advanced video: the plainest answer every TUI accepts.
        Query::DeviceAttributes => b"\x1b[?1;2c".to_vec(),
        Query::Status => b"\x1b[0n".to_vec(),
        Query::Foreground { bel } => format!("\x1b]10;rgb:ffff/ffff/ffff{}", st(bel)).into_bytes(),
        Query::Background { bel } => format!("\x1b]11;rgb:0000/0000/0000{}", st(bel)).into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(p: &[Piece]) -> Vec<u8> {
        p.iter()
            .flat_map(|p| match p {
                Piece::Bytes(b) => b.clone(),
                Piece::Query(_) => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn queries_are_split_out_in_stream_order() {
        let mut t = TermQueries::default();
        let p = t.feed(b"ab\x1b[6ncd\x1b[c\x1b[0c\x1b[5n");
        assert_eq!(
            p,
            vec![
                Piece::Bytes(b"ab".to_vec()),
                Piece::Query(Query::CursorPosition),
                Piece::Bytes(b"cd".to_vec()),
                Piece::Query(Query::DeviceAttributes),
                Piece::Query(Query::DeviceAttributes),
                Piece::Query(Query::Status),
            ]
        );
    }

    #[test]
    fn a_query_split_across_reads_is_completed() {
        let mut t = TermQueries::default();
        assert_eq!(t.feed(b"x\x1b["), vec![Piece::Bytes(b"x".to_vec())]);
        assert_eq!(t.feed(b"6"), vec![]);
        assert_eq!(t.feed(b"ny"), vec![Piece::Query(Query::CursorPosition), Piece::Bytes(b"y".to_vec())]);
        assert_eq!(t.feed(b"\x1b]1"), vec![]);
        assert_eq!(t.feed(b"1;?\x1b"), vec![]);
        assert_eq!(t.feed(b"\\"), vec![Piece::Query(Query::Background { bel: false })]);
    }

    #[test]
    fn osc_color_queries_with_bel_and_st() {
        let mut t = TermQueries::default();
        let p = t.feed(b"\x1b]10;?\x07\x1b]11;?\x1b\\");
        assert_eq!(
            p,
            vec![Piece::Query(Query::Foreground { bel: true }), Piece::Query(Query::Background { bel: false })]
        );
        assert_eq!(answer(Query::Foreground { bel: true }, (0, 0)), b"\x1b]10;rgb:ffff/ffff/ffff\x07");
        assert_eq!(answer(Query::Background { bel: false }, (0, 0)), b"\x1b]11;rgb:0000/0000/0000\x1b\\");
        assert_eq!(answer(Query::CursorPosition, (4, 9)), b"\x1b[5;10R");
    }

    #[test]
    fn other_sequences_pass_through_untouched() {
        let mut t = TermQueries::default();
        let input = b"\x1b[2J\x1b[1;31mred\x1b[0m\x1b]0;title\x07\x1b[6;5n\x1b[?2004h\x1b]8;;http://x\x1b\\";
        let p = t.feed(input);
        assert_eq!(bytes(&p), input.to_vec());
        assert!(p.iter().all(|p| matches!(p, Piece::Bytes(_))));
    }

    #[test]
    fn a_held_prefix_that_is_no_query_passes_through_with_the_next_chunk() {
        let mut t = TermQueries::default();
        assert_eq!(t.feed(b"\x1b]1"), vec![]);
        assert_eq!(bytes(&t.feed(b"2;?\x07")), b"\x1b]12;?\x07".to_vec());
        // At the end of the stream an unfinished prefix is returned as-is.
        t.feed(b"\x1b[");
        assert_eq!(t.flush(), b"\x1b[".to_vec());
        assert_eq!(t.flush(), Vec::<u8>::new());
    }

    #[test]
    fn bytes_without_escape_are_unchanged_whatever_they_are() {
        let mut t = TermQueries::default();
        let data: Vec<u8> = (0..=255u8).filter(|b| *b != 0x1b).cycle().take(5000).collect();
        let mut got = Vec::new();
        for chunk in data.chunks(333) {
            got.extend(bytes(&t.feed(chunk)));
        }
        assert_eq!(got, data);
    }

    #[test]
    fn clipboard_reads_and_dcs_requests_never_reach_a_client() {
        // A client terminal would answer them as typed input (OSC 52 read: the clipboard).
        let mut t = TermQueries::default();
        let p = t.feed(b"a\x1b]52;c;?\x07b\x1b]52;;?\x1b\\c\x1bP$qm\x1b\\d\x1bP+q544e;636f\x07e");
        assert_eq!(bytes(&p), b"abcde".to_vec());
        // Clipboard writes and other DCS strings pass through.
        let write = b"\x1b]52;c;aGVsbG8=\x07\x1bPq#0;2;0;0;0\x1b\\";
        assert_eq!(bytes(&t.feed(write)), write.to_vec());
    }

    #[test]
    fn stripped_sequences_split_across_reads_are_still_stripped() {
        let mut t = TermQueries::default();
        let mut got = Vec::new();
        for chunk in
            [&b"x\x1b]5"[..], b"2;c", b";", b"?", b"\x1b", b"\\y\x1bP", b"$", b"q long", b" body", b"\x1b", b"\\z"]
        {
            got.extend(bytes(&t.feed(chunk)));
        }
        assert_eq!(got, b"xyz".to_vec());
        assert!(t.flush().is_empty());
    }
}
