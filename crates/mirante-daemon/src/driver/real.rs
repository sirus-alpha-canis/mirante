//! The production driver factory: the real `claude` in a PTY with Mirante's scoped hook
//! settings, or the real `codex app-server` in auto-edits mode (its stderr in a private log
//! under `~/.mirante/sessions/<task>/`). Binaries: `MIRANTE_CLAUDE_BIN` /
//! `MIRANTE_CODEX_BIN`, else PATH, else the well-known install directories, never a wrapper
//! shim (`mirante_core::agent_bin`); an agent found outside PATH runs with its directory
//! first on PATH (an npm agent needs the `node` beside it). The hook command:
//! `MIRANTE_HOOK_BIN`, else `mirante-hook` next to the daemon executable.
//!
//! Claude's launch comes from Plan 2a's `claude_launch::launch_spec` (real binary, env
//! scrub that never touches auth, `--permission-mode manual`, additive `--settings`, the
//! agent terminal variables, `MIRANTE_TASK_ID` and `MIRANTE_SESSION_ID` = this Mirante
//! session, which the hook echoes back and the hub fences on). Added here: `MIRANTE_HOME`
//! (the hook must reach this daemon) and, on a fresh start, `--session-id <uuid>`, so the
//! native id is known before Claude prints anything.
//!
//! Repo memory (only for repos where it was enabled, `SpawnRequest::mcp`): Claude gets
//! `--mcp-config <session dir>/mcp.json` and the read-only memory tools pre-allowed in its
//! settings; Codex gets `-c mcp_servers.mirante-memory.*` overrides. Per task, never global.
//!
//! Skills (`SpawnRequest::skills`, see [`super::skill_bundle`]): Claude gets
//! `--plugin-dir <bundle>/claude-plugin` for the on-demand ones and
//! `--append-system-prompt-file <bundle>/always.md` for the always-on ones (inline
//! `--append-system-prompt` when that hidden flag is refused), both before `--mcp-config`.
//! A bundle that cannot be written is reported as a step and the session starts without
//! skills.
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mirante_core::agent_bin::{self, child_path, Agent, Lookup};
use mirante_core::claude_launch::{
    launch_spec, mcp_json, memory_allow, settings_json, AlwaysOn, LaunchInput, AGENT_ENV, MCP_SERVER, TASK_ENV,
};
use mirante_core::model::AgentKind;
use mirante_ipc::paths::HOME_ENV;
use mirante_ipc::types::AgentBinView;

use super::claude::{ClaudeDriver, ClaudeTiming};
use super::codex::{codex_env_to_scrub, ChildEnv, CodexDriver, CodexTiming};
use super::pty::PtySpec;
pub use super::skill_bundle::{append_file_supported, write_skill_bundle, SkillBundle};
use super::{Driver, DriverError, DriverEvent, DriverFactory, Sink, SpawnRequest};

#[derive(Debug, Clone)]
pub struct RealFactory {
    /// PTY size before any client attaches.
    pub rows: u16,
    pub cols: u16,
}

impl Default for RealFactory {
    fn default() -> Self {
        RealFactory { rows: 40, cols: 120 }
    }
}

pub fn hook_bin() -> Result<PathBuf, DriverError> {
    let p = match std::env::var_os("MIRANTE_HOOK_BIN").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe()?.with_file_name("mirante-hook"),
    };
    let ok = std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if ok {
        Ok(p)
    } else {
        Err(DriverError::Failed(format!("hook binary not found: {}", p.display())))
    }
}

/// The memory MCP server: `MIRANTE_MCP_BIN`, else `mirante-mcp` next to the daemon
/// executable; it must be an executable file. `Err` says why there is none.
pub fn mcp_bin() -> Result<PathBuf, String> {
    let p = match std::env::var_os("MIRANTE_MCP_BIN").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("mirante-mcp"),
    };
    let ok = std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if ok {
        Ok(p)
    } else {
        Err(format!("mirante-mcp not found: {}", p.display()))
    }
}

