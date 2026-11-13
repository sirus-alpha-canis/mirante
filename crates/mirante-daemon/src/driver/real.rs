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
//! Codex gets the bundle's first-turn item (`ChildEnv::first_turn`), sent before the goal
//! on a new thread; a resumed thread gets instead, when the hub names any, the withdrawal
//! of skills retired or changed since (their files removed). A bundle that cannot be
//! written is reported as a step and the session starts without skills.
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mirante_core::agent_bin::{self, child_path, Agent, Lookup};
use mirante_core::claude_launch::{
    launch_spec, mcp_json, memory_allow, orchestrator_allow, read_only_allow, settings_json, AlwaysOn, LaunchInput,
    McpServers, AGENT_ENV, TASK_ENV,
};
use mirante_core::model::AgentKind;
use mirante_ipc::paths::HOME_ENV;
use mirante_ipc::types::AgentBinView;

use super::claude::{ClaudeDriver, ClaudeTiming};
use super::codex::{codex_env_to_scrub, ChildEnv, CodexDriver, CodexTiming};
use super::pty::PtySpec;
pub use super::skill_bundle::{
    append_file_supported, withdraw_skills, withdrawal_item, write_skill_bundle, SkillBundle,
};
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

/// The per-task memory, skills and orchestrator servers for `codex app-server`, as `-c`
/// config overrides (nothing is written to `~/.codex`). Codex hands an
/// MCP server only a few variables of its own environment, so the task, the daemon's home
/// and the agent marker go in `env`.
///
/// `-c` merges into the user's `config.toml`: `args` is always set so a same-named user
/// entry's arguments cannot reach the binary. Other keys of such an entry (`url`, `cwd`,
/// tool filters) cannot be removed with `-c`; the distinct server names make that
/// collision unlikely.
pub fn codex_mcp_args(servers: McpServers<'_>, task: &str, home: &Path) -> Result<Vec<String>, String> {
    let (task, home) = (toml_str(task)?, toml_str(&home.to_string_lossy())?);
    let mut out = Vec::new();
    for (name, bin, args) in servers.each() {
        let bin = toml_str(&bin.to_string_lossy())?;
        let args: Vec<String> = args.iter().map(|a| toml_str(a)).collect::<Result<_, _>>()?;
        out.extend([
            "-c".to_string(),
            format!("mcp_servers.{name}.command={bin}"),
            "-c".to_string(),
            format!("mcp_servers.{name}.args=[{}]", args.join(",")),
            "-c".to_string(),
            format!("mcp_servers.{name}.env={{{TASK_ENV}={task},{HOME_ENV}={home},{AGENT_ENV}={task}}}"),
        ]);
    }
    Ok(out)
}

