//! `rules.layers`: every place an agent session takes instructions from, read from disk
//! (read-only), in the order the agent loads them. What each agent reads, with versions and
//! sources, is in `docs/agents/instruction-layers.md`; in short:
//!
//! - **Claude Code 2.1.285**: managed `CLAUDE.md` and `managed-settings.json`; the user's
//!   `~/.claude/CLAUDE.md`, `~/.claude/rules/**/*.md`, `settings.json`, skills, enabled
//!   plugins, output styles and auto memory (`~/.claude/projects/<project>/memory/MEMORY.md`,
//!   first 200 lines or 25 KB); `CLAUDE.md` / `CLAUDE.local.md` in every folder above the
//!   project; the project's `CLAUDE.md`, `.claude/CLAUDE.md`, `.claude/rules/`, `AGENTS.md`
//!   (only when no `CLAUDE.md` is found), `.claude/settings.json` and skills; the local
//!   `CLAUDE.local.md` and `.claude/settings.local.json`; `CLAUDE.md` files in subfolders
//!   on demand. `@path` imports are followed up to four hops, outside code spans and blocks.
//! - **Codex CLI 0.167**: `/etc/codex` (requirements, managed and system config, admin
//!   skills); `$CODEX_HOME/AGENTS.override.md` else `AGENTS.md`; `config.toml` (with
//!   `developer_instructions` and `model_instructions_file`); `~/.agents/skills` and the
//!   bundled system skills; per folder from the git root to the working folder, the first of
//!   `AGENTS.override.md`, `AGENTS.md` and `project_doc_fallback_filenames`, together capped
//!   at `project_doc_max_bytes` (32 KiB); the project's `.codex/config.toml` (trusted
//!   projects) and `.agents/skills`.
//!
//! Reading rules: a path is read only when it resolves (symlinks followed: dotfiles are
//! often links into a dotfiles repo) inside the home directory, the project, the managed
//! folders or Mirante's home; only regular files, opened without blocking (never a FIFO or a
//! device); at most [`FILE_CAP`] bytes each. Files that usually hold credentials (ssh, cloud
//! and agent auth files, `.env`, keys) are never shown, even when an instruction file
//! imports them. Everything shown goes through [`redact`]; settings show their relevant keys
//! with `env` values and secret-looking keys masked.
use std::collections::HashSet;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use mirante_core::redact::redact;
use mirante_ipc::types::{RuleImport, RuleLayer};
use serde_json::{Map, Value};

use crate::hub::files::may_hold_secrets;

/// Most bytes of one file shown.
pub const FILE_CAP: usize = 64 * 1024;
/// Claude Code follows `@path` imports this many hops deep.
pub const IMPORT_HOPS: usize = 4;
/// Most imports followed per layer (all hops together).
const IMPORTS_MAX: usize = 64;
/// Claude Code loads the first 200 lines or 25 KB of `MEMORY.md`.
pub const MEMORY_LINES: usize = 200;
pub const MEMORY_BYTES: usize = 25 * 1024;
/// Codex caps its project instructions (all `AGENTS.md` together) at this by default.
pub const CODEX_DOC_MAX: u64 = 32 * 1024;
/// Subfolder scan for nested instruction files: depth, folders and files at most.
const NESTED_DEPTH: usize = 8;
const NESTED_DIRS: usize = 5000;
const NESTED_FILES: usize = 50;
/// Most entries of a skills or rules folder listed.
const LIST_MAX: usize = 200;

/// The versions the loading rules were researched against.
pub const CLAUDE_RESEARCHED: &str = "Claude Code 2.1.285";
pub const CODEX_RESEARCHED: &str = "Codex CLI 0.167.0";

/// Where each agent's own configuration lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Homes {
    /// `$HOME`.
    pub home: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
    pub claude_dir: PathBuf,
    /// `$CODEX_HOME`, else `~/.codex`.
    pub codex_dir: PathBuf,
    /// macOS `/Library/Application Support/ClaudeCode`, Linux `/etc/claude-code`.
    pub claude_managed: PathBuf,
    /// `/etc/codex`.
    pub codex_managed: PathBuf,
    /// Mirante's home (per-task settings live under it).
    pub mirante_home: PathBuf,
}

impl Homes {
    /// From the daemon's environment; `None` without a `HOME`.
    pub fn from_env(mirante_home: &Path) -> Option<Homes> {
        let home = PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?);
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let claude_managed = if cfg!(target_os = "macos") {
            PathBuf::from("/Library/Application Support/ClaudeCode")
        } else {
            PathBuf::from("/etc/claude-code")
        };
        Some(Homes {
            claude_dir: var("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")),
            codex_dir: var("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            home,
            claude_managed,
            codex_managed: PathBuf::from("/etc/codex"),
            mirante_home: mirante_home.to_path_buf(),
        })
    }
}

/// One file as read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Read1 {
    pub exists: bool,
    pub size: Option<u64>,
    pub mtime: Option<i64>,
    /// Raw text (not redacted), capped.
    pub text: Option<String>,
    pub truncated: bool,
    pub error: Option<String>,
    /// Where it resolved.
    pub canon: Option<PathBuf>,
}

/// Reads files for one project: what may be read and how paths are shown.
pub struct Reader<'a> {
    pub homes: &'a Homes,
    allowed: Vec<PathBuf>,
    home_canon: PathBuf,
}