/// `s` as a TOML basic string (`"` and `\` escaped). Control characters are refused: they
/// have no place in a path or a task id, and a `-c` override must stay one value.
pub fn toml_str(s: &str) -> Result<String, String> {
    if s.chars().any(char::is_control) {
        return Err(format!("{s:?} has a control character"));
    }
    Ok(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// The per-task memory server for `codex app-server`, as `-c` config overrides (nothing is
/// written to `~/.codex`). Codex hands an MCP server only a few variables of its own
/// environment, so the task, the daemon's home and the agent marker go in `env`.
///
/// `-c` merges into the user's `config.toml`: `args` is set (empty) so a same-named user
/// entry's arguments cannot reach the binary. Other keys of such an entry (`url`, `cwd`,
/// tool filters) cannot be removed with `-c`; the distinct server name makes that
/// collision unlikely.
pub fn codex_mcp_args(bin: &Path, task: &str, home: &Path) -> Result<Vec<String>, String> {
    let (bin, task, home) = (toml_str(&bin.to_string_lossy())?, toml_str(task)?, toml_str(&home.to_string_lossy())?);
    Ok(vec![
        "-c".to_string(),
        format!("mcp_servers.{MCP_SERVER}.command={bin}"),
        "-c".to_string(),
        format!("mcp_servers.{MCP_SERVER}.args=[]"),
        "-c".to_string(),
        format!("mcp_servers.{MCP_SERVER}.env={{{TASK_ENV}={task},{HOME_ENV}={home},{AGENT_ENV}={task}}}"),
    ])
}

/// Writes Claude's per-task config into the session dir: the settings (hook for every
/// event; the memory read tools pre-allowed when memory is on) and, when `req.mcp` is set,
/// `mcp.json`; otherwise a previous session's `mcp.json` is removed. Returns their paths.
pub fn write_claude_config(req: &SpawnRequest, hook: &Path) -> std::io::Result<(PathBuf, Option<PathBuf>)> {
    let dir = req.paths.session_dir(&req.task.id);
    let settings = dir.join("claude-settings.json");
    let hook = hook.to_string_lossy();
    let mcp = match &req.mcp {
        Some(bin) => {
            let path = dir.join("mcp.json");
            write_private(&path, &mcp_json(bin, &req.task.id, req.paths.home()).to_string())?;
            write_private(&settings, &settings_json(&hook, &memory_allow()).to_string())?;
            Some(path)
        }
        None => {
            match std::fs::remove_file(dir.join("mcp.json")) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            write_private(&settings, &settings_json(&hook, &[]).to_string())?;
            None
        }
    };
    Ok((settings, mcp))
}

/// Writes `contents` to `path` (mode 0600) inside a 0700 directory.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(path)?;
    f.write_all(contents.as_bytes())
}

/// Most bytes `codex-stderr.log` keeps before it is moved to `codex-stderr.log.1`.
pub const STDERR_LOG_CAP: u64 = 1 << 20;

/// Makes `path` ready to be appended to (mode 0600 in a 0700 directory), keeping what
/// earlier sessions wrote; past `cap` bytes it is moved to `<path>.1` (replacing the older
/// one) and a fresh file is started.
pub fn prepare_append_log(path: &Path, cap: u64) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > cap) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        std::fs::rename(path, &old)?;
    }
    let f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
}

/// A random (version 4) UUID, lowercase `8-4-4-4-12`, from `/dev/urandom`.
pub fn new_uuid_v4() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

fn parent_env() -> Vec<String> {
    std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect()
}

/// What Claude's launch carries beyond the settings: the MCP config and the skills.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeExtras<'a> {
    pub mcp_config: Option<&'a Path>,
    pub plugin_dir: Option<&'a Path>,
    pub always: Option<AlwaysOn<'a>>,
}

