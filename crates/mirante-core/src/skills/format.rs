//! The skill format: `SKILL.md` as Claude Code and Codex read it, and the `mirante.skill/v1`
//! manifest that pins a version's exact files.
//!
//! Mirante renders `SKILL.md` itself: frontmatter `name` and `description` only (the keys
//! both CLIs accept), then the body. An imported `SKILL.md` may carry other keys; they are
//! reported (`extra_keys`) so lint can warn, or refuse `allowed-tools` and `hooks`, which
//! grant Claude permissions.
//!
//! The manifest is serialized canonically (field order as declared, compact JSON, a
//! trailing `\n`); its hash is the SHA-256 of exactly those bytes, and the signature covers
//! exactly those bytes. [`Manifest::parse`] accepts only canonical bytes.
use serde::{Deserialize, Serialize};

use super::origin::Scope;
use super::{SkillError, FORMAT, MANIFEST, MAX_BODY, MAX_DESCRIPTION, MAX_FILE, MAX_FILES, MAX_NAME, MAX_TOTAL};
use super::{SIGNATURE, SKILL_MD};
use crate::dna::sha256_hex;

fn bad(msg: impl Into<String>) -> SkillError {
    SkillError::Format(msg.into())
}

/// When a skill reaches the agent: always (in the system prompt / first turn) or on
/// demand (the agent loads it when relevant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    Always,
    OnDemand,
}

impl Activation {
    pub fn as_str(self) -> &'static str {
        match self {
            Activation::Always => "always",
            Activation::OnDemand => "on_demand",
        }
    }

    pub fn parse(s: &str) -> Option<Activation> {
        match s {
            "always" => Some(Activation::Always),
            "on_demand" => Some(Activation::OnDemand),
            _ => None,
        }
    }
}

/// Where a version came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Miner,
    Agent,
    Cli,
    Import,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub source: Source,
    pub task: Option<String>,
    pub proposal: Option<String>,
    /// The tenant the skill was first learned in, carried from version to version (an
    /// accepted edit, a rescope, a rollback), so widening it later still warns. Absent
    /// when it was never learned in a tenant (and in versions written before this field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learned_in: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignerRef {
    /// The `allowed_signers` principal (`mirante-local`).
    pub principal: String,
    /// The key's fingerprint (`SHA256:…`).
    pub key: String,
}

/// One file of a version, as the manifest pins it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

impl FileEntry {
    pub fn of(path: &str, bytes: &[u8]) -> FileEntry {
        FileEntry { path: path.to_string(), sha256: sha256_hex(bytes), bytes: bytes.len() as u64 }
    }
}

/// The manifest's `scope` object: `{"kind":"org","value":"github.com/acme"}`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeObject {
    kind: String,
    value: String,
}

mod scope_object {
    use super::{Scope, ScopeObject};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(s: &Scope, ser: S) -> Result<S::Ok, S::Error> {
        ScopeObject { kind: s.kind().into(), value: s.value().into() }.serialize(ser)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Scope, D::Error> {
        let o = ScopeObject::deserialize(de)?;
        Scope::from_parts(&o.kind, &o.value).map_err(serde::de::Error::custom)
    }
}

/// `mirante-skill.json`. Field order is the serialization order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub name: String,
    pub version: u32,
    pub description: String,
    pub activation: Activation,
    /// `claude` and/or `codex`.
    pub agents: Vec<String>,
    #[serde(with = "scope_object")]
    pub scope: Scope,
    /// `SKILL.md` first, then the extra files in path order.
    pub files: Vec<FileEntry>,
    /// The previous version's manifest hash.
    pub parent: Option<String>,
    pub provenance: Provenance,
    pub signer: SignerRef,
    /// `YYYY-MM-DD`.
    pub created: String,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

/// A description both CLIs accept: 1..=1024 chars, one line, no `<` or `>`.
pub fn check_description(d: &str) -> Result<(), SkillError> {
    if d.trim().is_empty() || d.chars().count() > MAX_DESCRIPTION {
        return Err(bad(format!("description must be 1..={MAX_DESCRIPTION} chars")));
    }
    if d.chars().any(|c| c.is_control() || c == '<' || c == '>') {
        return Err(bad("description must be one line without < or >"));
    }
    Ok(())
}

impl Manifest {
    /// The canonical bytes: compact JSON in field order, then `\n`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = serde_json::to_vec(self).expect("a manifest always serializes");
        out.push(b'\n');
        out
    }