fn canon_or(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl<'a> Reader<'a> {
    /// `roots`: the project folders (checkout, worktree) whose files may be read besides the
    /// home directory, the managed folders and Mirante's home.
    pub fn new(homes: &'a Homes, roots: &[&Path]) -> Reader<'a> {
        let mut allowed: Vec<PathBuf> = [
            &homes.home,
            &homes.claude_dir,
            &homes.codex_dir,
            &homes.claude_managed,
            &homes.codex_managed,
            &homes.mirante_home,
        ]
        .iter()
        .map(|p| canon_or(p))
        .collect();
        allowed.extend(roots.iter().map(|p| canon_or(p)));
        Reader { homes, allowed, home_canon: canon_or(&homes.home) }
    }

    /// `p` for display: `~/…` under the home directory.
    pub fn show(&self, p: &Path) -> String {
        for h in [&self.homes.home, &self.home_canon] {
            if let Ok(rest) = p.strip_prefix(h) {
                return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
            }
        }
        p.display().to_string()
    }

    fn allowed(&self, canon: &Path) -> bool {
        self.allowed.iter().any(|a| canon.starts_with(a))
    }

    /// A path that usually holds credentials: never shown.
    fn secret(&self, canon: &Path) -> bool {
        let name = canon.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if may_hold_secrets(&canon.to_string_lossy())
            || [".netrc", ".npmrc", ".pypirc", ".git-credentials", ".credentials.json", ".claude.json", "auth.json"]
                .contains(&name.as_str())
            || name.ends_with(".p12")
            || name.ends_with(".pfx")
        {
            return true;
        }
        [".ssh", ".aws", ".gnupg", ".config/gh", ".docker", ".kube", ".azure", ".config/gcloud"]
            .iter()
            .any(|d| canon.starts_with(self.home_canon.join(d)))
    }

    /// Reads `p` under the rules in the module doc.
    pub fn read(&self, p: &Path, cap: usize) -> Read1 {
        let Ok(lmeta) = std::fs::symlink_metadata(p) else { return Read1::default() };
        let mut r = Read1 { exists: true, ..Default::default() };
        let canon = match std::fs::canonicalize(p) {
            Ok(c) => c,
            Err(e) => {
                r.error = Some(if lmeta.file_type().is_symlink() {
                    "a symlink to nothing".to_string()
                } else {
                    format!("cannot resolve: {e}")
                });
                return r;
            }
        };
        r.canon = Some(canon.clone());
        if !self.allowed(&canon) {
            r.error = Some(format!(
                "resolves to {}, outside your home folder, the project and the managed folders: not read",
                canon.display()
            ));
            return r;
        }
        if self.secret(&canon) {
            r.error = Some("may hold credentials: not shown".into());
            return r;
        }
        let f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW).open(&canon);
        let f = match f {
            Ok(f) => f,
            Err(e) => {
                r.error = Some(format!("cannot open: {e}"));
                return r;
            }
        };
        let meta = match f.metadata() {
            Ok(m) => m,
            Err(e) => {
                r.error = Some(format!("cannot stat: {e}"));
                return r;
            }
        };
        if !meta.is_file() {
            r.error =
                Some(if meta.is_dir() { "a folder, not a file".into() } else { "not a regular file: not read".into() });
            return r;
        }
        r.size = Some(meta.len());
        r.mtime =
            meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
        let mut buf = Vec::with_capacity(meta.len().min(cap as u64 + 1) as usize);
        if let Err(e) = f.take(cap as u64 + 1).read_to_end(&mut buf) {
            r.error = Some(format!("cannot read: {e}"));
            return r;
        }
        if buf.len() > cap {
            buf.truncate(cap);
            r.truncated = true;
        }
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        if r.truncated {
            // A cut multi-byte character reads as U+FFFD at the end; drop it.
            while text.ends_with('\u{FFFD}') {
                text.pop();
            }
        }
        r.text = Some(text);
        r
    }

    /// A folder inside the allowed places: its canonical path, or why not.
    fn dir(&self, p: &Path) -> Result<Option<PathBuf>, String> {
        let Ok(c) = std::fs::canonicalize(p) else { return Ok(None) };
        if !c.is_dir() {
            return Ok(None);
        }
        if !self.allowed(&c) {
            return Err(format!("resolves to {}, outside the allowed places: not read", c.display()));
        }
        Ok(Some(c))
    }
}

/// A layer for `path` from what [`Reader::read`] found; `content` is redacted.
fn layer(r: &Reader, scope: &str, agent: &str, kind: &str, path: &Path, read: &Read1, note: &str) -> RuleLayer {
    RuleLayer {
        scope: scope.into(),
        agent: agent.into(),
        kind: kind.into(),
        path: r.show(path),
        exists: read.exists,
        size: read.size,
        mtime: read.mtime,
        content: read.text.as_deref().map(redact),
        truncated: read.truncated,
        note: note.into(),
        loaded: read.exists && read.error.is_none(),
        error: read.error.clone(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------------------
// Claude `@path` imports

/// The `@path` imports of a Claude instruction file: outside fenced blocks and code spans,
/// an `@` at the start of a word followed by something path-like.
pub fn import_specs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(f) = fence {
            if trimmed.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        // Drop code spans: every other backtick-delimited run.
        let mut plain = String::new();
        for (i, part) in line.split('`').enumerate() {
            plain.push_str(if i % 2 == 0 { part } else { " " });
            plain.push(' ');
        }
        let chars: Vec<char> = plain.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c != '@' || (i > 0 && !chars[i - 1].is_whitespace() && chars[i - 1] != '(') {
                continue;
            }
            let word: String = chars[i + 1..].iter().take_while(|c| !c.is_whitespace()).collect();
            let word = word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']);
            let pathlike = word.starts_with('/')
                || word.starts_with("~/")
                || word.starts_with("./")
                || word.starts_with("../")
                || word.contains('.')
                || word.contains('/');
            if !word.is_empty() && !word.starts_with('@') && pathlike && !out.iter().any(|w| w == word) {
                out.push(word.to_string());
            }
        }
    }
    out
}

fn resolve_spec(r: &Reader, spec: &str, base: &Path) -> PathBuf {
    if let Some(rest) = spec.strip_prefix("~/") {
        return r.homes.home.join(rest);
    }
    let p = Path::new(spec);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// `p` without `.` and `..` (textually), for display.
fn tidy(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// The imports of `text` (a file in `base`), `hop` hops from the layer, each with its own.
/// `chain`: the files being expanded above (a cycle stops there); `budget`: imports left.
fn imports_of(
    r: &Reader,
    text: &str,
    base: &Path,
    hop: usize,
    chain: &mut Vec<PathBuf>,
    budget: &mut usize,
) -> Vec<RuleImport> {
    let mut out = Vec::new();
    for spec in import_specs(text) {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let path = tidy(&resolve_spec(r, &spec, base));
        let mut imp = RuleImport { spec: spec.clone(), path: r.show(&path), ..Default::default() };
        if hop > IMPORT_HOPS {
            imp.exists = path.exists();
            imp.error = Some(format!("more than {IMPORT_HOPS} hops deep: Claude Code does not load it"));
            out.push(imp);
            continue;
        }
        let read = r.read(&path, FILE_CAP);
        imp.exists = read.exists;
        imp.size = read.size;
        imp.truncated = read.truncated;
        imp.error = read.error.clone();
        if let (Some(canon), Some(text)) = (read.canon.clone(), read.text.as_deref()) {
            if chain.contains(&canon) {
                imp.error = Some("import cycle: this file is already being read above".into());
                imp.truncated = false;
            } else {
                imp.content = Some(redact(text));
                chain.push(canon.clone());
                let dir = canon.parent().map(Path::to_path_buf).unwrap_or_default();
                imp.imports = imports_of(r, text, &dir, hop + 1, chain, budget);
                chain.pop();
            }
        }
        out.push(imp);
    }
    out
}

/// A Claude instruction file as a layer, its imports followed.
fn claude_file(r: &Reader, scope: &str, path: &Path, note: &str) -> RuleLayer {
    let read = r.read(path, FILE_CAP);
    let mut l = layer(r, scope, "claude", "instructions", path, &read, note);
    if let (Some(canon), Some(text)) = (read.canon.as_ref(), read.text.as_deref()) {
        let mut chain = vec![canon.clone()];
        let mut budget = IMPORTS_MAX;
        // Relative imports resolve against the importing file's folder (as written).
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        l.imports = imports_of(r, text, &base, 1, &mut chain, &mut budget);
    }
    l
}

// ---------------------------------------------------------------------------------------
// Folders: rules, skills, nested files

/// `*.md` under `dir`, recursively (depth 4), sorted, at most [`LIST_MAX`].
fn md_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut seen = HashSet::new();
    while let Some((d, depth)) = stack.pop() {
        if !seen.insert(canon_or(&d)) || depth > 4 {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                stack.push((p, depth + 1));
            } else if p.extension().is_some_and(|e| e == "md") {
                out.push(p);
            }
            if out.len() >= LIST_MAX {
                return out;
            }
        }
    }
    out.sort();
    out
}

/// Each `.md` in a rules folder as a layer, or one "not present" layer for the folder.
fn rules_dir(r: &Reader, scope: &str, agent: &str, dir: &Path, note: &str) -> Vec<RuleLayer> {
    let shown = |exists: bool, error: Option<String>| RuleLayer {
        scope: scope.into(),
        agent: agent.into(),
        kind: "instructions".into(),
        path: format!("{}/", r.show(dir)),
        exists,
        note: note.into(),
        error,
        ..Default::default()
    };
    match r.dir(dir) {
        Err(e) => vec![shown(true, Some(e))],
        Ok(None) => vec![shown(false, None)],
        Ok(Some(_)) => {
            let files = md_files(dir);
            if files.is_empty() {
                return vec![shown(true, None)];
            }
            files.iter().map(|f| claude_file(r, scope, f, note)).collect()
        }
    }
}

/// `name` and `description` from a `SKILL.md`'s frontmatter.
pub fn skill_head(text: &str) -> (Option<String>, Option<String>) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let (mut name, mut desc) = (None, None);
    for l in lines {
        if l.trim() == "---" {
            break;
        }
        let unq = |v: &str| v.trim().trim_matches(['"', '\'']).to_string();
        if let Some(v) = l.strip_prefix("name:") {
            name = Some(unq(v));
        } else if let Some(v) = l.strip_prefix("description:") {
            desc = Some(unq(v));
        }
    }
    (name, desc)
}

/// A skills folder (`<name>/SKILL.md` each, also `.system/<name>/` for Codex's bundled
/// ones) as one layer listing `name: description`.
fn skills_dir(r: &Reader, scope: &str, agent: &str, dir: &Path, note: &str) -> RuleLayer {
    let mut l = RuleLayer {
        scope: scope.into(),
        agent: agent.into(),
        kind: "skills".into(),
        path: format!("{}/", r.show(dir)),
        note: note.into(),
        ..Default::default()
    };
    let canon = match r.dir(dir) {
        Err(e) => {
            l.exists = true;
            l.error = Some(e);
            return l;
        }
        Ok(None) => return l,
        Ok(Some(c)) => c,
    };
    l.exists = true;
    l.loaded = true;
    let mut lines = Vec::new();
    let mut bases = vec![(canon.clone(), String::new())];
    if canon.join(".system").is_dir() {
        bases.push((canon.join(".system"), "(system) ".to_string()));
    }
    for (base, tag) in bases {
        let Ok(rd) = std::fs::read_dir(&base) else { continue };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let fname = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if fname.starts_with('.') || lines.len() >= LIST_MAX {
                continue;
            }
            let md = p.join("SKILL.md");
            let read = r.read(&md, 8 * 1024);
            if !read.exists {
                continue;
            }
            let (name, desc) = read.text.as_deref().map(skill_head).unwrap_or((None, None));
            let name = name.unwrap_or(fname);
            let mut desc = desc.or(read.error).unwrap_or_default();
            if desc.chars().count() > 240 {
                desc = desc.chars().take(239).collect::<String>() + "…";
            }
            lines.push(format!("{tag}{name}: {desc}"));
        }
    }
    l.skills = lines.len();
    l.content = Some(redact(&lines.join("\n")));
    l
}

