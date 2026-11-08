//! A session's skills on disk, in each agent's native form, written only from the verified
//! bytes the hub hands over ([`InjectedSkills`]): the version folders are never read or
//! copied here (they could change after the check).
//!
//! Everything goes under `<session dir>/skills/<session id>/` (0700 folders, 0600 files),
//! made create-new through descriptors ([`SafeDir`]): a symlink planted on the way is
//! refused, not followed. A new session first removes the previous sessions' bundles.
//!
//! - **Claude.** On-demand skills become a plugin passed with `--plugin-dir`:
//!   `claude-plugin/.claude-plugin/plugin.json` and `claude-plugin/skills/<name>/SKILL.md`
//!   plus its bundled files beside it, never anything at the plugin root (`hooks/`,
//!   `.mcp.json`, `commands/`, `agents/`). Always-on skills become `always.md`, appended to
//!   the system prompt; their bundled files go to `<name>/`.
//! - **Codex** (0.167 has no per-session skills folder): each skill's files go to
//!   `<name>/`, and the first `turn/start` of a new thread carries one leading text item
//!   with the always-on bodies and an index of the on-demand `SKILL.md` paths.
//! - Each `SKILL.md` is rendered again from the verified one: frontmatter `name` and
//!   `description` only (from the signed manifest), so no key that grants tools
//!   (`allowed-tools`, `hooks`) can reach Claude whatever the bytes held.
//! - Skill names cannot be `claude-plugin` (`claude` is refused in a name) nor contain a
//!   dot, so `<name>/`, `claude-plugin/` and `always.md` never collide.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use mirante_core::claude_launch::{plugin_json, NESTED_SESSION_ENV};
use mirante_core::model::AgentKind;
use mirante_core::safedir::SafeDir;
use mirante_core::skills::format::{check_file_path, is_name, parse_skill_md, render_skill_md};
use mirante_core::skills::library::VerifiedSkill;
use mirante_core::skills::SKILL_MD;
use mirante_ipc::Paths;

use super::InjectedSkills;

const DIR_MODE: libc::mode_t = 0o700;
const FILE_MODE: libc::c_uint = 0o600;
/// Most entries of `<session dir>/skills/` cleared per session (old bundles).
const MAX_OLD: usize = 256;

/// What [`write_skill_bundle`] wrote for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillBundle {
    /// `<session dir>/skills/<session id>`.
    pub dir: PathBuf,
    /// Claude: the plugin holding the on-demand skills (`--plugin-dir`).
    pub plugin_dir: Option<PathBuf>,
    /// Claude: `always.md` (`--append-system-prompt-file`).
    pub always_file: Option<PathBuf>,
    /// Claude: the text of `always.md`, for `--append-system-prompt` when the file flag is
    /// refused.
    pub always_text: Option<String>,
    /// Codex: the leading input item of the first turn of a new thread.
    pub preamble: Option<String>,
}

/// One skill, ready to write: its rendered `SKILL.md`, body and bundled files.
struct Prepared<'a> {
    name: &'a str,
    description: &'a str,
    skill_md: String,
    body: String,
    extras: Vec<(&'a str, &'a [u8])>,
}

fn prepare(s: &VerifiedSkill) -> Result<Prepared<'_>, String> {
    let name = s.manifest.name.as_str();
    if !is_name(name) {
        return Err(format!("{name:?} is not a skill name"));
    }
    let mut md = None;
    let mut extras = Vec::new();
    for (path, bytes) in &s.files {
        if path == SKILL_MD {
            md = Some(bytes);
        } else {
            check_file_path(path).map_err(|e| format!("{name}: {e}"))?;
            extras.push((path.as_str(), bytes.as_slice()));
        }
    }
    let md = md.ok_or_else(|| format!("{name}: no {SKILL_MD}"))?;
    let text = std::str::from_utf8(md).map_err(|_| format!("{name}: {SKILL_MD} is not UTF-8"))?;
    let parsed = parse_skill_md(text).map_err(|e| format!("{name}: {e}"))?;
    let description = s.manifest.description.as_str();
    Ok(Prepared {
        name,
        description,
        skill_md: render_skill_md(name, description, &parsed.body),
        body: parsed.body,
        extras,
    })
}