/// How the bundle's always-on text reaches Claude: the file when `file_flag` (the binary
/// accepts `--append-system-prompt-file`), else the text inline.
pub fn claude_always(b: &SkillBundle, file_flag: bool) -> Option<AlwaysOn<'_>> {
    match (&b.always_file, &b.always_text) {
        (Some(f), _) if file_flag => Some(AlwaysOn::File(f)),
        (_, Some(t)) => Some(AlwaysOn::Inline(t)),
        _ => None,
    }
}

/// How long the `--append-system-prompt-file` probe may take.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The session's skills on disk for `agent`, or `None` (none, or they could not be written:
/// reported as a `skills_failed` step, and the session starts without them).
fn skill_bundle_for(req: &SpawnRequest, agent: AgentKind, sink: &Sink) -> Option<SkillBundle> {
    match write_skill_bundle(&req.paths, &req.task.id, &req.session_id, req.skills.as_ref(), agent) {
        Ok(b) => b,
        Err(e) => {
            sink.emit(DriverEvent::Step(serde_json::json!({ "type": "skills_failed", "error": e })));
            None
        }
    }
}

/// The PTY launch for `req`, and the native session id pinned for a fresh start
/// (`None` when resuming: the id to resume is `req.resume`).
pub fn claude_spec(
    req: &SpawnRequest,
    claude: &Path,
    settings: &Path,
    size: (u16, u16),
    parent_env: Vec<String>,
    extras: ClaudeExtras<'_>,
) -> std::io::Result<(PtySpec, Option<String>)> {
    let spec = launch_spec(LaunchInput {
        claude_bin: claude,
        worktree: Path::new(&req.task.worktree_path),
        settings_path: settings,
        task_id: &req.task.id,
        session_id: &req.session_id,
        resume: req.resume.as_deref(),
        parent_env,
        mcp_config: extras.mcp_config,
        plugin_dir: extras.plugin_dir,
        always: extras.always,
    });
    let mut args = spec.args;
    let pinned = match req.resume {
        Some(_) => None,
        None => {
            let id = new_uuid_v4()?;
            args.push("--session-id".to_string());
            args.push(id.clone());
            Some(id)
        }
    };
    let mut env_set = spec.env_set;
    // The hook (a child of claude) must reach this daemon, not one under another home.
    env_set.push((HOME_ENV.to_string(), req.paths.home().to_string_lossy().into_owned()));
    let pty = PtySpec {
        program: spec.program,
        args,
        cwd: spec.cwd,
        env_remove: spec.env_remove,
        env_set,
        rows: size.0,
        cols: size.1,
    };
    Ok((pty, pinned))
}

/// What the codex process gets set in its environment: the agent marker
/// ([`AGENT_ENV`] = the task id) and, when needed, `PATH` with its directory first.
pub fn codex_env_set(req: &SpawnRequest, bin: &Path, path_var: Option<&str>) -> Vec<(String, String)> {
    let mut env = vec![(AGENT_ENV.to_string(), req.task.id.clone())];
    env.extend(agent_path_env(bin, path_var));
    env
}

/// `PATH` for an agent at `bin`: its directory first when PATH lacks it, else nothing to set.
pub fn agent_path_env(bin: &Path, path_var: Option<&str>) -> Vec<(String, String)> {
    child_path(bin, path_var).map(|p| vec![("PATH".to_string(), p)]).unwrap_or_default()
}

fn resolve_agent(agent: Agent) -> Result<PathBuf, DriverError> {
    agent_bin::resolve(agent, &Lookup::from_env(agent)).map(|r| r.path).map_err(DriverError::Failed)
}

/// Each agent binary as this daemon would resolve it (for `mirante doctor`).
pub fn agents_report() -> Vec<AgentBinView> {
    agents_report_with(Lookup::from_env)
}