/// Instruction files named `names` in folders below `root` (not `root` itself), skipping
/// hidden folders, dependencies and build output; symlinked folders are not followed.
pub fn nested_files(root: &Path, names: &[&str]) -> Vec<PathBuf> {
    const SKIP: [&str; 8] = ["node_modules", "target", "dist", "build", "vendor", "venv", "__pycache__", "Pods"];
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut dirs = 0;
    while let Some((d, depth)) = queue.pop_front() {
        dirs += 1;
        if dirs > NESTED_DIRS {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_dir() && depth < NESTED_DEPTH && !name.starts_with('.') && !SKIP.contains(&name.as_str()) {
                queue.push_back((e.path(), depth + 1));
            } else if depth > 0 && (ft.is_file() || ft.is_symlink()) && names.contains(&name.as_str()) {
                out.push(e.path());
                if out.len() >= NESTED_FILES {
                    return out;
                }
            }
        }
    }
    out
}

/// The folders from `/` down to `root`'s parent.
fn ancestors_top_down(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = root.ancestors().skip(1).map(Path::to_path_buf).collect();
    v.reverse();
    v
}

// ---------------------------------------------------------------------------------------
// Settings

/// An env-like key whose value is a secret.
fn secret_key(k: &str) -> bool {
    let k = k.to_ascii_uppercase();
    [
        "TOKEN",
        "KEY",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASS",
        "AUTH",
        "CREDENTIAL",
        "COOKIE",
        "SESSION",
        "PRIVATE",
        "BEARER",
    ]
    .iter()
    .any(|s| k.contains(s))
}

const MASK: &str = "«redacted»";