    /// SHA-256 of [`Manifest::to_bytes`]: what the ledger records and the signature covers.
    pub fn hash(&self) -> String {
        sha256_hex(&self.to_bytes())
    }

    /// A manifest from its file bytes: canonical bytes only, every field valid.
    pub fn parse(bytes: &[u8]) -> Result<Manifest, SkillError> {
        if bytes.len() > 64 * 1024 {
            return Err(bad("manifest larger than 64 KiB"));
        }
        let m: Manifest = serde_json::from_slice(bytes).map_err(|e| bad(format!("manifest: {e}")))?;
        m.validate()?;
        if m.to_bytes() != bytes {
            return Err(bad("manifest is not in canonical form"));
        }
        Ok(m)
    }

    /// Every rule of the format: tag, name, version, description, agents, the file list
    /// (`SKILL.md` first, then unique extra text files in path order, within the limits),
    /// parent hash, signer and date.
    pub fn validate(&self) -> Result<(), SkillError> {
        if self.format != FORMAT {
            return Err(bad(format!("unknown format {:?}", self.format)));
        }
        if !is_name(&self.name) {
            return Err(bad(format!("{:?} is not a skill name", self.name)));
        }
        if self.version == 0 {
            return Err(bad("versions start at 1"));
        }
        check_description(&self.description)?;
        let known = |a: &String| a == "claude" || a == "codex";
        if self.agents.is_empty() || !self.agents.iter().all(known) || !self.agents.windows(2).all(|w| w[0] < w[1]) {
            return Err(bad("agents must be a sorted, non-empty subset of claude and codex"));
        }
        let (first, extra) = self.files.split_first().ok_or_else(|| bad("no files"))?;
        if first.path != SKILL_MD {
            return Err(bad("the first file must be SKILL.md"));
        }
        if extra.len() > MAX_FILES {
            return Err(bad(format!("more than {MAX_FILES} extra files")));
        }
        if !extra.windows(2).all(|w| w[0].path < w[1].path) {
            return Err(bad("extra files must be unique and in path order"));
        }
        let mut total = 0u64;
        for (i, f) in self.files.iter().enumerate() {
            if i > 0 {
                check_file_path(&f.path)?;
            }
            let max = if i == 0 { (MAX_BODY + 4096) as u64 } else { MAX_FILE as u64 };
            if !is_hex64(&f.sha256) || f.bytes > max {
                return Err(bad(format!("{}: bad hash or size", f.path)));
            }
            total += f.bytes;
        }
        if total > MAX_TOTAL as u64 {
            return Err(bad(format!("files larger than {MAX_TOTAL} bytes in all")));
        }
        if self.parent.as_deref().is_some_and(|p| !is_hex64(p)) {
            return Err(bad("parent must be a manifest hash"));
        }
        if self.signer.principal.is_empty() || !self.signer.key.starts_with("SHA256:") {
            return Err(bad("signer needs a principal and a SHA256: key fingerprint"));
        }
        if !is_date(&self.created) {
            return Err(bad("created must be YYYY-MM-DD"));
        }
        Ok(())
    }
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`, without `claude` or `anthropic` (reserved by Claude Code).
pub fn is_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_NAME
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !s.contains("claude")
        && !s.contains("anthropic")
}

/// An extra file's path: relative, `/`-separated, at most 3 levels and 128 bytes, each
/// segment `[A-Za-z0-9._-]` not starting with `.`, ending in `.md` or `.txt` (text only:
/// scripts are deferred), and not one of the version's own names.
pub fn check_file_path(p: &str) -> Result<(), SkillError> {
    let refuse = |why: &str| Err(bad(format!("file path {p:?}: {why}")));
    if p.is_empty() || p.len() > 128 || p.starts_with('/') {
        return refuse("must be relative, 1..=128 bytes");
    }
    let segs: Vec<&str> = p.split('/').collect();
    if segs.len() > 3 {
        return refuse("at most 3 levels");
    }
    for s in &segs {
        if s.is_empty()
            || s.starts_with('.')
            || !s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return refuse("segments are [A-Za-z0-9._-] and never start with .");
        }
    }
    if !(p.ends_with(".md") || p.ends_with(".txt")) {
        return refuse("only .md and .txt files");
    }
    if [SKILL_MD, MANIFEST, SIGNATURE].iter().any(|n| p.eq_ignore_ascii_case(n)) {
        return refuse("reserved name");
    }
    Ok(())
}

/// `v` double-quoted on one line (`\\`, `"`, line breaks and tabs escaped).
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

/// `SKILL.md` as Mirante writes it: `name` and a quoted `description`, then the body.
pub fn render_skill_md(name: &str, description: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {}\n---\n{body}", quote(description))
}

/// A `SKILL.md` read back: its name, description and body, plus any other top-level
/// frontmatter keys, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSkill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub extra_keys: Vec<String>,
}

