//! Anchors: where a note cites code. `path[#symbol]:L<start>-<end>@<commit>~<fingerprint>`.
use serde::{Deserialize, Serialize};

use super::DnaError;

/// One citation of committed code. Lines are 1-based and inclusive; `fingerprint` is the
/// first 16 hex of SHA-256 over the cited lines (each right-trimmed, joined with `\n`).
/// Deserializing validates exactly like [`Anchor::parse`]: an anchor from JSON (a stored
/// proposal, a wire message) can never carry a commit or path that git would read as an
/// option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawAnchor")]
pub struct Anchor {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub start: u32,
    pub end: u32,
    pub commit: String,
    pub fingerprint: String,
}

#[derive(Deserialize)]
struct RawAnchor {
    path: String,
    #[serde(default)]
    symbol: Option<String>,
    start: u32,
    end: u32,
    commit: String,
    fingerprint: String,
}

impl TryFrom<RawAnchor> for Anchor {
    type Error = DnaError;

    fn try_from(r: RawAnchor) -> Result<Anchor, DnaError> {
        let a = Anchor {
            path: r.path,
            symbol: r.symbol,
            start: r.start,
            end: r.end,
            commit: r.commit,
            fingerprint: r.fingerprint,
        };
        a.validate()?;
        Ok(a)
    }
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

/// Compared in ASCII lowercase (APFS and NTFS are case-insensitive).
const SENSITIVE_NAMES: [&str; 5] = [".npmrc", ".netrc", ".pypirc", ".git-credentials", "credentials"];
const SENSITIVE_PREFIXES: [&str; 5] = [".env", "id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"];
const SENSITIVE_EXT: [&str; 6] = [".pem", ".key", ".p12", ".pfx", ".keystore", ".kdbx"];
/// Directories whose files are never cited, at any depth.
const SENSITIVE_DIRS: [&str; 5] = [".git", ".mirante", ".aws", ".ssh", ".gnupg"];

/// A path an anchor may cite: repo-relative, `/`-separated, no `.`/`..`/empty segment, no
/// control character, backslash or `#`, not inside `.git`, `.mirante`, `.aws`, `.ssh` or
/// `.gnupg`, and not a secrets file. Name checks ignore ASCII case.
pub fn check_path(p: &str) -> Result<(), DnaError> {
    let bad = |why| Err(DnaError::Path(p.to_string(), why));
    if p.is_empty() || p.len() > 512 {
        return bad("empty or longer than 512 bytes");
    }
    if p.starts_with('/') || p.contains(['\\', '#']) || p.chars().any(char::is_control) {
        return bad("must be repo-relative, no control character, backslash or #");
    }
    let lower = p.to_ascii_lowercase();
    let segs: Vec<&str> = lower.split('/').collect();
    if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return bad("no empty, . or .. segments");
    }
    if segs.iter().any(|s| SENSITIVE_DIRS.contains(s)) {
        return bad("inside .git, .mirante or a credentials directory");
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

/// A line number: ASCII digits, no sign, no leading zero.
fn line_number(s: &str) -> Option<u32> {
    let ok = !s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0');
    ok.then(|| s.parse().ok()).flatten()
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
        let (start, end): (u32, u32) = (line_number(a).ok_or(err("bad start"))?, line_number(b).ok_or(err("bad end"))?);
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

    /// Ok when the anchor is exactly what [`Anchor::parse`] makes of its rendering (every
    /// grammar rule holds; fields built in code or read from JSON are checked the same way).
    pub fn validate(&self) -> Result<(), DnaError> {
        let text = self.render();
        if Anchor::parse(&text)? != *self {
            return Err(DnaError::Anchor(text, "does not round-trip"));
        }
        Ok(())
    }

    pub fn render(&self) -> String {
        let sym = self.symbol.as_ref().map(|s| format!("#{s}")).unwrap_or_default();
        format!("{}{sym}:L{}-{}@{}~{}", self.path, self.start, self.end, self.commit, self.fingerprint)
    }
}

/// First 16 hex of SHA-256 over `lines`, each right-trimmed, joined with `\n`. Trailing
/// whitespace (and `\r`) is ignored on purpose: a change that only adds or removes trailing
/// spaces does not make an anchor stale.
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

/// Most line comparisons (or fingerprinted lines) one [`locate_with`] spends looking for a
/// moved copy. A file crafted so that every window nearly matches stops here, not after
/// `lines × window` work.
const LOCATE_BUDGET: usize = 2_000_000;

/// Window starts in `0..windows`, nearest to `hint` first (the lower one on a tie).
fn nearest_first(hint: usize, windows: usize) -> impl Iterator<Item = usize> {
    (1..windows.max(hint + 1))
        .flat_map(move |d| [hint.checked_sub(d), Some(hint + d)])
        .flatten()
        .filter(move |&i| i < windows)
}

/// Where `a`'s cited text is in `text` (the file now). See [`locate_with`].
pub fn locate(a: &Anchor, text: &str) -> Located {
    locate_with(a, None, text)
}

/// Where `a`'s cited text is in `text` (the file now). `cited`: the cited lines as read at
/// the anchor's commit, used when they match its fingerprint; a moved copy is then found by
/// comparing lines (windows whose first and last lines differ are skipped at once) instead
/// of fingerprinting every window. The search goes outward from the old position and stops
/// after [`LOCATE_BUDGET`] comparisons.
pub fn locate_with(a: &Anchor, cited: Option<&[&str]>, text: &str) -> Located {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let n = (a.end - a.start + 1) as usize;
    let hint = a.start as usize - 1;
    if hint + n <= lines.len() && fingerprint(&lines[hint..hint + n]) == a.fingerprint {
        return Located::Same;
    }
    let cited: Option<Vec<&str>> = cited
        .filter(|c| c.len() == n && fingerprint(c) == a.fingerprint)
        .map(|c| c.iter().map(|l| l.trim_end()).collect());
    let windows = (lines.len() + 1).saturating_sub(n);
    let mut budget = LOCATE_BUDGET;
    let mut found = None;
    for i in nearest_first(hint, windows) {
        let window = &lines[i..i + n];
        let hit = match &cited {
            Some(c) => {
                budget = budget.saturating_sub(1);
                window[0] == c[0] && window[n - 1] == c[n - 1] && {
                    let same = window.iter().zip(c).take_while(|(x, y)| x == y).count();
                    budget = budget.saturating_sub(same);
                    same == n
                }
            }
            None => {
                budget = budget.saturating_sub(n);
                fingerprint(window) == a.fingerprint
            }
        };
        if hit {
            found = Some(i);
            break;
        }
        if budget == 0 {
            break;
        }
    }
    if let Some(i) = found {
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
            ".ENV",
            "a/ID_RSA",
            ".Mirante/memory/x.md",
            ".GIT/config",
            "a\u{1}b",
            "a\nb",
            "a\u{85}b",
            "home/.ssh/id_ecdsa",
            "home/.ssh/id_dsa",
            ".aws/config",
            "x/.aws/credentials",
            "credentials",
            "vault.kdbx",
            "Server.PEM",
        ] {
            assert!(check_path(p).is_err(), "{p} must be refused");
        }
        for p in ["src/main.rs", "docs/a b.md", "crates/x/.gitignore"] {
            assert!(check_path(p).is_ok(), "{p} must pass");
        }
    }

    #[test]
    fn line_numbers_are_plain_decimal() {
        let c = "50ea8cc255941e739b13cdbe2c937abca0319834~9f2c4b1a7d3e5f60";
        assert!(Anchor::parse(&format!("a.rs:L1-2@{c}")).is_ok());
        for bad in ["L+1-2", "L01-2", "L1-02", "L1-+2", "L-2", "L1-"] {
            assert!(Anchor::parse(&format!("a.rs:{bad}@{c}")).is_err(), "{bad}");
        }
    }

    #[test]
    fn deserializing_validates_like_parse() {
        let good = anchor_over("x\n", 1, 1, None);
        let json = serde_json::to_string(&good).unwrap();
        assert_eq!(serde_json::from_str::<Anchor>(&json).unwrap(), good);
        let bad_commit = json.replace(&"0".repeat(40), "--output=/tmp/pwned-0000000000000000000000");
        assert!(serde_json::from_str::<Anchor>(&bad_commit).is_err());
        let bad_path = json.replace("src/a.rs", "../../etc/passwd");
        assert!(serde_json::from_str::<Anchor>(&bad_path).is_err());
        assert!(good.validate().is_ok());
        assert!(Anchor { commit: "-x".into(), ..good }.validate().is_err());
    }

    #[test]
    fn locate_is_bounded_on_pathological_files() {
        // 401 cited lines whose first and last lines match every window of the file.
        let mut cited = vec!["x"; 200];
        cited.push("y");
        cited.extend(vec!["x"; 200]);
        let cited_text = cited.join("\n") + "\n";
        let a = anchor_over(&cited_text, 1, 401, None);
        let file = "x\n".repeat(200_000);
        let t = std::time::Instant::now();
        assert!(matches!(locate_with(&a, Some(&cited), &file), Located::Changed { .. }));
        assert!(matches!(locate(&a, &file), Located::Changed { .. }));
        assert!(t.elapsed() < std::time::Duration::from_secs(3), "{:?}", t.elapsed());
        // The cited lines still match a copy that moved.
        let moved = format!("q\n{cited_text}");
        assert_eq!(locate_with(&a, Some(&cited), &moved), Located::Moved { start: 2, end: 402 });
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
