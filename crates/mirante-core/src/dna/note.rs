//! The note format: Markdown with a strict frontmatter block (a line-based subset of YAML).
//!
//! ```text
//! ---
//! id: <uuid>
//! title: "<at most 120 chars>"
//! kind: fact|decision|pattern|gotcha
//! status: active|retired
//! created: YYYY-MM-DD
//! source: manual|task:<id>|…
//! anchors:
//!   - "<anchor>"
//! ---
//! Markdown body with [[links]].
//! ```
//!
//! `key: value` lines take a plain or double-quoted value (`\"` and `\\` escapes only).
//! Other keys (`tags: [..]`, …) are ignored, and [`update`] keeps them verbatim.
use serde::{Deserialize, Serialize};

use super::anchor::Anchor;
use super::{DnaError, MAX_ANCHORS, MAX_NOTE_BYTES, MAX_TITLE_CHARS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Fact,
    Decision,
    Pattern,
    Gotcha,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Fact => "fact",
            Kind::Decision => "decision",
            Kind::Pattern => "pattern",
            Kind::Gotcha => "gotcha",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "fact" => Kind::Fact,
            "decision" => Kind::Decision,
            "pattern" => Kind::Pattern,
            "gotcha" => Kind::Gotcha,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Retired,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Retired => "retired",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "active" => Status::Active,
            "retired" => Status::Retired,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    pub status: Status,
    pub created: String,
    pub source: String,
    pub anchors: Vec<Anchor>,
    pub body: String,
}

fn note_err(msg: impl Into<String>) -> DnaError {
    DnaError::Note(msg.into())
}

/// `(frontmatter lines, body)`: the lines between the opening and closing `---` (each
/// without its line ending) and everything after the closing line.
fn split(text: &str) -> Result<(Vec<&str>, &str), DnaError> {
    if text.len() > MAX_NOTE_BYTES {
        return Err(note_err(format!("larger than {MAX_NOTE_BYTES} bytes")));
    }
    let mut rest = text;
    let mut first = true;
    let mut lines = Vec::new();
    while !rest.is_empty() {
        let (line, next) = match rest.find('\n') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        rest = next;
        if first {
            if line != "---" {
                return Err(note_err("must start with a --- frontmatter line"));
            }
            first = false;
        } else if line == "---" {
            return Ok((lines, rest));
        } else {
            lines.push(line);
        }
    }
    Err(note_err("frontmatter is not closed with ---"))
}

/// A plain value (trimmed) or a double-quoted one with `\"`, `\\`, `\n`, `\r` and `\t`
/// escapes.
fn scalar(v: &str) -> Result<String, DnaError> {
    let v = v.trim();
    let Some(inner) = v.strip_prefix('"') else {
        return Ok(v.to_string());
    };
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(e @ ('"' | '\\')) => out.push(e),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                _ => return Err(note_err(format!("unsupported escape in {v}"))),
            },
            '"' => {
                return if chars.as_str().trim().is_empty() {
                    Ok(out)
                } else {
                    Err(note_err(format!("text after the closing quote in {v}")))
                }
            }
            c => out.push(c),
        }
    }
    Err(note_err(format!("unterminated quote in {v}")))
}