pub fn agents_report_with(lookup: impl Fn(Agent) -> Lookup) -> Vec<AgentBinView> {
    [Agent::Claude, Agent::Codex]
        .into_iter()
        .map(|agent| match agent_bin::resolve(agent, &lookup(agent)) {
            Ok(r) => AgentBinView {
                agent: agent.exe().to_string(),
                path: Some(r.path.to_string_lossy().into_owned()),
                source: Some(r.source.to_string()),
                error: None,
            },
            Err(e) => AgentBinView { agent: agent.exe().to_string(), path: None, source: None, error: Some(e) },
        })
        .collect()
}

impl RealFactory {
    fn claude(&self, req: &SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        let claude = resolve_agent(Agent::Claude)?;
        let hook = hook_bin()?;
        let (settings, mcp) = write_claude_config(req, &hook)?;
        let bundle = skill_bundle_for(req, AgentKind::Claude, &sink);
        let always = bundle.as_ref().and_then(|b| {
            let file_flag = b.always_file.is_some() && append_file_supported(&claude, PROBE_TIMEOUT);
            claude_always(b, file_flag)
        });
        let extras = ClaudeExtras {
            mcp_config: mcp.as_deref(),
            plugin_dir: bundle.as_ref().and_then(|b| b.plugin_dir.as_deref()),
            always,
        };
        let (mut pty, pinned) = claude_spec(req, &claude, &settings, (self.rows, self.cols), parent_env(), extras)?;
        pty.env_set.extend(agent_path_env(&claude, std::env::var("PATH").ok().as_deref()));
        let driver = ClaudeDriver::spawn(pty, sink.clone(), ClaudeTiming::default(), req.resume.is_some())?;
        if let Some(id) = pinned {
            // Known before Claude prints anything: a crash during boot can still be resumed.
            // Only once the process exists: a failed spawn leaves nothing to resume.
            sink.emit(DriverEvent::NativeId(id));
        }
        Ok(driver)
    }

    fn codex(&self, req: &SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        let bin = resolve_agent(Agent::Codex)?;
        let env_set = codex_env_set(req, &bin, std::env::var("PATH").ok().as_deref());
        let log = req.paths.session_dir(&req.task.id).join("codex-stderr.log");
        // Appended, never truncated: the last session's stderr explains its crash.
        prepare_append_log(&log, STDERR_LOG_CAP)?;
        let env_remove = codex_env_to_scrub(parent_env());
        let extra_args = match &req.mcp {
            Some(mcp) => codex_mcp_args(mcp, &req.task.id, req.paths.home()).map_err(DriverError::Failed)?,
            None => Vec::new(),
        };
        let d = CodexDriver::spawn(
            &bin,
            &req.task.worktree_path,
            req.resume.as_deref(),
            ChildEnv { remove: &env_remove, set: &env_set, args: &extra_args },
            &log,
            sink,
            CodexTiming::default(),
        )?;
        Ok(d)
    }
}

impl DriverFactory for RealFactory {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        match req.task.agent {
            AgentKind::Claude => self.claude(&req, sink),
            AgentKind::Codex => self.codex(&req, sink),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{Lifecycle, Status, Task};
    use mirante_ipc::test_dir::TestDir;
    use mirante_ipc::Paths;