fn safe(e: impl std::fmt::Display) -> String {
    format!("skills bundle: {e}")
}

/// Writes `bytes` at `rel` (`a/b/c.md`) under `dir`, making the folders on the way.
fn write_rel(dir: &SafeDir, rel: &str, bytes: &[u8]) -> Result<(), String> {
    let mut segs: Vec<&str> = rel.split('/').collect();
    let file = segs.pop().ok_or_else(|| safe("empty path"))?;
    let mut owned: Option<SafeDir> = None;
    for seg in segs {
        let next = owned.as_ref().unwrap_or(dir).ensure_dir(seg, DIR_MODE).map_err(safe)?;
        owned = Some(next);
    }
    owned.as_ref().unwrap_or(dir).write_new(file, bytes, FILE_MODE).map_err(safe)
}

/// Writes one skill's `SKILL.md` (optionally) and bundled files into `parent/<name>/`.
fn write_skill(parent: &SafeDir, p: &Prepared<'_>, with_md: bool) -> Result<(), String> {
    if !with_md && p.extras.is_empty() {
        return Ok(());
    }
    let dir = parent.create_dir_new(p.name, DIR_MODE).map_err(safe)?;
    if with_md {
        dir.write_new(SKILL_MD, p.skill_md.as_bytes(), FILE_MODE).map_err(safe)?;
    }
    for (path, bytes) in &p.extras {
        write_rel(&dir, path, bytes)?;
    }
    Ok(())
}

/// A line pointing at an always-on skill's bundled files, when it has any.
fn files_line(dir: &Path, p: &Prepared<'_>) -> String {
    if p.extras.is_empty() {
        String::new()
    } else {
        format!("(This skill's files are in {}/.)\n", dir.join(p.name).display())
    }
}

