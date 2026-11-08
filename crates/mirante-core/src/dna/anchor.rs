//! Anchors: where a note cites code. `path[#symbol]:L<start>-<end>@<commit>~<fingerprint>`.
use serde::{Deserialize, Serialize};

use super::DnaError;

/// One citation of committed code. Lines are 1-based and inclusive; `fingerprint` is the
/// first 16 hex of SHA-256 over the cited lines (each right-trimmed, joined with `\n`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub start: u32,
    pub end: u32,
    pub commit: String,
    pub fingerprint: String,
}

/// Where an anchor's cited text is in a file now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Located {
    Same,
    Moved {
        start: u32,
        end: u32,
    },
    /// The cited text is not in the file. `symbol_line`: where the symbol is now, if found.
    Changed {
        symbol_line: Option<u32>,
        reason: String,
    },
}

const SENSITIVE_NAMES: [&str; 4] = [".npmrc", ".netrc", ".pypirc", ".git-credentials"];
const SENSITIVE_PREFIXES: [&str; 3] = [".env", "id_rsa", "id_ed25519"];
const SENSITIVE_EXT: [&str; 5] = [".pem", ".key", ".p12", ".pfx", ".keystore"];

/// A path an anchor may cite: repo-relative, `/`-separated, no `.`/`..`/empty segment, no
/// NUL, backslash or `#`, not inside `.git` or `.mirante`, and not a secrets file.
pub fn check_path(p: &str) -> Result<(), DnaError> {
    let bad = |why| Err(DnaError::Path(p.to_string(), why));
    if p.is_empty() || p.len() > 512 {
        return bad("empty or longer than 512 bytes");
    }
    if p.starts_with('/') || p.contains(['\0', '\\', '#']) {
        return bad("must be repo-relative, no NUL, backslash or #");
    }
    let segs: Vec<&str> = p.split('/').collect();
    if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return bad("no empty, . or .. segments");
    }
    if matches!(segs[0], ".git" | ".mirante") {
        return bad("inside .git or .mirante");
    }
    let name = segs[segs.len() - 1];
    if SENSITIVE_NAMES.contains(&name)
        || SENSITIVE_PREFIXES.iter().any(|x| name.starts_with(x))
        || SENSITIVE_EXT.iter().any(|x| name.ends_with(x))
    {
        return bad("looks like a secrets file");
    }
    Ok(())
}

fn is_symbol(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 120
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '.' | '$'))
}