/// Writes Claude's per-task config into the session dir: the settings (hook for every
/// event; the memory read tools pre-allowed when memory is on, the orchestrator's read
/// tools when it is given, `skill_propose` and `task_spawn` never) and, when any of
/// Mirante's servers is given, `mcp.json` with them; otherwise a previous session's
/// `mcp.json` is removed. Returns their paths.
pub fn write_claude_config(req: &SpawnRequest, hook: &Path) -> std::io::Result<(PathBuf, Option<PathBuf>)> {
    let dir = req.paths.session_dir(&req.task.id);
    let settings = dir.join("claude-settings.json");
    let hook = hook.to_string_lossy();
    let mut allow = if req.mcp.is_some() { memory_allow() } else { Vec::new() };
    if req.orchestrator.is_some() {
        allow.extend(orchestrator_allow());
    }
    // A workspace task reads its folder's other repositories without asking (writes outside
    // the workspace still ask: Claude runs in manual mode).
    allow.extend(req.task.group_path.as_deref().and_then(|g| read_only_allow(Path::new(g))));
    let mcp = if servers_of(req).any() {
        let path = dir.join("mcp.json");
        let json = mcp_json(servers_of(req), &req.task.id, req.paths.home());
        write_private(&path, &json.to_string())?;
        Some(path)
    } else {
        match std::fs::remove_file(dir.join("mcp.json")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        None
    };
    write_private(&settings, &settings_json(&hook, &allow).to_string())?;
    Ok((settings, mcp))
}

/// The Mirante MCP servers `req` gives its session.
fn servers_of(req: &SpawnRequest) -> McpServers<'_> {
    McpServers {
        memory: req.mcp.as_deref(),
        skills: req.skills_mcp.as_deref(),
        orchestrator: req.orchestrator.as_deref(),
    }
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

/// How often a running Codex's stderr log is checked against [`STDERR_LOG_CAP`].
const STDERR_LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// Cuts a log a running process appends to (it keeps its descriptor): past `cap` bytes
/// the content is copied to `<path>.1` (0600, replacing the older one) and the file is
/// emptied in place, so the process's next appends start it again. `true` when it was cut.
pub fn trim_log_if_over(path: &Path, cap: u64) -> std::io::Result<bool> {
    if !std::fs::metadata(path).is_ok_and(|m| m.len() > cap) {
        return Ok(false);
    }
    let mut old = path.as_os_str().to_owned();
    old.push(".1");
    let old = std::path::PathBuf::from(old);
    let _ = std::fs::remove_file(&old);
    let mut to = std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&old)?;
    std::io::copy(&mut std::fs::File::open(path)?, &mut to)?;
    std::fs::OpenOptions::new().write(true).open(path)?.set_len(0)?;
    Ok(true)
}

/// Checks `log` every [`STDERR_LOG_EVERY`] while `driver` lives ([`trim_log_if_over`]).
fn watch_stderr_log<T: ?Sized + Send + Sync + 'static>(log: std::path::PathBuf, driver: std::sync::Weak<T>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(STDERR_LOG_EVERY);
        if driver.upgrade().is_none() {
            return;
        }
        if let Err(e) = trim_log_if_over(&log, STDERR_LOG_CAP) {
            eprintln!("mirante-daemon: cutting {}: {e}", log.display());
        }
    });
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
/// accepts `--append-system-prompt-file`), else the text inline. The file is preferred:
/// inline, the text sits in Claude's argv, which any local user can read with `ps` for as
/// long as the session runs (skills are lint-checked for secrets, but they are still the
/// operator's instructions).
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

/// Codex's bundle: none on a resumed thread, whose history already points at the files of
/// the session that started it (they are left in place).
fn codex_skill_bundle(req: &SpawnRequest, sink: &Sink) -> Option<SkillBundle> {
    match req.resume {
        Some(_) => None,
        None => skill_bundle_for(req, AgentKind::Codex, sink),
    }
}

