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
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mirante_core::agent_bin::{self, child_path, Agent, Lookup};
use mirante_core::claude_launch::{launch_spec, settings_json, LaunchInput};
use mirante_core::model::AgentKind;
use mirante_ipc::paths::HOME_ENV;
use mirante_ipc::types::AgentBinView;

use super::claude::{ClaudeDriver, ClaudeTiming};
use super::codex::{codex_env_to_scrub, ChildEnv, CodexDriver, CodexTiming};
use super::pty::PtySpec;
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

/// The PTY launch for `req`, and the native session id pinned for a fresh start
/// (`None` when resuming: the id to resume is `req.resume`).
pub fn claude_spec(
    req: &SpawnRequest,
    claude: &Path,
    settings: &Path,
    size: (u16, u16),
    parent_env: Vec<String>,
) -> std::io::Result<(PtySpec, Option<String>)> {
    let spec = launch_spec(LaunchInput {
        claude_bin: claude,
        worktree: Path::new(&req.task.worktree_path),
        settings_path: settings,
        task_id: &req.task.id,
        session_id: &req.session_id,
        resume: req.resume.as_deref(),
        parent_env,
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
        let settings = req.paths.session_dir(&req.task.id).join("claude-settings.json");
        write_private(&settings, &settings_json(&hook.to_string_lossy(), &[]).to_string())?;
        let (mut pty, pinned) = claude_spec(req, &claude, &settings, (self.rows, self.cols), parent_env())?;
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
        let env_set = agent_path_env(&bin, std::env::var("PATH").ok().as_deref());
        let log = req.paths.session_dir(&req.task.id).join("codex-stderr.log");
        // Appended, never truncated: the last session's stderr explains its crash.
        prepare_append_log(&log, STDERR_LOG_CAP)?;
        let env_remove = codex_env_to_scrub(parent_env());
        let d = CodexDriver::spawn(
            &bin,
            &req.task.worktree_path,
            req.resume.as_deref(),
            ChildEnv { remove: &env_remove, set: &env_set },
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
        }
    }

    fn env<'a>(spec: &'a PtySpec, key: &str) -> Option<&'a str> {
        spec.env_set.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_fresh_launch_pins_the_session_id_and_stamps_the_mirante_session() {
        let parent = vec!["CLAUDECODE".to_string(), "ANTHROPIC_API_KEY".to_string()];
        let (spec, pinned) =
            claude_spec(&request(None), Path::new("/bin/claude"), Path::new("/h/s.json"), (40, 120), parent).unwrap();
        let id = pinned.expect("fresh start pins an id");
        let args = spec.args.join(" ");
        assert!(args.contains(&format!("--session-id {id}")), "{args}");
        assert!(args.contains("--permission-mode manual") && args.contains("--settings /h/s.json"), "{args}");
        assert!(!args.contains("--resume") && !args.contains("dangerously"), "{args}");
        assert_eq!(env(&spec, "MIRANTE_SESSION_ID"), Some("sess-42"));
        assert_eq!(env(&spec, "MIRANTE_TASK_ID"), Some("task-1"));
        assert_eq!(env(&spec, "MIRANTE_HOME"), Some("/h"));
        assert_eq!(env(&spec, "TERM"), Some("xterm-256color"));
        assert!(spec.env_remove.contains(&"CLAUDECODE".to_string()));
        assert!(!spec.env_remove.iter().any(|k| k.starts_with("ANTHROPIC_")));
        assert_eq!((spec.cwd.as_path(), spec.rows, spec.cols), (Path::new("/w/tree"), 40, 120));
    }

    #[test]
    fn a_resume_passes_the_native_id_and_pins_nothing() {
        let (spec, pinned) =
            claude_spec(&request(Some("nat-1")), Path::new("/bin/claude"), Path::new("/h/s.json"), (40, 120), vec![])
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
}