fn is_hex(s: &str, lens: &[usize]) -> bool {
    lens.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl Anchor {
    pub fn parse(s: &str) -> Result<Anchor, DnaError> {
        let err = |why| DnaError::Anchor(s.to_string(), why);
        let (rest, fp) = s.rsplit_once('~').ok_or(err("missing ~fingerprint"))?;
        let (rest, commit) = rest.rsplit_once('@').ok_or(err("missing @commit"))?;
        let (target, lines) = rest.rsplit_once(":L").ok_or(err("missing :L<start>-<end>"))?;
        let (a, b) = lines.split_once('-').ok_or(err("lines must be L<start>-<end>"))?;
        let (start, end): (u32, u32) =
            (a.parse().map_err(|_| err("bad start"))?, b.parse().map_err(|_| err("bad end"))?);
        if start == 0 || end < start || end - start > 400 {
            return Err(err("lines must be 1-based, ordered, at most 400 long"));
        }
        if !is_hex(commit, &[40, 64]) {
            return Err(err("commit must be a full lowercase sha"));
        }
        if !is_hex(fp, &[16]) {
            return Err(err("fingerprint must be 16 lowercase hex"));
        }
        let (path, symbol) = match target.split_once('#') {
            Some((p, sym)) if is_symbol(sym) => (p, Some(sym.to_string())),
            Some(_) => return Err(err("bad symbol")),
            None => (target, None),
        };
        check_path(path)?;
        Ok(Anchor { path: path.into(), symbol, start, end, commit: commit.into(), fingerprint: fp.into() })
    }

    pub fn render(&self) -> String {
        let sym = self.symbol.as_ref().map(|s| format!("#{s}")).unwrap_or_default();
        format!("{}{sym}:L{}-{}@{}~{}", self.path, self.start, self.end, self.commit, self.fingerprint)
    }
}

/// First 16 hex of SHA-256 over `lines`, each right-trimmed (so `\r` and trailing spaces do
/// not count), joined with `\n`.
pub fn fingerprint<S: AsRef<str>>(lines: &[S]) -> String {
    let joined = lines.iter().map(|l| l.as_ref().trim_end()).collect::<Vec<_>>().join("\n");
    super::sha256_hex(joined.as_bytes())[..16].to_string()
}

/// The `start..=end` lines (1-based) of `text`, if it has them.
pub fn slice(text: &str, start: u32, end: u32) -> Option<Vec<&str>> {
    let lines: Vec<&str> = text.lines().collect();
    (start >= 1 && start <= end && end as usize <= lines.len())
        .then(|| lines[start as usize - 1..end as usize].to_vec())
}

/// Where `a`'s cited text is in `text` (the file now).
pub fn locate(a: &Anchor, text: &str) -> Located {
    let lines: Vec<&str> = text.lines().collect();
    let n = (a.end - a.start + 1) as usize;
    if slice(text, a.start, a.end).is_some_and(|w| fingerprint(&w) == a.fingerprint) {
        return Located::Same;
    }
    let hint = a.start as i64 - 1;
    let best = (0..(lines.len() + 1).saturating_sub(n))
        .filter(|&i| fingerprint(&lines[i..i + n]) == a.fingerprint)
        .min_by_key(|&i| (i as i64 - hint).abs());
    if let Some(i) = best {
        return Located::Moved { start: i as u32 + 1, end: (i + n) as u32 };
    }
    let symbol_line = a.symbol.as_deref().and_then(|s| find_symbol(&lines, s)).map(|i| i as u32 + 1);
    let reason = match symbol_line {
        Some(l) => format!(
            "the code cited at {}:{}-{} changed ({} is now at line {l})",
            a.path,
            a.start,
            a.end,
            a.symbol.as_deref().unwrap_or("")
        ),
        None => format!("the code cited at {}:{}-{} is no longer in the file", a.path, a.start, a.end),
    };
    Located::Changed { symbol_line, reason }
}

const DEFINERS: &[&str] = &[
    "fn",
    "struct",
    "enum",
    "trait",
    "impl",
    "type",
    "const",
    "static",
    "mod",
    "class",
    "function",
    "def",
    "interface",
    "let",
    "var",
    "export",
];

fn words(line: &str) -> Vec<&str> {
    line.split(|c: char| !ident_byte(c)).filter(|w| !w.is_empty()).collect()
}

fn ident_byte(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// First line that defines `symbol`'s last segment (`Hub::new` → `new`) after a definition
/// keyword, else the first line containing it as a whole word.
fn find_symbol(lines: &[&str], symbol: &str) -> Option<usize> {
    let name = symbol.rsplit([':', '.']).find(|s| !s.is_empty())?;
    let defines = lines.iter().position(|l| {
        let w = words(l);
        w.windows(2).any(|p| DEFINERS.contains(&p[0]) && p[1] == name)
    });
    defines.or_else(|| lines.iter().position(|l| words(l).contains(&name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor_over(text: &str, start: u32, end: u32, symbol: Option<&str>) -> Anchor {
        let lines = slice(text, start, end).unwrap();
        Anchor {
            path: "src/a.rs".into(),
            symbol: symbol.map(str::to_string),
            start,
            end,
            commit: "0".repeat(40),
            fingerprint: fingerprint(&lines),
        }
    }

    #[test]
    fn round_trips_the_grammar() {
        let s = "crates/x/src/lib.rs#Hub::new:L10-24@50ea8cc255941e739b13cdbe2c937abca0319834~9f2c4b1a7d3e5f60";
        let a = Anchor::parse(s).unwrap();
        assert_eq!(
            (a.path.as_str(), a.symbol.as_deref(), a.start, a.end),
            ("crates/x/src/lib.rs", Some("Hub::new"), 10, 24)
        );
        assert_eq!(a.render(), s);
        assert!(Anchor::parse("a.rs:L5-4@50ea8cc255941e739b13cdbe2c937abca0319834~9f2c4b1a7d3e5f60").is_err()); // end < start
        assert!(Anchor::parse("a.rs:L1-2@nothex~9f2c4b1a7d3e5f60").is_err());
    }

    #[test]
    fn refuses_unsafe_and_sensitive_paths() {
        for p in [
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "./a",
            ".git/config",
            "a\\b",
            "a#b",
            "",
            ".env",
            "cfg/.env.local",
            "certs/server.pem",
            "k.key",
            "home/id_rsa",
            ".npmrc",
            ".mirante/memory/x.md",
        ] {
            assert!(check_path(p).is_err(), "{p} must be refused");
        }
        for p in ["src/main.rs", "docs/a b.md", "crates/x/.gitignore"] {
            assert!(check_path(p).is_ok(), "{p} must pass");
        }
    }

    #[test]
    fn fingerprint_ignores_trailing_space_and_crlf() {
        assert_eq!(fingerprint(&["fn a() {", "}"]), fingerprint(&["fn a() {  ", "}\r"]));
        assert_ne!(fingerprint(&["fn a() {", "}"]), fingerprint(&["fn a() {", " }"]));
        assert_eq!(fingerprint(&["x"]).len(), 16);
    }

    #[test]
    fn locate_same_moved_changed_gone() {
        let old = "a\nfn spool() {\n  write();\n}\nz\n";
        let a = anchor_over(old, 2, 4, Some("spool"));
        assert_eq!(locate(&a, old), Located::Same);
        let moved = "header\n\na\nfn spool() {\n  write();\n}\nz\n";
        assert_eq!(locate(&a, moved), Located::Moved { start: 4, end: 6 });
        let changed = "a\nfn spool() {\n  write_twice();\n}\nz\n";
        assert!(matches!(locate(&a, changed), Located::Changed { symbol_line: Some(2), .. }));
        assert!(matches!(locate(&a, "nothing here\n"), Located::Changed { symbol_line: None, .. }));
    }

    #[test]
    fn moved_picks_the_copy_nearest_the_old_lines() {
        let a = anchor_over("x\ny\n", 1, 1, None);
        assert_eq!(locate(&a, "q\nx\nq\nq\nq\nq\nx\n"), Located::Moved { start: 2, end: 2 });
    }
}