/// Every string in `v` redacted; values under secret-looking keys masked.
fn mask_json(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(a) => Value::Array(a.iter().map(mask_json).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| {
                    let v = if secret_key(k) && !v.is_object() && !v.is_array() {
                        Value::String(MASK.into())
                    } else {
                        mask_json(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        v => v.clone(),
    }
}

/// The keys of a Claude settings file that shape what the agent is told or may do.
pub const CLAUDE_KEYS: [&str; 16] = [
    "permissions",
    "hooks",
    "env",
    "enabledPlugins",
    "outputStyle",
    "claudeMdExcludes",
    "autoMemoryEnabled",
    "autoMemoryDirectory",
    "model",
    "additionalDirectories",
    "apiKeyHelper",
    "disableAllHooks",
    "allowManagedHooksOnly",
    "allowManagedPermissionRulesOnly",
    "pluginConfigs",
    "sandbox",
];

/// A Claude settings file's relevant keys as pretty JSON (every `env` value masked, other
/// strings redacted), then the names of the other keys. `Err`: not a JSON object.
pub fn claude_settings_text(text: &str) -> Result<(String, Map<String, Value>), String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let Value::Object(o) = v else { return Err("not a JSON object".into()) };
    let mut shown = Map::new();
    for k in CLAUDE_KEYS {
        if let Some(v) = o.get(k) {
            let v = match (k, v) {
                ("env", Value::Object(env)) => {
                    Value::Object(env.keys().map(|name| (name.clone(), Value::String(MASK.into()))).collect())
                }
                (_, v) => mask_json(v),
            };
            shown.insert(k.to_string(), v);
        }
    }
    let mut out = serde_json::to_string_pretty(&Value::Object(shown)).unwrap_or_default();
    let others: Vec<&str> = o.keys().map(String::as_str).filter(|k| !CLAUDE_KEYS.contains(k)).collect();
    if !others.is_empty() {
        out.push_str(&format!("\n\nOther keys (values not shown): {}", others.join(", ")));
    }
    Ok((redact(&out), o))
}

fn settings_layer(r: &Reader, scope: &str, path: &Path, note: &str) -> (RuleLayer, Option<Map<String, Value>>) {
    let read = r.read(path, FILE_CAP);
    let mut l = layer(r, scope, "claude", "settings", path, &read, note);
    let mut parsed = None;
    if let Some(text) = read.text.as_deref() {
        match claude_settings_text(text) {
            Ok((t, o)) => {
                l.content = Some(t);
                parsed = Some(o);
            }
            Err(e) => {
                l.content = None;
                l.error = Some(e);
            }
        }
    }
    (l, parsed)
}

// ---------------------------------------------------------------------------------------
// Codex config.toml

/// One `key = value` of a TOML file, with the table it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TomlEntry {
    pub table: String,
    pub key: String,
    /// The value as written (multi-line strings and arrays joined).
    pub raw: String,
}

/// A small line-based TOML reader: enough to list keys and values (multi-line strings and
/// arrays included), never to validate.
pub fn toml_entries(text: &str) -> Vec<TomlEntry> {
    let mut out = Vec::new();
    let mut table = String::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') {
            table = t.trim_start_matches('[').split(']').next().unwrap_or("").trim().to_string();
            continue;
        }
        let Some((k, v)) = t.split_once('=') else { continue };
        let key = k.trim().trim_matches('"').to_string();
        let mut raw = v.trim().to_string();
        for delim in ["\"\"\"", "'''"] {
            if raw.starts_with(delim) && (raw.len() < 6 || !raw[3..].contains(delim)) {
                for more in lines.by_ref() {
                    raw.push('\n');
                    raw.push_str(more);
                    if more.contains(delim) {
                        break;
                    }
                }
            }
        }
        if raw.starts_with('[') {
            let depth = |s: &str| s.matches('[').count() as i64 - s.matches(']').count() as i64;
            while depth(&raw) > 0 {
                let Some(more) = lines.next() else { break };
                raw.push('\n');
                raw.push_str(more.trim());
            }
        }
        out.push(TomlEntry { table: table.clone(), key, raw });
    }
    out
}

/// A TOML string value's text (basic, literal, or multi-line), else the raw value.
pub fn toml_string(raw: &str) -> String {
    let r = raw.trim();
    for d in ["\"\"\"", "'''"] {
        if let Some(inner) = r.strip_prefix(d) {
            let inner = inner.rsplit_once(d).map_or(inner, |(a, _)| a);
            let inner = inner.strip_prefix('\n').unwrap_or(inner);
            return if d == "'''" { inner.to_string() } else { unescape(inner) };
        }
    }
    if let Some(inner) = r.strip_prefix('"') {
        let end = inner.rfind('"').unwrap_or(inner.len());
        return unescape(&inner[..end]);
    }
    if let Some(inner) = r.strip_prefix('\'') {
        return inner.rsplit_once('\'').map_or(inner, |(a, _)| a).to_string();
    }
    r.split('#').next().unwrap_or(r).trim().to_string()
}

fn unescape(s: &str) -> String {
    s.replace("\\n", "\n").replace("\\t", "\t").replace("\\\"", "\"").replace("\\\\", "\\")
}

/// A Codex `config.toml` summed up: every key with its value, values under env/header
/// tables or secret-looking keys masked, long values cut.
pub fn codex_config_text(entries: &[TomlEntry]) -> String {
    let mut out = String::new();
    let mut last: Option<&str> = None;
    for e in entries {
        if last != Some(e.table.as_str()) {
            if !e.table.is_empty() {
                out.push_str(&format!("{}[{}]\n", if out.is_empty() { "" } else { "\n" }, e.table));
            }
            last = Some(e.table.as_str());
        }
        let tl = e.table.to_ascii_lowercase();
        let masked = secret_key(&e.key)
            || tl.ends_with(".env")
            || tl.contains("headers")
            || tl.contains("shell_environment_policy.set");
        let value = if masked {
            MASK.to_string()
        } else if e.key == "developer_instructions" || e.key == "instructions" {
            format!("(text, {} characters: shown as its own layer)", toml_string(&e.raw).chars().count())
        } else if e.raw.chars().count() > 400 {
            e.raw.chars().take(399).collect::<String>() + "…"
        } else {
            e.raw.clone()
        };
        out.push_str(&format!("{} = {}\n", e.key, value));
    }
    redact(out.trim_end())
}

/// The value of `key` at the top level of `entries`.
fn top<'e>(entries: &'e [TomlEntry], key: &str) -> Option<&'e TomlEntry> {
    entries.iter().find(|e| e.table.is_empty() && e.key == key)
}

// ---------------------------------------------------------------------------------------
// The layers per agent

/// Where Claude Code keeps auto memory for a project: `~/.claude/projects/<key>/memory`,
/// `<key>` being the project path with every character other than a letter or digit
/// turned into `-`.
pub fn claude_memory_dir(claude_dir: &Path, project: &Path) -> PathBuf {
    let key: String =
        project.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    claude_dir.join("projects").join(key).join("memory")
}

/// The first 200 lines or 25 KB of `text`, and whether that cut anything.
pub fn memory_head(text: &str) -> (String, bool) {
    let mut out = String::new();
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i >= MEMORY_LINES || out.len() + line.len() > MEMORY_BYTES {
            return (out, true);
        }
        out.push_str(line);
    }
    (out, false)
}

fn expand_home(h: &Homes, s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => h.home.join(rest),
        None => PathBuf::from(s),
    }
}