/// A plain value (trimmed), a double-quoted one (`\"`, `\\`, `\n`, `\r`, `\t`) or a
/// single-quoted one (`''` for a quote).
fn scalar(v: &str) -> Result<String, SkillError> {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('\'') {
        let inner = inner.strip_suffix('\'').ok_or_else(|| bad(format!("unterminated quote in {v}")))?;
        return Ok(inner.replace("''", "'"));
    }
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
                _ => return Err(bad(format!("unsupported escape in {v}"))),
            },
            '"' if chars.as_str().trim().is_empty() => return Ok(out),
            '"' => return Err(bad(format!("text after the closing quote in {v}"))),
            c => out.push(c),
        }
    }
    Err(bad(format!("unterminated quote in {v}")))
}

/// A `SKILL.md`: `---`, `key: value` lines at column 0 (indented and list lines belong to
/// the key above and are skipped), `---`, then the body. `name` and `description` are
/// required, once each; every other key is listed in `extra_keys`.
pub fn parse_skill_md(text: &str) -> Result<ParsedSkill, SkillError> {
    if text.len() > MAX_BODY + 8 * 1024 {
        return Err(bad("SKILL.md is too large"));
    }
    let mut rest = text;
    let mut first = true;
    let (mut name, mut description) = (None, None);
    let mut extra_keys = Vec::new();
    loop {
        if rest.is_empty() {
            return Err(bad("frontmatter is not closed with ---"));
        }
        let (line, next) = match rest.find('\n') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        rest = next;
        if first {
            if line != "---" {
                return Err(bad("must start with a --- frontmatter line"));
            }
            first = false;
            continue;
        }
        if line == "---" {
            break;
        }
        if line.is_empty() || line.starts_with([' ', '\t', '#', '-']) {
            continue;
        }
        let (k, v) = line.split_once(':').ok_or_else(|| bad(format!("not a key: value line: {line:?}")))?;
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err(bad(format!("bad key {k:?}")));
        }
        let slot = match k {
            "name" => &mut name,
            "description" => &mut description,
            other => {
                if !extra_keys.iter().any(|e| e == other) {
                    extra_keys.push(other.to_string());
                }
                continue;
            }
        };
        if slot.is_some() {
            return Err(bad(format!("{k} appears twice")));
        }
        *slot = Some(scalar(v)?);
    }
    Ok(ParsedSkill {
        name: name.ok_or_else(|| bad("missing name"))?,
        description: description.ok_or_else(|| bad("missing description"))?,
        body: rest.to_string(),
        extra_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        let md = render_skill_md("cargo-tests-first", "Run cargo tests first", "Body\n");
        Manifest {
            format: FORMAT.into(),
            name: "cargo-tests-first".into(),
            version: 3,
            description: "Run cargo tests first".into(),
            activation: Activation::Always,
            agents: vec!["claude".into(), "codex".into()],
            scope: Scope::org("github.com/sirus-alpha-canis"),
            files: vec![FileEntry::of(SKILL_MD, md.as_bytes()), FileEntry::of("reference/a.md", b"ref\n")],
            parent: Some("a".repeat(64)),
            provenance: Provenance { source: Source::Miner, task: None, proposal: Some("p1".into()), learned_in: None },
            signer: SignerRef { principal: "mirante-local".into(), key: "SHA256:abc".into() },
            created: "2026-11-08".into(),
        }
    }

    #[test]
    fn manifest_bytes_are_canonical_and_hash_stable() {
        let m = sample_manifest();
        assert_eq!(m.to_bytes(), Manifest::parse(&m.to_bytes()).unwrap().to_bytes());
        assert!(m.to_bytes().ends_with(b"}\n"));
        assert_eq!(m.hash(), sha256_hex(&m.to_bytes()));
        let text = String::from_utf8(m.to_bytes()).unwrap();
        assert!(text.starts_with(r#"{"format":"mirante.skill/v1","name":"cargo-tests-first","version":3,"#), "{text}");
        assert!(text.contains(r#""scope":{"kind":"org","value":"github.com/sirus-alpha-canis"}"#), "{text}");
        assert!(text.contains(r#""activation":"always""#));
        // Not canonical (pretty-printed, or without the newline): refused.
        let pretty = serde_json::to_vec_pretty(&m).unwrap();
        assert!(Manifest::parse(&pretty).is_err());
        assert!(Manifest::parse(&m.to_bytes()[..m.to_bytes().len() - 1]).is_err());
        // Unknown fields and a widened, invalid scope are refused.
        let extra = text.replacen("{\"format\"", "{\"x\":1,\"format\"", 1);
        assert!(Manifest::parse(extra.as_bytes()).is_err());
        let wide = text.replace("github.com/sirus-alpha-canis", "github.com");
        assert!(Manifest::parse(wide.as_bytes()).is_err());
        let personal = Manifest { scope: Scope::Personal, ..m };
        assert!(String::from_utf8(personal.to_bytes()).unwrap().contains(r#""scope":{"kind":"personal","value":""}"#));
        assert_eq!(Manifest::parse(&personal.to_bytes()).unwrap(), personal);
    }

    #[test]
    fn manifest_validation_pins_the_file_list() {
        let m = sample_manifest();
        let mut no_skill = m.clone();
        no_skill.files.remove(0);
        assert!(no_skill.validate().is_err());
        let mut dup = m.clone();
        dup.files.push(dup.files[1].clone());
        assert!(dup.validate().is_err());
        let mut script = m.clone();
        script.files[1].path = "run.sh".into();
        assert!(script.validate().is_err());
        assert!(Manifest { agents: vec!["codex".into(), "claude".into()], ..m.clone() }.validate().is_err());
        assert!(Manifest { agents: vec!["gpt".into()], ..m.clone() }.validate().is_err());
        assert!(Manifest { version: 0, ..m.clone() }.validate().is_err());
        assert!(Manifest { parent: Some("x".into()), ..m.clone() }.validate().is_err());
        assert!(Manifest { description: "a <b>".into(), ..m.clone() }.validate().is_err());
        assert!(Manifest { name: "claude-helper".into(), ..m.clone() }.validate().is_err());
        assert!(m.validate().is_ok());
    }

    #[test]
    fn skill_md_has_only_name_and_description() {
        let s = render_skill_md("cargo-tests-first", "Run cargo tests: first", "Body\n");
        assert!(s.starts_with("---\nname: cargo-tests-first\ndescription: \"Run cargo tests: first\"\n---\n"));
        let p = parse_skill_md(&s).unwrap();
        assert_eq!(
            (p.name.as_str(), p.description.as_str(), p.body.as_str()),
            ("cargo-tests-first", "Run cargo tests: first", "Body\n")
        );
        assert!(p.extra_keys.is_empty());
        let quoted = render_skill_md("a", "say \"hi\" \\ there", "b");
        assert_eq!(parse_skill_md(&quoted).unwrap().description, "say \"hi\" \\ there");
    }

    #[test]
    fn parse_imported_skill_md_reports_extra_keys() {
        let p = parse_skill_md("---\nname: x\ndescription: d\nallowed-tools: Bash\nmodel: o\n---\nb").unwrap();
        assert_eq!(p.extra_keys, ["allowed-tools", "model"]);
        let p = parse_skill_md("---\nname: x\ndescription: 'it''s'\nhooks:\n  - pre: rm\n---\nb").unwrap();
        assert_eq!((p.description.as_str(), p.extra_keys.clone()), ("it's", vec!["hooks".to_string()]));
        for bad in [
            "name: x\n",
            "---\nname: x\n",
            "---\ndescription: d\n---\n",
            "---\nname: x\nname: y\ndescription: d\n---\n",
        ] {
            assert!(parse_skill_md(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn names_follow_both_clis() {
        assert!(is_name("a-1") && !is_name("A") && !is_name("-a") && !is_name(&"a".repeat(65)));
        assert!(is_name(&"a".repeat(64)) && !is_name("my-claude-x") && !is_name("anthropic") && !is_name("a_b"));
    }

    #[test]
    fn file_paths_are_relative_text_and_bounded() {
        for bad in
            ["../x", "/x", "a/../b", "x.sh", ".hidden", "a/.b.md", "a//b.md", "SKILL.md", "skill.md", "a/b/c/d.md", "x"]
        {
            assert!(check_file_path(bad).is_err(), "{bad}");
        }
        for ok in ["reference/a.md", "notes.txt", "a/b/c.md"] {
            assert!(check_file_path(ok).is_ok(), "{ok}");
        }
    }
}