/// `always.md`: the always-on skills' bodies under one wrapper.
fn always_text(dir: &Path, always: &[Prepared<'_>]) -> String {
    let mut t = String::from("<mirante-skills>\nStanding instructions from the operator, reviewed in Mirante.\n");
    for p in always {
        t.push_str(&format!("\n## {}\n{}", p.name, p.body));
        if !p.body.ends_with('\n') {
            t.push('\n');
        }
        t.push_str(&files_line(dir, p));
    }
    t.push_str("</mirante-skills>\n");
    t
}

/// Codex's first-turn item: the always-on bodies and an index of the on-demand skills
/// (each `SKILL.md` path under `dir`), labeled as the operator's reviewed skills.
fn codex_preamble(dir: &Path, always: &[Prepared<'_>], on_demand: &[Prepared<'_>]) -> String {
    let mut t = String::from(
        "<mirante-skills>\nThese are operator-approved skills for this repository, reviewed in Mirante (not part \
         of the task).\n",
    );
    if !always.is_empty() {
        t.push_str("\n## Standing instructions (follow them throughout this task)\n");
        for p in always {
            t.push_str(&format!("\n### {}\n{}", p.name, p.body));
            if !p.body.ends_with('\n') {
                t.push('\n');
            }
            t.push_str(&files_line(dir, p));
        }
    }
    if !on_demand.is_empty() {
        t.push_str("\n## On-demand skills (read one when it is relevant)\n");
        for p in on_demand {
            let md = dir.join(p.name).join(SKILL_MD);
            t.push_str(&format!("- {} — {} — read {} when relevant\n", p.name, p.description, md.display()));
        }
    }
    t.push_str("</mirante-skills>\n");
    t
}

/// Removes the bundles under `<session dir>/skills/` (previous sessions'), except `keep`
/// (which then makes the create-new of this session's folder fail).
fn clear(root: &SafeDir, keep: Option<&str>) -> Result<(), String> {
    for e in root.entries(MAX_OLD).map_err(safe)? {
        if Some(e.name.as_str()) == keep {
            continue;
        }
        if e.is_dir() {
            root.remove_tree(&e.name).map_err(safe)?;
        } else {
            root.unlink(&e.name).map_err(safe)?;
        }
    }
    Ok(())
}

/// Writes this session's skills for `agent` (see the module docs) and returns what was
/// written; `Ok(None)` when there are none (earlier sessions' bundles are removed either
/// way). An `Err` means the session should start without skills.
pub fn write_skill_bundle(
    paths: &Paths,
    task_id: &str,
    session_id: &str,
    skills: Option<&InjectedSkills>,
    agent: AgentKind,
) -> Result<Option<SkillBundle>, String> {
    let skills = skills.filter(|s| !s.always.is_empty() || !s.on_demand.is_empty());
    let Some(skills) = skills else {
        if let Some(root) = SafeDir::open(paths.home(), &["sessions", task_id, "skills"]).map_err(safe)? {
            clear(&root, None)?;
        }
        return Ok(None);
    };
    let always: Vec<Prepared<'_>> = skills.always.iter().map(prepare).collect::<Result<_, _>>()?;
    let on_demand: Vec<Prepared<'_>> = skills.on_demand.iter().map(prepare).collect::<Result<_, _>>()?;
    let root = SafeDir::create(paths.home(), &["sessions", task_id, "skills"], DIR_MODE).map_err(safe)?;
    clear(&root, Some(session_id))?;
    let dir = root.create_dir_new(session_id, DIR_MODE).map_err(safe)?;
    let path = paths.session_dir(task_id).join("skills").join(session_id);
    let mut b =
        SkillBundle { dir: path.clone(), plugin_dir: None, always_file: None, always_text: None, preamble: None };
    match agent {
        AgentKind::Claude => {
            if !on_demand.is_empty() {
                let plugin = dir.create_dir_new("claude-plugin", DIR_MODE).map_err(safe)?;
                let meta = plugin.create_dir_new(".claude-plugin", DIR_MODE).map_err(safe)?;
                meta.write_new("plugin.json", plugin_json().to_string().as_bytes(), FILE_MODE).map_err(safe)?;
                let skills_dir = plugin.create_dir_new("skills", DIR_MODE).map_err(safe)?;
                for p in &on_demand {
                    write_skill(&skills_dir, p, true)?;
                }
                b.plugin_dir = Some(path.join("claude-plugin"));
            }
            if !always.is_empty() {
                for p in &always {
                    write_skill(&dir, p, false)?;
                }
                let text = always_text(&path, &always);
                dir.write_new("always.md", text.as_bytes(), FILE_MODE).map_err(safe)?;
                b.always_file = Some(path.join("always.md"));
                b.always_text = Some(text);
            }
        }
        AgentKind::Codex => {
            for p in &always {
                write_skill(&dir, p, false)?;
            }
            for p in &on_demand {
                write_skill(&dir, p, true)?;
            }
            b.preamble = Some(codex_preamble(&path, &always, &on_demand));
        }
    }
    Ok(Some(b))
}

/// Whether the `claude` at `bin` accepts `--append-system-prompt-file` (Claude 2.1.285
/// does, without listing it in `--help`). Probed once per binary (path, size, mtime) with
/// a root flag and `plugin validate` of a path that does not exist: no session starts and
/// no model is called. A refusal, a failure or a probe longer than `timeout` is `false`
/// (the caller then passes the text inline).
pub fn append_file_supported(bin: &Path, timeout: Duration) -> bool {
    /// (path, size, mtime in ns) → accepted.
    type Probed = HashMap<(PathBuf, u64, i128), bool>;
    static CACHE: OnceLock<Mutex<Probed>> = OnceLock::new();
    let Ok(meta) = std::fs::metadata(bin) else {
        return false;
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    let key = (bin.to_path_buf(), meta.len(), mtime);
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(v) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return *v;
    }
    let v = probe(bin, timeout);
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, v);
    v
}