    #[test]
    fn private_files_are_0600_in_0700_dirs() {
        let d = TestDir::new();
        let p = d.path().join("sessions/t1/claude-settings.json");
        write_private(&p, "{}").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(p.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        write_private(&p, "{\"x\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"x\":1}");
    }

    #[test]
    fn the_codex_stderr_log_is_appended_and_rotated_past_its_cap() {
        let d = TestDir::new();
        let log = d.path().join("sessions/t1/codex-stderr.log");
        prepare_append_log(&log, 100).unwrap();
        assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(log.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::OpenOptions::new().append(true).open(&log).unwrap().write_all(b"first session\n").unwrap();
        // The next session keeps what the last one wrote (its crash is still readable).
        prepare_append_log(&log, 100).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "first session\n");
        std::fs::write(&log, "x".repeat(150)).unwrap();
        prepare_append_log(&log, 100).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
        let old = log.with_file_name("codex-stderr.log.1");
        assert_eq!(std::fs::read_to_string(&old).unwrap().len(), 150);
        assert_eq!(std::fs::metadata(&old).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn uuids_are_random_version_4() {
        let (a, b) = (new_uuid_v4().unwrap(), new_uuid_v4().unwrap());
        assert_ne!(a, b);
        let parts: Vec<usize> = a.split('-').map(str::len).collect();
        assert_eq!(parts, vec![8, 4, 4, 4, 12]);
        assert_eq!(&a[14..15], "4");
        assert!("89ab".contains(&a[19..20]), "{a}");
        assert!(a.chars().all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    fn request(resume: Option<&str>) -> SpawnRequest {
        SpawnRequest {
            task: Task {
                id: "task-1".into(),
                title: "T".into(),
                repo_path: "/r".into(),
                worktree_path: "/w/tree".into(),
                branch: "feat".into(),
                agent: AgentKind::Claude,
                goal: "g".into(),
                notes: String::new(),
                lifecycle: Lifecycle::Active,
                status: Status::Idle,
                created_at: 0,
                updated_at: 0,
            },
            session_id: "sess-42".into(),
            resume: resume.map(String::from),
            paths: Paths::new(Path::new("/h")),
            mcp: None,
            skills: None,
        }
    }

    fn env<'a>(spec: &'a PtySpec, key: &str) -> Option<&'a str> {
        spec.env_set.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_fresh_launch_pins_the_session_id_and_stamps_the_mirante_session() {
        let parent = vec!["CLAUDECODE".to_string(), "ANTHROPIC_API_KEY".to_string()];
        let (spec, pinned) = claude_spec(
            &request(None),
            Path::new("/bin/claude"),
            Path::new("/h/s.json"),
            (40, 120),
            parent,
            ClaudeExtras::default(),
        )
        .unwrap();
        let id = pinned.expect("fresh start pins an id");
        let args = spec.args.join(" ");
        assert!(args.contains(&format!("--session-id {id}")), "{args}");
        assert!(args.contains("--permission-mode manual") && args.contains("--settings /h/s.json"), "{args}");
        assert!(!args.contains("--resume") && !args.contains("dangerously"), "{args}");
        assert_eq!(env(&spec, "MIRANTE_SESSION_ID"), Some("sess-42"));
        assert_eq!(env(&spec, "MIRANTE_TASK_ID"), Some("task-1"));
        assert_eq!(env(&spec, "MIRANTE_TASK"), Some("task-1"));
        assert_eq!(env(&spec, "MIRANTE_HOME"), Some("/h"));
        assert_eq!(env(&spec, "TERM"), Some("xterm-256color"));
        assert!(spec.env_remove.contains(&"CLAUDECODE".to_string()));
        assert!(!spec.env_remove.iter().any(|k| k.starts_with("ANTHROPIC_")));
        assert_eq!((spec.cwd.as_path(), spec.rows, spec.cols), (Path::new("/w/tree"), 40, 120));
    }

    #[test]
    fn codex_mcp_args_are_toml_overrides() {
        let a = codex_mcp_args(Path::new("/opt/m/mirante-mcp"), "t1", Path::new("/h/.mirante")).unwrap();
        assert_eq!(
            a,
            [
                "-c",
                "mcp_servers.mirante-memory.command=\"/opt/m/mirante-mcp\"",
                "-c",
                "mcp_servers.mirante-memory.args=[]",
                "-c",
                "mcp_servers.mirante-memory.env={MIRANTE_TASK_ID=\"t1\",MIRANTE_HOME=\"/h/.mirante\",MIRANTE_TASK=\"t1\"}"
            ]
        );
    }

    #[test]
    fn toml_strings_escape_quotes_and_backslashes() {
        assert_eq!(toml_str(r#"/a "b"\c"#).unwrap(), r#""/a \"b\"\\c""#);
        assert!(toml_str("a\nb").is_err() && toml_str("a\u{7f}").is_err() && toml_str("\u{1b}[2J").is_err());
        let a = codex_mcp_args(Path::new("/x/o\"brien/mirante-mcp"), "t1", Path::new("/h")).unwrap();
        assert_eq!(a[1], "mcp_servers.mirante-memory.command=\"/x/o\\\"brien/mirante-mcp\"");
        assert!(codex_mcp_args(Path::new("/x\ny/mirante-mcp"), "t1", Path::new("/h")).is_err());
    }

    #[test]
    fn claude_gets_mcp_json_and_the_read_tools_only_when_memory_is_on() {
        let d = TestDir::new();
        let mut req = request(None);
        req.paths = Paths::new(&d.path().join("home"));
        let hook = Path::new("/opt/m/mirante-hook");
        let (settings, mcp) = write_claude_config(&req, hook).unwrap();
        assert_eq!(mcp, None);
        let s: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(s["permissions"]["allow"], serde_json::json!([]));

        req.mcp = Some(PathBuf::from("/opt/m/mirante-mcp"));
        let (settings, mcp) = write_claude_config(&req, hook).unwrap();
        let mcp = mcp.expect("mcp.json written");
        assert_eq!(mcp, req.paths.session_dir("task-1").join("mcp.json"));
        assert_eq!(std::fs::metadata(&mcp).unwrap().permissions().mode() & 0o777, 0o600);
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&mcp).unwrap()).unwrap();
        assert_eq!(m["mcpServers"]["mirante-memory"]["command"], "/opt/m/mirante-mcp");
        assert_eq!(m["mcpServers"]["mirante-memory"]["env"]["MIRANTE_TASK_ID"], "task-1");
        assert_eq!(m["mcpServers"]["mirante-memory"]["env"]["MIRANTE_HOME"], d.path().join("home").to_str().unwrap());
        let s: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            s["permissions"]["allow"],
            serde_json::json!(["mcp__mirante-memory__memory_search", "mcp__mirante-memory__memory_get"])
        );

        // The full launch: `--mcp-config` followed only by `--flag value` pairs.
        let extras = ClaudeExtras { mcp_config: Some(&mcp), ..ClaudeExtras::default() };
        let (spec, _) = claude_spec(&req, Path::new("/bin/claude"), &settings, (40, 120), vec![], extras).unwrap();
        let i = spec.args.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!(spec.args[i + 1], mcp.to_str().unwrap());
        assert_eq!((spec.args.len() - i) % 2, 0, "{:?}", spec.args);
        assert!(spec.args[i + 2..].iter().step_by(2).all(|a| a.starts_with("--")), "{:?}", spec.args);

        // Memory turned off before the next start: no stale mcp.json is left to load.
        req.mcp = None;
        let (_, mcp_again) = write_claude_config(&req, hook).unwrap();
        assert_eq!(mcp_again, None);
        assert!(!mcp.exists(), "the previous session's mcp.json was removed");
    }

    #[test]
    fn codex_runs_with_the_agent_marker() {
        let env = codex_env_set(&request(None), Path::new("/usr/bin/codex"), Some("/usr/bin:/bin"));
        assert_eq!(env, [("MIRANTE_TASK".to_string(), "task-1".to_string())]);
        let nvm = codex_env_set(&request(None), Path::new("/h/n/bin/codex"), Some("/usr/bin"));
        assert!(nvm.iter().any(|(k, _)| k == "PATH") && nvm.iter().any(|(k, v)| k == "MIRANTE_TASK" && v == "task-1"));
    }

    #[test]
    fn a_resume_passes_the_native_id_and_pins_nothing() {
        let (spec, pinned) = claude_spec(
            &request(Some("nat-1")),
            Path::new("/bin/claude"),
            Path::new("/h/s.json"),
            (40, 120),
            vec![],
            ClaudeExtras::default(),
        )
        .unwrap();
        assert_eq!(pinned, None);
        let args = spec.args.join(" ");
        assert!(args.contains("--resume nat-1") && !args.contains("--session-id"), "{args}");
        assert_eq!(env(&spec, "MIRANTE_SESSION_ID"), Some("sess-42"));
    }

    #[test]
    fn an_agent_outside_path_runs_with_its_directory_first() {
        // nvm's codex is `#!/usr/bin/env node`: node lives next to it, not on a GUI PATH.
        let codex = Path::new("/h/.nvm/versions/node/v22.14.0/bin/codex");
        assert_eq!(
            agent_path_env(codex, Some("/usr/bin:/bin")),
            vec![("PATH".to_string(), "/h/.nvm/versions/node/v22.14.0/bin:/usr/bin:/bin".to_string())]
        );
        assert!(agent_path_env(Path::new("/usr/bin/claude"), Some("/usr/bin:/bin")).is_empty());
    }

    #[test]
    fn the_agents_report_says_where_each_binary_came_from() {
        let d = TestDir::new();
        let bin = d.path().join("claude");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let l = |agent| mirante_core::agent_bin::Lookup {
            override_bin: (agent == Agent::Claude).then(|| bin.to_string_lossy().into_owned()),
            path_var: Some(String::new()),
            home: None,
            well_known: false,
        };
        let r = agents_report_with(l);
        assert_eq!(r[0].agent, "claude");
        assert_eq!(r[0].path.as_deref(), Some(bin.to_str().unwrap()));
        assert_eq!(r[0].source.as_deref(), Some("MIRANTE_CLAUDE_BIN"));
        assert_eq!(r[1].agent, "codex");
        assert!(r[1].path.is_none() && r[1].error.as_deref().is_some_and(|e| e.contains("no codex")), "{:?}", r[1]);
    }
    #[test]
    fn claude_gets_its_session_bundle_before_mcp_config_with_the_inline_fallback() {
        let d = TestDir::new();
        let mut req = request(None);
        req.paths = Paths::new(&d.path().join("home"));
        std::fs::create_dir_all(req.paths.home()).unwrap();
        req.mcp = Some(PathBuf::from("/opt/m/mirante-mcp"));
        req.skills = Some(crate::driver::skill_bundle::tests::sample());
        let (settings, mcp) = write_claude_config(&req, Path::new("/opt/m/mirante-hook")).unwrap();
        let b = write_skill_bundle(&req.paths, &req.task.id, &req.session_id, req.skills.as_ref(), AgentKind::Claude)
            .unwrap()
            .unwrap();
        for (supported, flag, value) in [
            (true, "--append-system-prompt-file", b.always_file.as_ref().unwrap().to_str().unwrap().to_string()),
            (false, "--append-system-prompt", b.always_text.clone().unwrap()),
        ] {
            let extras = ClaudeExtras {
                mcp_config: mcp.as_deref(),
                plugin_dir: b.plugin_dir.as_deref(),
                always: claude_always(&b, supported),
            };
            let (spec, _) = claude_spec(&req, Path::new("/bin/claude"), &settings, (40, 120), vec![], extras).unwrap();
            let at = |f: &str| spec.args.iter().position(|a| a == f).unwrap();
            assert_eq!(spec.args[at(flag) + 1], value);
            assert_eq!(spec.args[at("--plugin-dir") + 1], b.plugin_dir.as_ref().unwrap().to_str().unwrap());
            let i = at("--mcp-config");
            assert!(at("--plugin-dir") < i && at(flag) < i, "{:?}", spec.args);
            assert_eq!((spec.args.len() - i) % 2, 0, "{:?}", spec.args);
            assert!(spec.args[i + 2..].iter().step_by(2).all(|a| a.starts_with("--")), "{:?}", spec.args);
        }
        // No skills: neither flag.
        let (spec, _) =
            claude_spec(&req, Path::new("/bin/claude"), &settings, (40, 120), vec![], ClaudeExtras::default()).unwrap();
        assert!(!spec.args.iter().any(|a| a == "--plugin-dir" || a.starts_with("--append-system-prompt")));
    }
}