/// `v` double-quoted, escaping `\\`, `"` and line breaks and tabs (so a value never spans
/// two lines).
fn quote(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `v` plain when it is unambiguous YAML, else quoted.
fn plain_or_quoted(v: &str) -> String {
    let safe = !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'));
    if safe && !v.ends_with(':') {
        v.to_string()
    } else {
        quote(v)
    }
}

/// `key: value` at column 0. `None` for indented, comment or other lines.
fn key_value(line: &str) -> Option<(&str, &str)> {
    if line.starts_with([' ', '\t', '#', '-']) {
        return None;
    }
    let (k, v) = line.split_once(':')?;
    let ok = !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    (ok && (v.is_empty() || v.starts_with(' '))).then_some((k, v))
}

fn anchor_item(line: &str) -> Option<&str> {
    line.strip_prefix("  - ")
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

fn bounded(field: &str, v: String, max: usize) -> Result<String, DnaError> {
    if v.is_empty() || v.chars().count() > max || v.chars().any(char::is_control) {
        return Err(note_err(format!("{field} must be 1..={max} chars without control characters")));
    }
    Ok(v)
}

/// A note from its file text. Strict: every required field once, a known kind and status,
/// a valid date, and 1..=[`MAX_ANCHORS`] valid anchors.
pub fn parse(text: &str) -> Result<Note, DnaError> {
    let (lines, body) = split(text)?;
    let (mut id, mut title, mut kind, mut status, mut created, mut source) = (None, None, None, None, None, None);
    let mut anchors: Option<Vec<Anchor>> = None;
    let mut in_anchors = false;
    for line in lines {
        if in_anchors {
            if let Some(item) = anchor_item(line) {
                let list = anchors.get_or_insert_with(Vec::new);
                list.push(Anchor::parse(&scalar(item)?)?);
                continue;
            }
            in_anchors = false;
        }
        let Some((key, value)) = key_value(line) else {
            continue;
        };
        let slot = match key {
            "id" => &mut id,
            "title" => &mut title,
            "kind" => &mut kind,
            "status" => &mut status,
            "created" => &mut created,
            "source" => &mut source,
            "anchors" => {
                if anchors.is_some() || !value.trim().is_empty() {
                    return Err(note_err("anchors: must appear once, as a list of \"  - \" items"));
                }
                anchors = Some(Vec::new());
                in_anchors = true;
                continue;
            }
            _ => continue,
        };
        if slot.is_some() {
            return Err(note_err(format!("{key} appears twice")));
        }
        *slot = Some(scalar(value)?);
    }
    let need = |v: Option<String>, k: &str| v.ok_or_else(|| note_err(format!("missing {k}")));
    let kind_s = need(kind, "kind")?;
    let status_s = need(status, "status")?;
    let created = need(created, "created")?;
    if !is_date(&created) {
        return Err(note_err("created must be YYYY-MM-DD"));
    }
    let anchors = anchors.unwrap_or_default();
    if anchors.is_empty() || anchors.len() > MAX_ANCHORS {
        return Err(note_err(format!("a note needs 1..={MAX_ANCHORS} anchors")));
    }
    Ok(Note {
        id: bounded("id", need(id, "id")?, 128)?,
        title: bounded("title", need(title, "title")?, MAX_TITLE_CHARS)?,
        kind: Kind::parse(&kind_s).ok_or_else(|| note_err(format!("unknown kind {kind_s:?}")))?,
        status: Status::parse(&status_s).ok_or_else(|| note_err(format!("unknown status {status_s:?}")))?,
        created,
        source: bounded("source", need(source, "source")?, 256)?,
        anchors,
        body: body.to_string(),
    })
}

fn render_anchors(out: &mut String, anchors: &[Anchor]) {
    out.push_str("anchors:\n");
    for a in anchors {
        out.push_str("  - ");
        out.push_str(&quote(&a.render()));
        out.push('\n');
    }
}

/// The file text of `n`, keys in the documented order. Fails unless the text parses back
/// into exactly `n` (a value [`parse`] would refuse, e.g. a title with a line break).
pub fn render(n: &Note) -> Result<String, DnaError> {
    let mut out = String::from("---\n");
    out.push_str(&format!("id: {}\n", plain_or_quoted(&n.id)));
    out.push_str(&format!("title: {}\n", quote(&n.title)));
    out.push_str(&format!("kind: {}\n", n.kind.as_str()));
    out.push_str(&format!("status: {}\n", n.status.as_str()));
    out.push_str(&format!("created: {}\n", n.created));
    out.push_str(&format!("source: {}\n", plain_or_quoted(&n.source)));
    render_anchors(&mut out, &n.anchors);
    out.push_str("---\n");
    out.push_str(&n.body);
    if parse(&out)? != *n {
        return Err(note_err("the note does not render to the same note"));
    }
    Ok(out)
}

/// `text` with only its `status:` line and/or its `anchors:` block rewritten; every other
/// byte (unknown keys, comments, the body) is copied verbatim. Lines are read exactly as
/// [`parse`] reads them, and the result must parse with the requested status and anchors.
pub fn update(text: &str, status: Option<Status>, anchors: Option<&[Anchor]>) -> Result<String, DnaError> {
    parse(text)?;
    if anchors.is_some_and(|a| a.is_empty() || a.len() > MAX_ANCHORS) {
        return Err(note_err(format!("a note needs 1..={MAX_ANCHORS} anchors")));
    }
    let mut out = String::with_capacity(text.len() + 256);
    let mut lines = text.split_inclusive('\n');
    out.push_str(lines.next().unwrap_or_default()); // the opening ---
    let mut in_anchors = false;
    for raw in lines.by_ref() {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line); // exactly as `split` reads it
        if line == "---" {
            out.push_str(raw);
            break;
        }
        if in_anchors {
            if anchor_item(line).is_some() {
                if anchors.is_none() {
                    out.push_str(raw);
                }
                continue;
            }
            in_anchors = false;
        }
        match key_value(line) {
            Some(("status", _)) => match status {
                Some(st) => out.push_str(&format!("status: {}\n", st.as_str())),
                None => out.push_str(raw),
            },
            Some(("anchors", _)) => {
                in_anchors = true;
                match anchors {
                    Some(list) => render_anchors(&mut out, list),
                    None => out.push_str(raw),
                }
            }
            _ => out.push_str(raw),
        }
    }
    out.extend(lines);
    let n = parse(&out)?;
    if status.is_some_and(|st| st != n.status) || anchors.is_some_and(|a| a != n.anchors.as_slice()) {
        return Err(note_err("the update did not take effect (unusual frontmatter layout)"));
    }
    Ok(out)
}

/// The slugs `body` links to (`[[slug]]`, `[[slug|alias]]`, `[[slug#heading]]`), deduped,
/// in order of first appearance.
pub fn links(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find("[[") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find("]]") else {
            break;
        };
        let inner = &rest[..j];
        if inner.contains(['\n', '[']) {
            continue;
        }
        rest = &rest[j + 2..];
        let target = inner.split(['|', '#']).next().unwrap_or_default();
        if target.chars().any(char::is_alphanumeric) {
            let slug = slugify(target);
            if !out.contains(&slug) {
                out.push(slug);
            }
        }
    }
    out
}