/// Claude Code's own layers for a session started in `root`; `memory_project`: the main
/// checkout (auto memory is shared by a repository's worktrees).
pub fn claude_layers(r: &Reader, root: &Path, memory_project: &Path) -> Vec<RuleLayer> {
    let h = r.homes;
    let mut v = Vec::new();
    // Managed.
    v.push(claude_file(
        r,
        "managed",
        &h.claude_managed.join("CLAUDE.md"),
        "Organization policy, loaded first in every session; cannot be excluded.",
    ));
    let (l, managed) = settings_layer(
        r,
        "managed",
        &h.claude_managed.join("managed-settings.json"),
        "Managed settings: override every other settings file and --settings.",
    );
    v.push(l);
    if let Ok(Some(d)) = r.dir(&h.claude_managed.join("managed-settings.d")) {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&d)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        for f in files.into_iter().take(LIST_MAX) {
            v.push(settings_layer(r, "managed", &f, "Managed settings drop-in, merged into managed-settings.json.").0);
        }
    }
    // User.
    v.push(claude_file(
        r,
        "user",
        &h.claude_dir.join("CLAUDE.md"),
        "Your instructions for every project; loaded after managed, before the project's.",
    ));
    v.extend(rules_dir(
        r,
        "user",
        "claude",
        &h.claude_dir.join("rules"),
        "Your rules for every project (a `paths:` rule loads only with matching files); before project rules.",
    ));
    let (l, user) = settings_layer(
        r,
        "user",
        &h.claude_dir.join("settings.json"),
        "Your settings; project, local, --settings and managed win over them.",
    );
    v.push(l);
    v.push(skills_dir(
        r,
        "user",
        "claude",
        &h.claude_dir.join("skills"),
        "Your skills: names and descriptions at launch, the body when used.",
    ));
    // Project and local settings first: they decide plugins, output style and auto memory.
    let (proj_settings, project) = settings_layer(
        r,
        "project",
        &root.join(".claude/settings.json"),
        "Shared project settings: override yours; local, --settings and managed win.",
    );
    let (local_settings, local) = settings_layer(
        r,
        "local",
        &root.join(".claude/settings.local.json"),
        "Your settings for this project only: override the shared ones; --settings and managed win.",
    );
    let layers_settings = [&managed, &local, &project, &user];
    let pick = |k: &str| layers_settings.iter().find_map(|s| s.as_ref().and_then(|o| o.get(k)).cloned());
    // Enabled plugins (merged across files).
    let mut plugins: Vec<String> = Vec::new();
    for s in [&user, &project, &local, &managed].into_iter().flatten() {
        if let Some(Value::Object(p)) = s.get("enabledPlugins") {
            for (id, on) in p {
                if on.as_bool() == Some(true) && !plugins.contains(id) {
                    plugins.push(id.clone());
                }
            }
        }
    }
    let pdir = h.claude_dir.join("plugins");
    v.push(RuleLayer {
        scope: "user".into(),
        agent: "claude".into(),
        kind: "skills".into(),
        path: format!("{}/", r.show(&pdir)),
        exists: !plugins.is_empty() || pdir.is_dir(),
        loaded: !plugins.is_empty(),
        content: Some(redact(&plugins.join("\n"))),
        note: "Enabled plugins (enabledPlugins in settings): each can bring skills, commands, agents, hooks and MCP servers.".into(),
        ..Default::default()
    });
    let style = pick("outputStyle").and_then(|s| s.as_str().map(str::to_string));
    let mut styles = skills_dir(r, "user", "claude", &h.claude_dir.join("output-styles"), "");
    styles.kind = "instructions".into();
    styles.skills = 0;
    styles.loaded = false;
    styles.note = match &style {
        Some(s) => format!("Output styles; settings pick \"{s}\", which changes the system prompt."),
        None => "Output styles: used only when a settings file sets outputStyle.".into(),
    };
    if let Ok(Some(d)) = r.dir(&h.claude_dir.join("output-styles")) {
        let names: Vec<String> = std::fs::read_dir(&d)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".md"))
            .collect();
        styles.content = Some(redact(&names.join("\n")));
        styles.loaded = style.is_some();
    }
    v.push(styles);
    // Auto memory.
    let auto_on = pick("autoMemoryEnabled").and_then(|b| b.as_bool()).unwrap_or(true);
    let mem_dir = match pick("autoMemoryDirectory").and_then(|s| s.as_str().map(str::to_string)) {
        Some(d) => expand_home(h, &d),
        None => claude_memory_dir(&h.claude_dir, memory_project),
    };
    let mem = mem_dir.join("MEMORY.md");
    let read = r.read(&mem, FILE_CAP);
    let mut l = layer(r, "user", "claude", "memory", &mem, &read, "");
    if let Some(text) = read.text.as_deref() {
        let (head, cut) = memory_head(text);
        l.content = Some(redact(&head));
        l.truncated = cut || read.truncated;
    }
    l.loaded &= auto_on;
    l.note = if auto_on {
        "Auto memory Claude writes itself: the first 200 lines or 25 KB load in every session of this repository (all its worktrees); topic files beside it are read on demand.".into()
    } else {
        "Auto memory is turned off in settings (autoMemoryEnabled: false).".into()
    };
    v.push(l);
    // Folders above the project.
    let mut claude_md_found = false;
    for dir in ancestors_top_down(root) {
        for (name, scope) in [("CLAUDE.md", "project"), ("CLAUDE.local.md", "local")] {
            let p = dir.join(name);
            if std::fs::symlink_metadata(&p).is_ok() {
                claude_md_found = true;
                v.push(claude_file(
                    r,
                    scope,
                    &p,
                    "In a folder above the project: loaded at launch, before the project's own.",
                ));
            }
        }
    }
    // Project.
    let root_md = claude_file(
        r,
        "project",
        &root.join("CLAUDE.md"),
        "The project's instructions (usually committed); loaded after yours.",
    );
    let dot_md =
        claude_file(r, "project", &root.join(".claude/CLAUDE.md"), "The project's instructions, alternative location.");
    let local_path = root.join("CLAUDE.local.md");
    claude_md_found |= root_md.exists || dot_md.exists || std::fs::symlink_metadata(&local_path).is_ok();
    v.push(root_md);
    v.push(dot_md);
    v.extend(rules_dir(
        r,
        "project",
        "claude",
        &root.join(".claude/rules"),
        "Project rules (a `paths:` rule loads only when Claude touches matching files).",
    ));
    let agents_note = if claude_md_found {
        "Not read: Claude Code reads AGENTS.md only when there is no CLAUDE.md here or above (default since 2.1.277)."
    } else {
        "Read instead of CLAUDE.md, which this project does not have (since 2.1.277)."
    };
    for p in [root.join("AGENTS.md"), root.join(".claude/AGENTS.md")] {
        let mut l = claude_file(r, "project", &p, agents_note);
        l.loaded &= !claude_md_found;
        v.push(l);
    }
    v.push(proj_settings);
    v.push(skills_dir(
        r,
        "project",
        "claude",
        &root.join(".claude/skills"),
        "Project skills: names and descriptions at launch, the body when used.",
    ));
    // Local.
    v.push(claude_file(
        r,
        "local",
        &local_path,
        "Your private instructions for this project (not committed); read last at the project level.",
    ));
    v.push(local_settings);
    // Nested.
    for p in nested_files(root, &["CLAUDE.md", "CLAUDE.local.md"]) {
        let mut l =
            claude_file(r, "nested", &p, "In a subfolder: loaded on demand, once Claude reads or edits a file there.");
        l.loaded = false;
        v.push(l);
    }
    v
}