/// The item Codex's first `turn/start` carries: a new thread's skills (`bundle`), or on a
/// resume the withdrawal of `req.withdrawn` (their files removed, a `skills_withdrawn`
/// step; a failure is a `skills_failed` step and the item is still sent).
fn codex_first_turn(req: &SpawnRequest, bundle: Option<&SkillBundle>, sink: &Sink) -> Option<String> {
    if req.resume.is_none() {
        return bundle.and_then(|b| b.preamble.clone());
    }
    if req.withdrawn.is_empty() {
        return None;
    }
    sink.emit(DriverEvent::Step(serde_json::json!({ "type": "skills_withdrawn", "names": req.withdrawn })));
    match withdraw_skills(&req.paths, &req.task.id, &req.withdrawn) {
        Ok(item) => item,
        Err(e) => {
            sink.emit(DriverEvent::Step(serde_json::json!({ "type": "skills_failed", "error": e })));
            withdrawal_item(&req.withdrawn)
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
        worktree: Path::new(req.task.cwd()),
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

/// Where a binary came from, and the shims passed over on the way ("PATH; skipped cmux shim
/// /Users/me/.cmuxterm/cmux-cli-shims/x/claude").
fn source_note(r: &agent_bin::ResolvedBin) -> String {
    let mut s = r.source.to_string();
    if !r.skipped_shims.is_empty() {
        let list: Vec<String> = r.skipped_shims.iter().map(|p| p.display().to_string()).collect();
        let what =
            if r.skipped_shims.iter().all(|p| p.to_string_lossy().contains("cmux")) { "cmux shim" } else { "shim" };
        s.push_str(&format!("; skipped {what} {}", list.join(", ")));
    }
    s
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
                source: Some(source_note(&r)),
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
        let bundle = codex_skill_bundle(req, &sink);
        let first_turn = codex_first_turn(req, bundle.as_ref(), &sink);
        let first_turn = first_turn.as_deref();
        let extra_args =
            codex_mcp_args(servers_of(req), &req.task.id, req.paths.home()).map_err(DriverError::Failed)?;
        let d = CodexDriver::spawn(
            &bin,
            req.task.cwd(),
            req.resume.as_deref(),
            ChildEnv { remove: &env_remove, set: &env_set, args: &extra_args, first_turn },
            &log,
            sink,
            CodexTiming::default(),
        )?;
        // Codex holds its stderr for the whole session: the log is cut while it runs too.
        watch_stderr_log(log, Arc::downgrade(&d));
        Ok(d)
    }
}

impl DriverFactory for RealFactory {
    fn check_agent(&self, agent: AgentKind) -> Result<(), String> {
        let agent = match agent {
            AgentKind::Claude => Agent::Claude,
            AgentKind::Codex => Agent::Codex,
        };
        agent_bin::resolve(agent, &Lookup::from_env(agent)).map(|_| ())
    }

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

    /// The memory server only.
    fn mem(bin: &Path) -> McpServers<'_> {
        McpServers { memory: Some(bin), ..McpServers::default() }
    }

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

    /// Codex keeps its stderr open for the whole session: a growing log is cut while it
    /// runs (copied to `.1`, then emptied in place; the agent's appends land at the start).
    #[test]
    fn a_running_agent_s_stderr_log_is_cut_in_place() {
        let d = TestDir::new();
        let log = d.path().join("sessions/t1/codex-stderr.log");
        prepare_append_log(&log, 100).unwrap();
        let mut agent = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        agent.write_all(&[b'x'; 150]).unwrap();
        assert!(!trim_log_if_over(&log, 200).unwrap());
        assert!(trim_log_if_over(&log, 100).unwrap());
        assert_eq!(std::fs::read(log.with_file_name("codex-stderr.log.1")).unwrap().len(), 150);
        agent.write_all(b"after\n").unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "after\n");
        assert_eq!(
            std::fs::metadata(log.with_file_name("codex-stderr.log.1")).unwrap().permissions().mode() & 0o777,
            0o600
        );
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
                workspace_path: None,
                group_path: None,
                group_id: None,
                group_role: None,
                parent_id: None,
                child_kind: None,
            },
            session_id: "sess-42".into(),
            resume: resume.map(String::from),
            paths: Paths::new(Path::new("/h")),
            mcp: None,
            skills_mcp: None,
            orchestrator: None,
            skills: None,
            withdrawn: Vec::new(),
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
        let a = codex_mcp_args(mem(Path::new("/opt/m/mirante-mcp")), "t1", Path::new("/h/.mirante")).unwrap();
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
        let a = codex_mcp_args(mem(Path::new("/x/o\"brien/mirante-mcp")), "t1", Path::new("/h")).unwrap();
        assert_eq!(a[1], "mcp_servers.mirante-memory.command=\"/x/o\\\"brien/mirante-mcp\"");
        assert!(codex_mcp_args(mem(Path::new("/x\ny/mirante-mcp")), "t1", Path::new("/h")).is_err());
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
    fn the_agents_report_names_a_shim_it_passed_over() {
        let d = TestDir::new();
        let mk = |dir: &Path| {
            std::fs::create_dir_all(dir).unwrap();
            let p = dir.join("claude");
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let shim = mk(&d.path().join(".cmuxterm/cmux-cli-shims/x"));
        let real = mk(&d.path().join("bin"));
        let path = std::env::join_paths([shim.parent().unwrap(), real.parent().unwrap()]).unwrap();
        let l = |_| mirante_core::agent_bin::Lookup {
            override_bin: None,
            path_var: Some(path.to_string_lossy().into_owned()),
            home: None,
            well_known: false,
        };
        let r = agents_report_with(l);
        assert_eq!(r[0].path.as_deref(), Some(real.to_str().unwrap()));
        assert_eq!(r[0].source.as_deref(), Some(format!("PATH; skipped cmux shim {}", shim.display()).as_str()));
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
    #[test]
    fn a_resumed_codex_thread_keeps_the_bundle_its_history_points_at() {
        let d = TestDir::new();
        let mut req = request(None);
        req.task.agent = AgentKind::Codex;
        req.paths = Paths::new(d.path());
        req.skills = Some(crate::driver::skill_bundle::tests::sample());
        let (tx, _rx) = std::sync::mpsc::channel();
        let sink = Sink::new("sess-42", tx);
        let first = codex_skill_bundle(&req, &sink).expect("a new thread gets its bundle");
        assert!(first.preamble.is_some());
        let mut resumed = request(Some("th-1"));
        resumed.task.agent = AgentKind::Codex;
        resumed.paths = Paths::new(d.path());
        resumed.session_id = "sess-43".into();
        assert_eq!(codex_skill_bundle(&resumed, &sink), None);
        assert!(first.dir.join("lookup-api/SKILL.md").exists(), "the first turn's paths still resolve");
    }
    /// A resumed thread whose skills were retired or changed: their files go, the first
    /// turn says not to follow them, and the feed gets a `skills_withdrawn` step.
    #[test]
    fn a_resumed_codex_thread_gets_its_withdrawals_as_the_first_turn() {
        let d = TestDir::new();
        let mut req = request(None);
        req.task.agent = AgentKind::Codex;
        req.paths = Paths::new(d.path());
        req.skills = Some(crate::driver::skill_bundle::tests::sample());
        let (tx, rx) = std::sync::mpsc::channel();
        let sink = Sink::new("sess-42", tx);
        let first = codex_skill_bundle(&req, &sink).unwrap();
        assert_eq!(codex_first_turn(&req, Some(&first), &sink).as_deref(), first.preamble.as_deref());
        let mut resumed = request(Some("th-1"));
        resumed.task.agent = AgentKind::Codex;
        resumed.paths = Paths::new(d.path());
        resumed.session_id = "sess-43".into();
        assert_eq!(codex_first_turn(&resumed, None, &sink), None, "nothing withdrawn: nothing sent");
        resumed.withdrawn = vec!["lookup-api".into()];
        let item = codex_first_turn(&resumed, None, &sink).expect("the withdrawal item");
        assert!(item.contains("- lookup-api\n"), "{item}");
        assert!(!first.dir.join("lookup-api").exists());
        let steps: Vec<serde_json::Value> = rx
            .try_iter()
            .filter_map(|(_, e)| match e {
                DriverEvent::Step(v) => Some(v),
                _ => None,
            })
            .collect();
        assert_eq!(steps, [serde_json::json!({"type": "skills_withdrawn", "names": ["lookup-api"]})]);
    }

    #[test]
    fn codex_mcp_args_cover_both_servers() {
        let bin = Path::new("/opt/m/mirante-mcp");
        let a = codex_mcp_args(
            McpServers { memory: Some(bin), skills: Some(bin), orchestrator: None },
            "t1",
            Path::new("/h"),
        )
        .unwrap();
        assert_eq!(a.len(), 12, "{a:?}");
        assert!(a.iter().step_by(2).all(|f| f == "-c"), "{a:?}");
        assert_eq!(a[7], "mcp_servers.mirante-skills.command=\"/opt/m/mirante-mcp\"");
        assert_eq!(a[9], "mcp_servers.mirante-skills.args=[\"--surface\",\"skills\"]");
        assert_eq!(
            a[11],
            "mcp_servers.mirante-skills.env={MIRANTE_TASK_ID=\"t1\",MIRANTE_HOME=\"/h\",MIRANTE_TASK=\"t1\"}"
        );
        let only =
            codex_mcp_args(McpServers { skills: Some(bin), ..McpServers::default() }, "t1", Path::new("/h")).unwrap();
        assert_eq!(only, a[6..]);
        assert!(codex_mcp_args(McpServers::default(), "t1", Path::new("/h")).unwrap().is_empty());
    }

    #[test]
    fn claude_gets_the_skills_server_without_memory_and_never_pre_allows_propose() {
        let d = TestDir::new();
        let mut req = request(None);
        req.paths = Paths::new(&d.path().join("home"));
        req.skills_mcp = Some(PathBuf::from("/opt/m/mirante-mcp"));
        let (settings, mcp) = write_claude_config(&req, Path::new("/opt/m/mirante-hook")).unwrap();
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(mcp.unwrap()).unwrap()).unwrap();
        assert_eq!(m["mcpServers"].as_object().unwrap().keys().collect::<Vec<_>>(), ["mirante-skills"]);
        assert_eq!(m["mcpServers"]["mirante-skills"]["args"], serde_json::json!(["--surface", "skills"]));
        let s: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(s["permissions"]["allow"], serde_json::json!([]));
        // Both on: one file, two servers; only memory's read tools are pre-allowed.
        req.mcp = req.skills_mcp.clone();
        let (settings, mcp) = write_claude_config(&req, Path::new("/opt/m/mirante-hook")).unwrap();
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(mcp.unwrap()).unwrap()).unwrap();
        assert_eq!(m["mcpServers"].as_object().unwrap().len(), 2);
        let s: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert!(!s["permissions"]["allow"].to_string().contains("skill"), "{s}");
    }

    #[test]
    fn codex_mcp_args_include_the_orchestrator_when_enabled() {
        let bin = Path::new("/opt/m/mirante-mcp");
        let off = codex_mcp_args(mem(bin), "t1", Path::new("/h")).unwrap();
        assert!(!off.iter().any(|a| a.contains("orchestrator")), "{off:?}");
        let on = McpServers { memory: Some(bin), skills: None, orchestrator: Some(bin) };
        let a = codex_mcp_args(on, "t1", Path::new("/h")).unwrap();
        assert_eq!(a.len(), 12, "{a:?}");
        assert_eq!(a[7], "mcp_servers.mirante-orchestrator.command=\"/opt/m/mirante-mcp\"");
        assert_eq!(a[9], "mcp_servers.mirante-orchestrator.args=[\"--surface\",\"orchestrator\"]");
        assert_eq!(
            a[11],
            "mcp_servers.mirante-orchestrator.env={MIRANTE_TASK_ID=\"t1\",MIRANTE_HOME=\"/h\",MIRANTE_TASK=\"t1\"}"
        );
    }

    #[test]
    fn claude_gets_the_orchestrator_with_its_read_tools_pre_allowed_and_never_task_spawn() {
        let d = TestDir::new();
        let mut req = request(None);
        req.paths = Paths::new(&d.path().join("home"));
        req.orchestrator = Some(PathBuf::from("/opt/m/mirante-mcp"));
        let (settings, mcp) = write_claude_config(&req, Path::new("/opt/m/mirante-hook")).unwrap();
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(mcp.unwrap()).unwrap()).unwrap();
        assert_eq!(m["mcpServers"].as_object().unwrap().keys().collect::<Vec<_>>(), ["mirante-orchestrator"]);
        assert_eq!(m["mcpServers"]["mirante-orchestrator"]["args"], serde_json::json!(["--surface", "orchestrator"]));
        let s: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            s["permissions"]["allow"],
            serde_json::json!(["mcp__mirante-orchestrator__task_children", "mcp__mirante-orchestrator__task_result"])
        );
        assert!(!s.to_string().contains("task_spawn"), "{s}");
        // Turned off before the next start: no stale server.
        req.orchestrator = None;
        let (_, mcp) = write_claude_config(&req, Path::new("/opt/m/mirante-hook")).unwrap();
        assert_eq!(mcp, None);
    }
}