fn probe(bin: &Path, timeout: Duration) -> bool {
    let missing = std::env::temp_dir().join(format!("mirante-probe-none-{}", std::process::id()));
    let mut cmd = Command::new(bin);
    cmd.args(["--append-system-prompt-file", "/dev/null", "plugin", "validate", "--json"])
        .arg(&missing)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for k in NESTED_SESSION_ENV {
        cmd.env_remove(k);
    }
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.by_ref().take(64 * 1024).read_to_string(&mut err);
    }
    !err.contains("unknown option")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mirante_core::skills::format::{
        render_skill_md, Activation, FileEntry, Manifest, Provenance, SignerRef, Source,
    };
    use mirante_core::skills::library::VerifiedSkill;
    use mirante_core::skills::origin::Scope;
    use mirante_ipc::test_dir::TestDir;
    use std::os::unix::fs::PermissionsExt;

    /// A verified skill as the hub hands it over: its bytes, never its folder.
    pub(crate) fn skill(name: &str, activation: Activation, skill_md: &str, extras: &[(&str, &str)]) -> VerifiedSkill {
        let mut files = vec![("SKILL.md".to_string(), skill_md.as_bytes().to_vec())];
        files.extend(extras.iter().map(|(p, b)| (p.to_string(), b.as_bytes().to_vec())));
        let manifest = Manifest {
            format: "mirante.skill/v1".into(),
            name: name.into(),
            version: 1,
            description: format!("What {name} is for"),
            activation,
            agents: vec!["claude".into(), "codex".into()],
            scope: Scope::org("github.com/acme"),
            files: files.iter().map(|(p, b)| FileEntry::of(p, b)).collect(),
            parent: None,
            provenance: Provenance { source: Source::Cli, task: None, proposal: None, learned_in: None },
            signer: SignerRef { principal: "mirante-local".into(), key: "SHA256:x".into() },
            created: "2026-11-08".into(),
        };
        VerifiedSkill { manifest_hash: manifest.hash(), manifest, files }
    }

    pub(crate) fn sample() -> InjectedSkills {
        // A SKILL.md with a key that would grant tools: what is written carries only name
        // and description, whatever the verified bytes held.
        let sneaky =
            "---\nname: lookup-api\ndescription: d\nallowed-tools: Bash(*)\n---\nRead ./reference/a.md first.\n";
        InjectedSkills {
            always: vec![skill(
                "tests-first",
                Activation::Always,
                &render_skill_md("tests-first", "What tests-first is for", "Run `cargo test` before committing.\n"),
                &[],
            )],
            on_demand: vec![skill("lookup-api", Activation::OnDemand, sneaky, &[("reference/a.md", "ref\n")])],
        }
    }

    fn mode(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// Every path under `dir`, relative, sorted.
    fn tree(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
                if std::fs::symlink_metadata(&p).unwrap().is_dir() {
                    stack.push(p);
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn write_skill_bundle_creates_a_fresh_per_session_dir_with_rendered_skill_md_only() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let b = write_skill_bundle(&paths, "t1", "s1", Some(&sample()), AgentKind::Claude).unwrap().unwrap();
        assert_eq!(b.dir, paths.session_dir("t1").join("skills/s1"));
        assert_eq!(mode(&b.dir), 0o700);
        let plugin = b.plugin_dir.clone().expect("an on-demand skill makes a plugin");
        assert_eq!(plugin, b.dir.join("claude-plugin"));
        assert_eq!(
            tree(&plugin),
            [
                ".claude-plugin",
                ".claude-plugin/plugin.json",
                "skills",
                "skills/lookup-api",
                "skills/lookup-api/SKILL.md",
                "skills/lookup-api/reference",
                "skills/lookup-api/reference/a.md",
            ]
        );
        // Nothing at the plugin root that loads code or grants anything.
        for p in ["hooks", ".mcp.json", "agents", "commands", "settings.json"] {
            assert!(!plugin.join(p).exists(), "{p}");
        }
        let md = std::fs::read_to_string(plugin.join("skills/lookup-api/SKILL.md")).unwrap();
        assert_eq!(
            md,
            "---\nname: lookup-api\ndescription: \"What lookup-api is for\"\n---\nRead ./reference/a.md first.\n"
        );
        assert!(!md.contains("allowed-tools"));
        assert_eq!(std::fs::read_to_string(plugin.join("skills/lookup-api/reference/a.md")).unwrap(), "ref\n");
        let pj: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap()).unwrap();
        assert_eq!(pj["name"], "mirante-skills");
        for p in tree(&b.dir) {
            let full = b.dir.join(&p);
            let want = if std::fs::symlink_metadata(&full).unwrap().is_dir() { 0o700 } else { 0o600 };
            assert_eq!(mode(&full), want, "{p}");
        }

        // The always-on skill goes into the system prompt file, not the plugin.
        let always = b.always_file.clone().expect("an always-on skill makes the file");
        assert_eq!(always, b.dir.join("always.md"));
        let text = std::fs::read_to_string(&always).unwrap();
        assert_eq!(Some(&text), b.always_text.as_ref());
        assert!(text.starts_with("<mirante-skills>\nStanding instructions from the operator, reviewed in Mirante.\n"));
        assert!(text.contains("## tests-first\nRun `cargo test` before committing.\n"), "{text}");
        assert!(text.ends_with("</mirante-skills>\n"), "{text}");
        assert!(!text.contains("name: tests-first"), "frontmatter is not part of the text: {text}");
        assert!(!plugin.join("skills/tests-first").exists());
        assert_eq!(b.preamble, None);

        // Create-new: the same session never reuses a folder someone else made.
        assert!(write_skill_bundle(&paths, "t1", "s1", Some(&sample()), AgentKind::Claude).is_err());
        // A new session clears the previous session's bundle.
        let b2 = write_skill_bundle(&paths, "t1", "s2", Some(&sample()), AgentKind::Claude).unwrap().unwrap();
        assert!(!b.dir.exists() && b2.dir.exists());
        // No skills: nothing written, the old bundle is gone.
        assert_eq!(write_skill_bundle(&paths, "t1", "s3", None, AgentKind::Claude).unwrap(), None);
        assert!(!b2.dir.exists());
    }

    #[test]
    fn only_always_or_only_on_demand_writes_only_that() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let s = sample();
        let only_always = InjectedSkills { always: s.always.clone(), on_demand: vec![] };
        let b = write_skill_bundle(&paths, "t1", "s1", Some(&only_always), AgentKind::Claude).unwrap().unwrap();
        assert!(b.plugin_dir.is_none() && b.always_file.is_some());
        assert!(!b.dir.join("claude-plugin").exists());
        let only_od = InjectedSkills { always: vec![], on_demand: s.on_demand.clone() };
        let b = write_skill_bundle(&paths, "t1", "s2", Some(&only_od), AgentKind::Claude).unwrap().unwrap();
        assert!(b.plugin_dir.is_some() && b.always_file.is_none() && b.always_text.is_none());
        assert!(!b.dir.join("always.md").exists());
    }

    #[test]
    fn a_symlinked_skills_dir_is_refused() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let elsewhere = d.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(paths.session_dir("t1")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, paths.session_dir("t1").join("skills")).unwrap();
        assert!(write_skill_bundle(&paths, "t1", "s1", Some(&sample()), AgentKind::Claude).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing written through the link");
    }

    #[test]
    fn a_bad_extra_path_or_skill_md_fails_the_bundle() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let md = render_skill_md("x", "d", "b\n");
        for bad in [
            skill("x", Activation::OnDemand, &md, &[("../escape.md", "x")]),
            // A nested SKILL.md would be a second skill with its raw frontmatter.
            skill(
                "x",
                Activation::OnDemand,
                &md,
                &[("reference/SKILL.md", "---\nname: y\nallowed-tools: Bash\n---\n")],
            ),
            skill("x", Activation::OnDemand, &md, &[("a/skill.md", "x")]),
            skill("x", Activation::OnDemand, "no frontmatter", &[]),
            skill("Bad", Activation::OnDemand, &md, &[]),
        ] {
            let s = InjectedSkills { always: vec![], on_demand: vec![bad] };
            assert!(write_skill_bundle(&paths, "t1", "s1", Some(&s), AgentKind::Claude).is_err());
            assert!(!d.path().join("sessions/escape.md").exists());
        }
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn the_append_file_flag_is_probed_once_per_binary() {
        let d = TestDir::new();
        let log = d.path().join("calls");
        let yes = script(
            d.path(),
            "claude-new",
            &format!("echo \"$@\" >> {}\necho '{{\"success\":false}}'\nexit 1", log.display()),
        );
        let no = script(d.path(), "claude-old", "echo \"error: unknown option '$1'\" >&2\nexit 1");
        let hangs = script(d.path(), "claude-hangs", "sleep 30");
        assert!(append_file_supported(&yes, Duration::from_secs(10)));
        assert!(append_file_supported(&yes, Duration::from_secs(10)));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(calls.lines().count(), 1, "cached: {calls}");
        // Only a root flag and a subcommand that starts no session and calls no model.
        assert!(calls.starts_with("--append-system-prompt-file /dev/null plugin validate --json "), "{calls}");
        assert!(!append_file_supported(&no, Duration::from_secs(10)));
        let t = std::time::Instant::now();
        assert!(!append_file_supported(&hangs, Duration::from_millis(300)), "a probe that hangs counts as no");
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(!append_file_supported(&d.path().join("missing"), Duration::from_secs(1)));
    }
    /// Live check (no network, no session): `MIRANTE_LIVE_CLAUDE=/path/to/claude cargo test
    /// -p mirante-daemon live_claude -- --ignored` validates a generated plugin with
    /// `claude plugin validate --json` and probes the hidden system-prompt flag.
    #[test]
    #[ignore]
    fn live_claude_validates_the_generated_plugin() {
        let Some(claude) = std::env::var_os("MIRANTE_LIVE_CLAUDE").map(PathBuf::from) else {
            eprintln!("MIRANTE_LIVE_CLAUDE is not set: skipped");
            return;
        };
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let b = write_skill_bundle(&paths, "t1", "s1", Some(&sample()), AgentKind::Claude).unwrap().unwrap();
        let out = Command::new(&claude)
            .args(["plugin", "validate", "--json"])
            .arg(b.plugin_dir.as_ref().unwrap())
            .output()
            .unwrap();
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        eprintln!("{report:#}");
        assert!(out.status.success() && report["success"] == true, "{report}");
        assert_eq!(report["manifest"]["errors"], serde_json::json!([]));
        assert!(append_file_supported(&claude, Duration::from_secs(10)), "the file flag is accepted");
    }
    #[test]
    fn preamble_lists_on_demand_paths_and_always_text() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let mut s = sample();
        let md = render_skill_md("with-files", "d", "See ./notes.md.\n");
        s.always.push(skill("with-files", Activation::Always, &md, &[("notes.md", "n\n")]));
        let b = write_skill_bundle(&paths, "t1", "s1", Some(&s), AgentKind::Codex).unwrap().unwrap();
        assert!(b.plugin_dir.is_none() && b.always_file.is_none() && b.always_text.is_none());
        let p = b.preamble.clone().expect("codex gets a first-turn item");
        assert!(p.starts_with("<mirante-skills>\n"), "{p}");
        assert!(p.ends_with("</mirante-skills>\n"), "{p}");
        assert!(p.contains("operator-approved"), "clearly labeled: {p}");
        assert!(p.contains("### tests-first\nRun `cargo test` before committing.\n"), "{p}");
        let od = b.dir.join("lookup-api/SKILL.md");
        assert!(
            p.contains(&format!("- lookup-api — What lookup-api is for — read {} when relevant", od.display())),
            "{p}"
        );
        assert!(p.contains(&format!("(This skill's files are in {}/.)", b.dir.join("with-files").display())), "{p}");
        // The on-demand skill is on disk where the index says, re-rendered.
        let text = std::fs::read_to_string(&od).unwrap();
        assert!(text.starts_with("---\nname: lookup-api\ndescription: \"What lookup-api is for\"\n---\n"), "{text}");
        assert!(!text.contains("allowed-tools"));
        assert_eq!(std::fs::read_to_string(b.dir.join("lookup-api/reference/a.md")).unwrap(), "ref\n");
        assert_eq!(std::fs::read_to_string(b.dir.join("with-files/notes.md")).unwrap(), "n\n");
        assert!(!b.dir.join("tests-first").exists(), "an always-on skill without files needs no folder");
        assert!(!b.dir.join("claude-plugin").exists() && !b.dir.join("always.md").exists());
    }
}