/// Codex's own layers for a session started in `root` (its git root).
pub fn codex_layers(r: &Reader, root: &Path) -> Vec<RuleLayer> {
    let h = r.homes;
    let mut v = Vec::new();
    let toml_layer = |scope: &str, path: &Path, note: &str| -> (RuleLayer, Vec<TomlEntry>) {
        let read = r.read(path, FILE_CAP);
        let mut l = layer(r, scope, "codex", "settings", path, &read, note);
        let entries = read.text.as_deref().map(toml_entries).unwrap_or_default();
        if read.text.is_some() {
            l.content = Some(codex_config_text(&entries));
        }
        (l, entries)
    };
    let plain = |scope: &str, kind: &str, path: &Path, note: &str| -> RuleLayer {
        let read = r.read(path, FILE_CAP);
        layer(r, scope, "codex", kind, path, &read, note)
    };
    // Managed.
    v.push(
        toml_layer(
            "managed",
            &h.codex_managed.join("requirements.toml"),
            "Admin requirements: limits no other config can lift (approval, sandbox, MCP servers).",
        )
        .0,
    );
    v.push(
        toml_layer(
            "managed",
            &h.codex_managed.join("managed_config.toml"),
            "Legacy managed config: overrides your config.toml.",
        )
        .0,
    );
    v.push(toml_layer("managed", &h.codex_managed.join("config.toml"), "System config: below yours.").0);
    v.push(skills_dir(
        r,
        "managed",
        "codex",
        &h.codex_managed.join("skills"),
        "Admin skills: names and descriptions at launch, the body when used.",
    ));
    // User instructions: the first non-empty of the two.
    let over = plain("user", "instructions", &h.codex_dir.join("AGENTS.override.md"), "");
    let base = plain("user", "instructions", &h.codex_dir.join("AGENTS.md"), "");
    let nonempty = |l: &RuleLayer| l.loaded && l.content.as_deref().is_some_and(|c| !c.trim().is_empty());
    let over_wins = nonempty(&over);
    let mut over = over;
    over.loaded = over_wins;
    over.note = "Your global instructions, used instead of AGENTS.md when present and not empty.".into();
    let mut base = base;
    base.loaded = !over_wins && nonempty(&base);
    base.note = if over_wins {
        "Not read: AGENTS.override.md wins.".into()
    } else {
        "Your global instructions for every project; read before the project's.".into()
    };
    v.push(over);
    v.push(base);
    let (cfg, entries) = toml_layer(
        "user",
        &h.codex_dir.join("config.toml"),
        "Your config: profiles and -c flags override it; managed config and requirements win.",
    );
    v.push(cfg);
    if let Some(e) = top(&entries, "developer_instructions") {
        let text = toml_string(&e.raw);
        v.push(RuleLayer {
            scope: "user".into(),
            agent: "codex".into(),
            kind: "instructions".into(),
            path: format!("{} · developer_instructions", r.show(&h.codex_dir.join("config.toml"))),
            exists: true,
            loaded: true,
            size: Some(text.len() as u64),
            content: Some(redact(&text)),
            note: "Added to every session as a developer message.".into(),
            ..Default::default()
        });
    }
    for key in ["model_instructions_file", "experimental_instructions_file"] {
        if let Some(e) = top(&entries, key) {
            let p = expand_home(h, &toml_string(&e.raw));
            let p = if p.is_absolute() { p } else { h.codex_dir.join(p) };
            let mut l = plain("user", "instructions", &p, "");
            l.note = if key == "model_instructions_file" {
                "Replaces Codex's built-in base instructions (model_instructions_file).".into()
            } else {
                l.loaded = false;
                "experimental_instructions_file: renamed model_instructions_file; 0.167 does not read this name.".into()
            };
            v.push(l);
        }
    }
    v.push(skills_dir(
        r,
        "user",
        "codex",
        &h.home.join(".agents/skills"),
        "Your skills: names and descriptions at launch (up to 2% of context), the body when used.",
    ));
    v.push(skills_dir(
        r,
        "user",
        "codex",
        &h.codex_dir.join("skills"),
        "Codex's skills folder, with the bundled system skills under .system.",
    ));
    // Project: AGENTS.md per folder from the git root down to where Codex starts (here the
    // root itself), the first of override, AGENTS.md and the fallback names.
    let max =
        top(&entries, "project_doc_max_bytes").and_then(|e| e.raw.trim().parse::<u64>().ok()).unwrap_or(CODEX_DOC_MAX);
    let mut names = vec!["AGENTS.override.md".to_string(), "AGENTS.md".to_string()];
    if let Some(e) = top(&entries, "project_doc_fallback_filenames") {
        names
            .extend(e.raw.trim_matches(['[', ']']).split(',').map(|s| toml_string(s.trim())).filter(|s| !s.is_empty()));
    }
    let mut picked = false;
    let mut used: u64 = 0;
    for n in &names {
        let mut l = plain("project", "instructions", &root.join(n), "");
        let take = !picked && nonempty(&l);
        l.loaded = take;
        l.note = if take {
            picked = true;
            used += l.size.unwrap_or(0);
            if used > max {
                format!("The project's instructions; Codex keeps only the first {max} bytes of all of them (project_doc_max_bytes).")
            } else {
                "The project's instructions, read after your global ones (one file per folder).".into()
            }
        } else if picked && l.exists {
            "Not read: an earlier name in this folder wins (one file per folder).".into()
        } else {
            "Where the project's instructions could live (one file per folder: override, AGENTS.md, fallbacks).".into()
        };
        // Fallback names that do not exist are noise.
        if l.exists || n == "AGENTS.md" || n == "AGENTS.override.md" {
            v.push(l);
        }
    }
    v.push(
        toml_layer(
            "project",
            &root.join(".codex/config.toml"),
            "Project config: read only when you trusted this project in Codex.",
        )
        .0,
    );
    v.push(skills_dir(
        r,
        "project",
        "codex",
        &root.join(".agents/skills"),
        "Project skills (every .agents/skills from the working folder up to the git root).",
    ));
    // Nested.
    for p in nested_files(root, &["AGENTS.md", "AGENTS.override.md"]) {
        let mut l = plain(
            "nested",
            "instructions",
            &p,
            "Below the folder Codex starts in: not loaded by Codex; the agent reads it only if it opens it.",
        );
        l.loaded = false;
        v.push(l);
    }
    v
}