/// Common Latin letters with diacritics (lowercase), folded to ASCII. Shared with the skill
/// miner's tokenizer.
pub(crate) fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => "a",
        'ç' | 'ć' | 'č' => "c",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ě' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'ī' => "i",
        'ñ' | 'ń' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => "o",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' => "u",
        'ý' | 'ÿ' => "y",
        'š' | 'ś' => "s",
        'ž' | 'ź' | 'ż' => "z",
        'ł' => "l",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        _ => return None,
    })
}

/// ASCII kebab-case of `s`: lowercase, common accents folded, every other run of
/// characters a single `-`, at most 64 bytes; `"note"` when nothing is left.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars().flat_map(char::to_lowercase) {
        let piece = match c {
            'a'..='z' | '0'..='9' => Some(c.encode_utf8(&mut [0; 4]).to_string()),
            c => fold(c).map(str::to_string),
        };
        match piece {
            Some(p) => {
                if dash && !out.is_empty() {
                    out.push('-');
                }
                dash = false;
                out.push_str(&p);
            }
            None => dash = true,
        }
        if out.len() >= 64 {
            break;
        }
    }
    out.truncate(64);
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "note".to_string()
    } else {
        out.to_string()
    }
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`.
pub fn is_slug(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nid: 0193b7c2-5f1e-7c3a-9a10-3f2b8d1e4c55\ntitle: \"Spool: the \\\"fallback\\\"\"\nkind: gotcha\nstatus: active\ncreated: 2026-11-07\nsource: manual\ntags: [hooks]\nanchors:\n  - \"src/a.rs#run:L1-3@50ea8cc255941e739b13cdbe2c937abca0319834~9f2c4b1a7d3e5f60\"\n---\nBody with [[other-note]] and [[x|alias]] and [[y#h]].\n";

    #[test]
    fn parses_and_renders_the_documented_format() {
        let n = parse(SAMPLE).unwrap();
        assert_eq!(n.title, "Spool: the \"fallback\"");
        assert_eq!((n.kind, n.status), (Kind::Gotcha, Status::Active));
        assert_eq!(n.anchors.len(), 1);
        assert_eq!(parse(&render(&n).unwrap()).unwrap(), n);
    }

    #[test]
    fn links_are_slugs() {
        assert_eq!(links(&parse(SAMPLE).unwrap().body), ["other-note", "x", "y"]);
    }

    /// The cockpit resolves [[links]] itself (apps/cockpit/src/lib/dna.ts): both sides
    /// check the same vectors.
    #[test]
    fn slugify_and_links_match_the_shared_vectors() {
        let v: serde_json::Value = serde_json::from_str(include_str!("link-vectors.json")).unwrap();
        for case in v["slugify"].as_array().unwrap() {
            let (input, want) = (case[0].as_str().unwrap(), case[1].as_str().unwrap());
            assert_eq!(slugify(input), want, "slugify({input:?})");
        }
        for case in v["links"].as_array().unwrap() {
            let want: Vec<&str> = case[1].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
            assert_eq!(links(case[0].as_str().unwrap()), want, "links({:?})", case[0]);
        }
    }

    #[test]
    fn rejects_missing_fields_bad_kind_no_anchors_and_oversize() {
        assert!(parse("no frontmatter").is_err());
        assert!(parse(&SAMPLE.replace("kind: gotcha", "kind: rumor")).is_err());
        assert!(parse(&SAMPLE.replace(
            "anchors:\n  - \"src/a.rs#run:L1-3@50ea8cc255941e739b13cdbe2c937abca0319834~9f2c4b1a7d3e5f60\"\n",
            "anchors:\n"
        ))
        .is_err());
        assert!(parse(&format!("{SAMPLE}{}", "x".repeat(MAX_NOTE_BYTES))).is_err());
    }

    #[test]
    fn update_keeps_unknown_lines_and_the_body_byte_for_byte() {
        let n2 = update(SAMPLE, Some(Status::Retired), None).unwrap();
        assert!(n2.contains("tags: [hooks]\n") && n2.contains("status: retired\n"));
        assert_eq!(n2.split("\n---\n").nth(1), SAMPLE.split("\n---\n").nth(1));
    }

    #[test]
    fn update_rewrites_only_the_anchors_block() {
        let a = Anchor::parse("src/b.rs:L2-5@50ea8cc255941e739b13cdbe2c937abca0319834~0123456789abcdef").unwrap();
        let n2 = update(SAMPLE, None, Some(std::slice::from_ref(&a))).unwrap();
        let n = parse(&n2).unwrap();
        assert_eq!((n.anchors, n.status), (vec![a], Status::Active));
        assert!(n2.contains("tags: [hooks]\n") && !n2.contains("src/a.rs"));
        assert!(update(SAMPLE, None, Some(&[])).is_err());
    }

    #[test]
    fn update_and_parse_agree_on_the_closing_line() {
        // `---\r\r` is not a closing line for parse; update must not treat it as one.
        let text = SAMPLE.replace("kind: gotcha\n", "kind: gotcha\n---\r\r\n");
        assert_eq!(parse(&text).unwrap().status, Status::Active);
        let out = update(&text, Some(Status::Retired), None).unwrap();
        assert_eq!(parse(&out).unwrap().status, Status::Retired);
    }

    #[test]
    fn render_round_trips_or_fails() {
        let n = parse(SAMPLE).unwrap();
        assert_eq!(parse(&render(&n).unwrap()).unwrap(), n);
        let bad = Note { title: "two\nlines".into(), ..n.clone() };
        assert!(render(&bad).is_err());
        assert_eq!(scalar(&quote("a\nb\r\"c\\")).unwrap(), "a\nb\r\"c\\");
        assert!(!quote("a\nb").contains('\n'));
    }

    #[test]
    fn slugify_is_ascii_kebab_and_bounded() {
        assert_eq!(slugify("Hook events: São Paulo/Δ!"), "hook-events-sao-paulo");
        assert_eq!(slugify("!!!"), "note");
        assert!(slugify(&"a ".repeat(200)).len() <= 64);
        assert!(is_slug("hook-events") && !is_slug("../x") && !is_slug("A"));
    }
}