/// The counts for the summary line: files read at launch (imports included), their bytes,
/// settings files, skills.
pub fn summarize(layers: &[RuleLayer]) -> mirante_ipc::types::RulesSummary {
    fn imports(i: &[RuleImport], files: &mut usize, bytes: &mut u64) {
        for x in i {
            if x.content.is_some() {
                *files += 1;
                *bytes += x.size.unwrap_or(0);
                imports(&x.imports, files, bytes);
            }
        }
    }
    let mut s = mirante_ipc::types::RulesSummary::default();
    for l in layers.iter().filter(|l| l.loaded) {
        match l.kind.as_str() {
            // Mirante's own layers are flags and messages, not files the agent reads.
            "instructions" | "memory" if l.scope != "mirante" => {
                if l.content.is_some() {
                    s.instruction_files += 1;
                    s.instruction_bytes += l.size.unwrap_or(0);
                }
                imports(&l.imports, &mut s.instruction_files, &mut s.instruction_bytes);
            }
            "settings" if l.exists => s.settings_files += 1,
            "skills" => s.skills += l.skills,
            _ => {}
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;

    struct Fake {
        _d: TestDir,
        homes: Homes,
        repo: PathBuf,
        outside: PathBuf,
    }

    fn fake() -> Fake {
        let d = TestDir::new();
        let base = std::fs::canonicalize(d.path()).unwrap();
        let home = base.join("home");
        let repo = home.join("src/app");
        let outside = base.join("outside");
        for p in [&home, &repo, &outside] {
            std::fs::create_dir_all(p).unwrap();
        }
        let homes = Homes {
            claude_dir: home.join(".claude"),
            codex_dir: home.join(".codex"),
            claude_managed: base.join("managed-claude"),
            codex_managed: base.join("managed-codex"),
            mirante_home: home.join(".mirante"),
            home,
        };
        Fake { _d: d, homes, repo, outside }
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    fn find<'l>(ls: &'l [RuleLayer], path_end: &str) -> &'l RuleLayer {
        ls.iter()
            .find(|l| l.path.ends_with(path_end))
            .unwrap_or_else(|| panic!("no layer {path_end}: {:?}", ls.iter().map(|l| &l.path).collect::<Vec<_>>()))
    }

    #[test]
    fn import_specs_skip_code_and_emails() {
        let t = "See @docs/a.md and @~/x.md.\nmail me@x.com\n`@not/this.md`\n```\n@nor/this.md\n```\n(@./paren.md) @@twice @README";
        assert_eq!(import_specs(t), ["docs/a.md", "~/x.md", "./paren.md"]);
    }

    #[test]
    fn claude_layers_follow_imports_stop_cycles_and_refuse_outside_home() {
        let f = fake();
        let cd = &f.homes.claude_dir;
        write(&cd.join("CLAUDE.md"), "Global rules.\n@a.md\n@~/notes/b.md\n");
        write(&cd.join("a.md"), "A imports back: @CLAUDE.md and @a.md\n");
        write(&f.homes.home.join("notes/b.md"), "B then @c.md\n");
        write(&f.homes.home.join("notes/c.md"), "C then @d.md\n");
        write(&f.homes.home.join("notes/d.md"), "D then @e.md\n");
        write(&f.homes.home.join("notes/e.md"), "E then @f.md\n");
        write(&f.homes.home.join("notes/f.md"), "F\n");
        write(&f.outside.join("secret.md"), "outside");
        write(
            &f.repo.join("CLAUDE.md"),
            &format!("Project.\n@{}\n@~/.ssh/id_rsa\n", f.outside.join("secret.md").display()),
        );
        write(&f.homes.home.join(".ssh/id_rsa"), "-----BEGIN OPENSSH PRIVATE KEY-----");
        let r = Reader::new(&f.homes, &[&f.repo]);
        let ls = claude_layers(&r, &f.repo, &f.repo);
        let user = find(&ls, "~/.claude/CLAUDE.md");
        assert!(user.loaded && user.content.as_deref() == Some("Global rules.\n@a.md\n@~/notes/b.md\n"));
        let a = &user.imports[0];
        assert_eq!((a.spec.as_str(), a.path.as_str()), ("a.md", "~/.claude/a.md"));
        // a.md imports CLAUDE.md (the layer itself) and a.md (itself): both cycles.
        assert_eq!(a.imports.len(), 2);
        assert!(
            a.imports.iter().all(|i| i.error.as_deref().is_some_and(|e| e.contains("cycle")) && i.content.is_none()),
            "{a:?}"
        );
        // b (hop 1) → c (2) → d (3) → e (4) → f (5: not loaded).
        let b = &user.imports[1];
        let e = &b.imports[0].imports[0].imports[0];
        assert_eq!(e.path, "~/notes/e.md");
        assert!(e.content.is_some());
        let f5 = &e.imports[0];
        assert!(f5.content.is_none() && f5.error.as_deref().unwrap().contains("4 hops"), "{f5:?}");
        let proj = find(&ls, "src/app/CLAUDE.md");
        assert!(proj.imports[0].error.as_deref().unwrap().contains("outside your home"), "{:?}", proj.imports);
        assert!(proj.imports[0].content.is_none());
        assert_eq!(proj.imports[1].error.as_deref(), Some("may hold credentials: not shown"));
        // Missing places are listed, greyed.
        let managed = find(&ls, "managed-claude/CLAUDE.md");
        assert!(!managed.exists && !managed.loaded);
        assert!(!find(&ls, "src/app/CLAUDE.local.md").exists);
        assert!(!find(&ls, "~/.claude/rules/").exists);
    }

    #[test]
    fn a_symlink_outside_home_is_refused_and_one_inside_is_followed() {
        let f = fake();
        write(&f.outside.join("rules.md"), "evil");
        write(&f.homes.home.join("dotfiles/claude.md"), "from dotfiles");
        std::fs::create_dir_all(&f.homes.claude_dir).unwrap();
        std::os::unix::fs::symlink(f.homes.home.join("dotfiles/claude.md"), f.homes.claude_dir.join("CLAUDE.md"))
            .unwrap();
        std::fs::create_dir_all(f.repo.join(".claude")).unwrap();
        std::os::unix::fs::symlink(f.outside.join("rules.md"), f.repo.join("CLAUDE.md")).unwrap();
        let r = Reader::new(&f.homes, &[&f.repo]);
        let ls = claude_layers(&r, &f.repo, &f.repo);
        assert_eq!(find(&ls, "~/.claude/CLAUDE.md").content.as_deref(), Some("from dotfiles"));
        let p = find(&ls, "src/app/CLAUDE.md");
        assert!(p.exists && !p.loaded && p.content.is_none(), "{p:?}");
        assert!(p.error.as_deref().unwrap().contains("outside"), "{p:?}");
    }

    #[test]
    fn fifos_are_never_opened_and_files_are_capped() {
        let f = fake();
        std::fs::create_dir_all(&f.homes.claude_dir).unwrap();
        let fifo = f.homes.claude_dir.join("CLAUDE.md");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        write(&f.repo.join("CLAUDE.md"), &"é".repeat(FILE_CAP));
        let r = Reader::new(&f.homes, &[&f.repo]);
        let ls = claude_layers(&r, &f.repo, &f.repo);
        let l = find(&ls, "~/.claude/CLAUDE.md");
        assert!(l.error.as_deref().unwrap().contains("not a regular file"), "{l:?}");
        let big = find(&ls, "src/app/CLAUDE.md");
        assert!(big.truncated && big.size == Some(2 * FILE_CAP as u64));
        assert!(
            big.content.as_ref().unwrap().len() <= FILE_CAP && !big.content.as_ref().unwrap().ends_with('\u{FFFD}')
        );
    }

    #[test]
    fn settings_show_relevant_keys_with_env_and_secrets_masked() {
        let f = fake();
        write(
            &f.homes.claude_dir.join("settings.json"),
            r#"{"permissions":{"allow":["Bash(ls)"],"deny":["Read(.env)"]},"env":{"GITHUB_TOKEN":"ghp_abcdefghijklmnopqrstuvwxyz0123456789","DEBUG":"1"},
               "hooks":{"Stop":[{"hooks":[{"type":"command","command":"curl -H 'Authorization: Bearer sk-ant-api03-abcdefghijklmnop' x"}]}]},
               "apiKeyHelper":"echo sk-ant-api03-zzzzzzzzzzzzzzzzzzzz","theme":"dark","enabledPlugins":{"p@m":true,"q@m":false}}"#,
        );
        let r = Reader::new(&f.homes, &[&f.repo]);
        let ls = claude_layers(&r, &f.repo, &f.repo);
        let s = find(&ls, "~/.claude/settings.json");
        let c = s.content.as_deref().unwrap();
        assert!(c.contains("Bash(ls)") && c.contains("GITHUB_TOKEN") && c.contains("DEBUG"), "{c}");
        for leak in ["ghp_abc", "sk-ant-api03", "\"1\""] {
            assert!(!c.contains(leak), "{leak} leaked: {c}");
        }
        assert!(c.contains("Other keys (values not shown): theme"), "{c}");
        let plugins = find(&ls, "~/.claude/plugins/");
        assert_eq!(plugins.content.as_deref(), Some("p@m"));
        let summary = summarize(&ls);
        assert_eq!(summary.settings_files, 1);
    }

    #[test]
    fn agents_md_counts_for_claude_only_without_claude_md_and_memory_is_capped() {
        let f = fake();
        write(&f.repo.join("AGENTS.md"), "agents");
        let mem = claude_memory_dir(&f.homes.claude_dir, &f.repo).join("MEMORY.md");
        write(&mem, &"line\n".repeat(300));
        write(&f.repo.join("sub/deep/CLAUDE.md"), "nested");
        write(&f.repo.join("node_modules/x/CLAUDE.md"), "skip");
        let r = Reader::new(&f.homes, &[&f.repo]);
        let ls = claude_layers(&r, &f.repo, &f.repo);
        assert!(find(&ls, "src/app/AGENTS.md").loaded);
        let m = find(&ls, "memory/MEMORY.md");
        assert!(m.loaded && m.truncated && m.content.as_ref().unwrap().lines().count() == 200, "{m:?}");
        let nested: Vec<_> = ls.iter().filter(|l| l.scope == "nested").map(|l| l.path.clone()).collect();
        assert_eq!(nested.len(), 1);
        assert!(nested[0].ends_with("sub/deep/CLAUDE.md") && !ls.iter().any(|l| l.scope == "nested" && l.loaded));
        write(&f.repo.join("CLAUDE.md"), "c");
        let ls = claude_layers(&r, &f.repo, &f.repo);
        assert!(!find(&ls, "src/app/AGENTS.md").loaded);
        assert!(claude_memory_dir(Path::new("/h/.claude"), Path::new("/Users/me/my.app"))
            .ends_with("projects/-Users-me-my-app/memory"));
    }

    #[test]
    fn codex_layers_pick_the_override_read_config_and_skills() {
        let f = fake();
        let cx = &f.homes.codex_dir;
        write(&cx.join("AGENTS.md"), "global");
        write(&cx.join("AGENTS.override.md"), "override");
        write(
            &cx.join("config.toml"),
            "model = \"gpt-5\"\napproval_policy = \"on-request\"\ndeveloper_instructions = \"\"\"\nAlways test.\nTwice.\n\"\"\"\nmodel_instructions_file = \"base.md\"\nproject_doc_fallback_filenames = [\"TEAM.md\"]\n[mcp_servers.x.env]\nAPI_TOKEN = \"secret-value\"\n[profiles.fast]\nmodel = \"o4\"\n",
        );
        write(&cx.join("base.md"), "base");
        write(&cx.join("skills/.system/imagegen/SKILL.md"), "---\nname: imagegen\ndescription: Make images\n---\nbody");
        write(
            &f.homes.home.join(".agents/skills/find/SKILL.md"),
            "---\nname: find\ndescription: \"Find skills\"\n---\n",
        );
        write(&f.repo.join("AGENTS.md"), "project");
        write(&f.repo.join("TEAM.md"), "team");
        write(&f.repo.join("pkg/AGENTS.md"), "nested");
        let r = Reader::new(&f.homes, &[&f.repo]);
        let ls = codex_layers(&r, &f.repo);
        assert!(find(&ls, "~/.codex/AGENTS.override.md").loaded);
        assert!(!find(&ls, "~/.codex/AGENTS.md").loaded);
        let cfg = find(&ls, "~/.codex/config.toml").content.clone().unwrap();
        assert!(cfg.contains("approval_policy = \"on-request\"") && cfg.contains("[profiles.fast]"), "{cfg}");
        assert!(!cfg.contains("secret-value"), "{cfg}");
        assert_eq!(find(&ls, "developer_instructions").content.as_deref(), Some("Always test.\nTwice.\n"));
        assert_eq!(find(&ls, "~/.codex/base.md").content.as_deref(), Some("base"));
        assert_eq!(find(&ls, "~/.codex/skills/").content.as_deref(), Some("(system) imagegen: Make images"));
        assert_eq!(find(&ls, "~/.agents/skills/").skills, 1);
        assert!(find(&ls, "src/app/AGENTS.md").loaded);
        let team = find(&ls, "src/app/TEAM.md");
        assert!(team.exists && !team.loaded);
        assert!(!find(&ls, "src/app/pkg/AGENTS.md").loaded);
        let s = summarize(&ls);
        assert_eq!((s.skills, s.settings_files), (2, 1));
        // override, developer_instructions, base.md, project AGENTS.md
        assert_eq!(s.instruction_files, 4, "{s:?}");
    }

    #[test]
    fn toml_reader_handles_multiline_values() {
        let e = toml_entries("a = [\n 'x',\n 'y',\n]\nb = '''\nraw\n'''\n# c = 1\n[t]\nd = \"q\\\"z\" # note\n");
        assert_eq!(e.len(), 3);
        assert_eq!(toml_string(&e[1].raw), "raw\n");
        assert_eq!(toml_string(&e[2].raw), "q\"z");
        assert_eq!(e[2].table, "t");
    }
}
