//! `shell.*`: a person's own shells beside a task (the cockpit's side pane Terminal), at most
//! [`MAX_PER_TASK`] per task. A person only (see [`crate::access`]).
//!
//! A shell is not an agent session. It lives in its own registry, never in `State::live`,
//! so it is never taken for one: nothing about it enters `task_of_tree` / `task_of_tty`,
//! the hook, the status engine, memguard's hibernation or auto-resume. Its environment
//! carries no agent marker ([`AGENT_ENV`], [`TASK_ENV`], every `MIRANTE_*`, a parent
//! agent's session variables). It starts in the task's worktree; where it goes from there
//! is the person's business. Nothing is ever typed into it but what the person types.
//!
//! [`Hub::identify`] is unchanged: the `mirante` CLI run from inside a worktree is
//! classified as an agent (worktrees are agent-written), from a shell too. Kept on purpose.
//!
//! Lifecycle: `shell.close`, archiving the task and daemon shutdown end it with everything
//! it started (`kill_tree`). A shell that exits is forgotten; shells do not survive a
//! daemon restart.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use mirante_core::claude_launch::{AGENT_ENV, NESTED_SESSION_ENV, SCRUB_PREFIXES, SESSION_ENV, TASK_ENV};
use mirante_core::model::Lifecycle;
use mirante_ipc::types::{ShellAttach, ShellOpen, ShellResize, ShellView};

use super::{Hub, HubError, Result};
use crate::driver::pty::{PtyProcess, PtySpec};

/// Shells one task may have at once.
pub const MAX_PER_TASK: usize = 4;
/// SIGTERM → SIGKILL grace when a shell is ended.
const KILL_GRACE: Duration = Duration::from_millis(300);
/// Typed bytes the shell must take within this long (else the attachment is dropped).
pub(crate) const INPUT_TIMEOUT: Duration = Duration::from_secs(5);
/// The shell when `$SHELL` is unset or not a login shell listed in `/etc/shells`.
pub const FALLBACK_SHELL: &str = "/bin/zsh";

pub(crate) struct Shell {
    task: String,
    n: u8,
    cwd: PathBuf,
    pub(crate) pty: Arc<PtyProcess>,
}

/// Running shells by id.
#[derive(Default)]
pub(crate) struct Registry {
    shells: HashMap<String, Shell>,
    /// Tests: the program to run instead of the person's shell.
    pub(crate) program: Option<PathBuf>,
}

impl Registry {
    /// Forgets shells that exited.
    fn prune(&mut self) {
        self.shells.retain(|_, s| !s.pty.exited());
    }
}

/// `$SHELL` when `/etc/shells` lists it (an absolute path), else [`FALLBACK_SHELL`].
pub fn pick_shell(env_shell: Option<&str>, etc_shells: &str) -> PathBuf {
    let listed = |s: &str| etc_shells.lines().map(str::trim).any(|l| !l.starts_with('#') && l == s);
    match env_shell {
        Some(s) if s.starts_with('/') && listed(s) => PathBuf::from(s),
        _ => PathBuf::from(FALLBACK_SHELL),
    }
}

/// Whether the daemon's variable `key` stays out of a person's shell: agent markers, every
/// `MIRANTE_*`, a parent agent's session variables and terminal-wrapper integrations.
pub fn scrubbed(key: &str) -> bool {
    key.starts_with("MIRANTE_")
        || [AGENT_ENV, TASK_ENV, SESSION_ENV].contains(&key)
        || NESTED_SESSION_ENV.contains(&key)
        || SCRUB_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// The shell's spawn: a login shell (`-l`) in `cwd`, without the scrubbed variables of
/// `daemon_env`, as an `xterm-256color` terminal.
pub fn shell_spec(
    program: &Path,
    cwd: &Path,
    rows: u16,
    cols: u16,
    daemon_env: impl Iterator<Item = String>,
) -> PtySpec {
    let mut env_remove: Vec<String> = daemon_env.filter(|k| scrubbed(k)).collect();
    for k in [AGENT_ENV, TASK_ENV, SESSION_ENV] {
        if !env_remove.iter().any(|x| x == k) {
            env_remove.push(k.to_string());
        }
    }
    PtySpec {
        program: program.to_path_buf(),
        args: vec!["-l".into()],
        cwd: cwd.to_path_buf(),
        env_remove,
        env_set: vec![("TERM".into(), "xterm-256color".into()), ("COLORTERM".into(), "truecolor".into())],
        rows: rows.max(10),
        cols: cols.max(40),
    }
}

fn view(id: &str, s: &Shell) -> ShellView {
    ShellView { shell: id.to_string(), task: s.task.clone(), n: s.n, cwd: s.cwd.to_string_lossy().into_owned() }
}

/// What `shell.attach` hands the server: the PTY and its client.
pub struct ShellStream {
    pub pty: Arc<PtyProcess>,
    pub id: u64,
    pub initial: Vec<u8>,
    pub output: mpsc::Receiver<Vec<u8>>,
}

impl Hub {
    fn shells(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.shells.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Tests: run `program` instead of the person's shell.
    pub fn set_shell_program(&self, program: &Path) {
        self.shells().program = Some(program.to_path_buf());
    }

    /// Where shell `p` of its task starts: the task's worktree (checked against a swap), a
    /// workspace task's repo worktree, or the workspace folder.
    fn shell_cwd(&self, p: &ShellOpen) -> Result<(String, PathBuf)> {
        let (t, repos) = {
            let st = self.lock();
            let t = st.resolve(&p.task)?;
            if matches!(t.lifecycle, Lifecycle::Archived | Lifecycle::Archiving) {
                return Err(HubError::Conflict(format!("task {} is archived", t.branch)));
            }
            let repos = if t.workspace_path.is_some() { st.store.task_repos(&t.id)? } else { Vec::new() };
            (t, repos)
        };
        let cwd = match (&t.workspace_path, &p.repo) {
            (None, _) => self.checked_worktree(&t).map_err(HubError::Conflict)?,
            (Some(ws), None) => PathBuf::from(ws),
            (Some(_), Some(name)) => {
                let r = self.checked_repos(&t, &repos).map_err(HubError::Conflict)?;
                let r = r
                    .into_iter()
                    .find(|r| &r.name == name)
                    .ok_or_else(|| HubError::NotFound(format!("task {} has no repo {name}", t.branch)))?;
                PathBuf::from(r.worktree_path)
            }
        };
        Ok((t.id, cwd))
    }

    /// `shell.open`: starts shell `n` of the task (refused while it runs, past
    /// [`MAX_PER_TASK`], or for an archived task).
    pub fn shell_open(&self, p: ShellOpen) -> Result<ShellView> {
        if p.n == 0 || usize::from(p.n) > MAX_PER_TASK {
            return Err(HubError::Invalid(format!("shell number {} is not 1..={MAX_PER_TASK}", p.n)));
        }
        if self.is_shutting_down() {
            return Err(HubError::Conflict("the daemon is shutting down".into()));
        }
        let (task, cwd) = self.shell_cwd(&p)?;
        let mut reg = self.shells();
        reg.prune();
        let mine: Vec<&Shell> = reg.shells.values().filter(|s| s.task == task).collect();
        if mine.iter().any(|s| s.n == p.n) {
            return Err(HubError::Conflict(format!("shell {} of this task is already running", p.n)));
        }
        if mine.len() >= MAX_PER_TASK {
            return Err(HubError::Conflict(format!("a task has at most {MAX_PER_TASK} shells")));
        }
        let program = match &reg.program {
            Some(p) => p.clone(),
            None => pick_shell(
                std::env::var("SHELL").ok().as_deref(),
                &std::fs::read_to_string("/etc/shells").unwrap_or_default(),
            ),
        };
        let keys = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
        let spec = shell_spec(&program, &cwd, p.rows, p.cols, keys);
        let pty = PtyProcess::spawn(&spec, |_| {})
            .map_err(|e| HubError::Invalid(format!("could not start {}: {e}", program.display())))?;
        let id = mirante_core::model::new_id();
        let s = Shell { task, n: p.n, cwd, pty };
        let v = view(&id, &s);
        reg.shells.insert(id, s);
        Ok(v)
    }

    /// `shell.list`: the task's running shells, by number.
    pub fn shell_list(&self, task: &str) -> Result<Vec<ShellView>> {
        let id = self.lock().resolve(task)?.id;
        let mut reg = self.shells();
        reg.prune();
        let mut v: Vec<ShellView> = reg.shells.iter().filter(|(_, s)| s.task == id).map(|(i, s)| view(i, s)).collect();
        v.sort_by_key(|s| s.n);
        Ok(v)
    }

    fn shell_pty(&self, id: &str) -> Result<Arc<PtyProcess>> {
        let mut reg = self.shells();
        reg.prune();
        reg.shells.get(id).map(|s| s.pty.clone()).ok_or_else(|| HubError::NotFound(format!("no shell {id}")))
    }

    /// `shell.attach`: a client of the shell's terminal (its scrollback and screen first).
    pub fn shell_attach(&self, p: &ShellAttach) -> Result<ShellStream> {
        let pty = self.shell_pty(&p.shell)?;
        if p.rows > 0 && p.cols > 0 {
            pty.resize(p.rows.max(10), p.cols.max(40)).map_err(|e| HubError::Invalid(e.to_string()))?;
        }
        let (id, initial, output) = pty.attach();
        Ok(ShellStream { pty, id, initial, output })
    }

    pub fn shell_resize(&self, p: &ShellResize) -> Result<()> {
        let pty = self.shell_pty(&p.shell)?;
        pty.resize(p.rows.max(10), p.cols.max(40)).map_err(|e| HubError::Invalid(e.to_string()))
    }

    /// `shell.close`: ends the shell and everything it started.
    pub fn shell_close(&self, id: &str) -> Result<()> {
        let s = self.shells().shells.remove(id).ok_or_else(|| HubError::NotFound(format!("no shell {id}")))?;
        s.pty.kill_tree(KILL_GRACE);
        Ok(())
    }

    /// Ends every shell of `task` (it is being archived).
    pub(crate) fn close_shells_of(&self, task: &str) {
        let gone: Vec<Shell> = {
            let mut reg = self.shells();
            let ids: Vec<String> = reg.shells.iter().filter(|(_, s)| s.task == task).map(|(i, _)| i.clone()).collect();
            ids.iter().filter_map(|i| reg.shells.remove(i)).collect()
        };
        for s in gone {
            s.pty.kill_tree(KILL_GRACE);
        }
    }

    /// Ends every shell (daemon shutdown).
    pub(crate) fn close_all_shells(&self) {
        let gone: Vec<Shell> = self.shells().shells.drain().map(|(_, s)| s).collect();
        for s in gone {
            s.pty.kill_tree(KILL_GRACE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use crate::hub::dna::Caller;
    use mirante_core::model::AgentKind;
    use std::time::Instant;

    fn open(k: &Kit, task: &str, n: u8) -> Result<ShellView> {
        k.hub.shell_open(ShellOpen { task: task.into(), n, repo: None, rows: 24, cols: 80 })
    }

    fn kit() -> Kit {
        let k = Kit::new();
        k.hub.set_shell_program(Path::new("/bin/sh"));
        k
    }

    /// Reads the shell's output until `want` shows (5 s at most).
    fn read_until(rx: &mpsc::Receiver<Vec<u8>>, want: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while !String::from_utf8_lossy(&got).contains(want) {
            let left = deadline.saturating_duration_since(Instant::now());
            got.extend(
                rx.recv_timeout(left).unwrap_or_else(|_| panic!("no {want:?} in {:?}", String::from_utf8_lossy(&got))),
            );
        }
        String::from_utf8_lossy(&got).into_owned()
    }

    #[test]
    fn shell_env_has_no_agent_markers() {
        let env = [
            "PATH",
            "HOME",
            "MIRANTE_HOME",
            "MIRANTE_TASK",
            "MIRANTE_TASK_ID",
            "MIRANTE_SESSION_ID",
            "MIRANTE_CLAUDE_INLINE_ANSWERS",
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CMUX_AGENT_X",
            "ANTHROPIC_API_KEY",
            "LANG",
        ];
        let spec = shell_spec(Path::new("/bin/zsh"), Path::new("/w"), 30, 100, env.iter().map(|s| s.to_string()));
        for k in [
            "MIRANTE_HOME",
            AGENT_ENV,
            TASK_ENV,
            SESSION_ENV,
            "MIRANTE_CLAUDE_INLINE_ANSWERS",
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "CMUX_AGENT_X",
        ] {
            assert!(spec.env_remove.iter().any(|x| x == k), "{k} is not removed");
        }
        for k in ["PATH", "HOME", "ANTHROPIC_API_KEY", "LANG"] {
            assert!(!spec.env_remove.iter().any(|x| x == k), "{k} is removed");
        }
        // Even when the daemon has none of them, the markers are removed.
        let bare = shell_spec(Path::new("/bin/zsh"), Path::new("/w"), 30, 100, std::iter::empty());
        assert_eq!(bare.env_remove, [AGENT_ENV, TASK_ENV, SESSION_ENV]);
        assert!(!spec.env_set.iter().any(|(k, _)| k.starts_with("MIRANTE")));
        assert!(spec.env_set.contains(&("TERM".into(), "xterm-256color".into())));
        assert_eq!(spec.args, ["-l"]);
        assert_eq!((spec.rows, spec.cols, spec.cwd.as_path()), (30, 100, Path::new("/w")));
    }

    #[test]
    fn shell_not_in_etc_shells_falls_back() {
        let etc = "# List of acceptable shells\n/bin/bash\n/bin/zsh\n/opt/homebrew/bin/fish\n";
        assert_eq!(pick_shell(Some("/opt/homebrew/bin/fish"), etc), Path::new("/opt/homebrew/bin/fish"));
        assert_eq!(pick_shell(Some("/tmp/evil"), etc), Path::new(FALLBACK_SHELL));
        assert_eq!(pick_shell(Some("bash"), "bash\n"), Path::new(FALLBACK_SHELL));
        assert_eq!(pick_shell(Some("# List of acceptable shells"), etc), Path::new(FALLBACK_SHELL));
        assert_eq!(pick_shell(None, etc), Path::new(FALLBACK_SHELL));
    }

    #[test]
    fn a_shell_starts_in_the_worktree_and_runs_what_the_person_types() {
        let k = kit();
        let t = k.create(AgentKind::Claude, "a").task;
        let s = open(&k, &t.id, 1).unwrap();
        let wt = std::fs::canonicalize(&t.worktree_path).unwrap();
        assert_eq!(std::fs::canonicalize(&s.cwd).unwrap(), wt);
        let a = k.hub.shell_attach(&ShellAttach { shell: s.shell.clone(), rows: 0, cols: 0 }).unwrap();
        // Quotes split the markers in the echoed command line: only the output matches.
        a.pty.write_within(b"echo D\"\"IR=$(pwd -P) M\"\"ARK=${MIRANTE_TASK:-none}.\n", INPUT_TIMEOUT).unwrap();
        let out = read_until(&a.output, "MARK=none.");
        assert!(out.contains(&format!("DIR={} ", wt.display())), "{out}");
        assert_eq!(k.hub.shell_list(&t.id).unwrap(), vec![s.clone()]);
        k.hub.shell_resize(&ShellResize { shell: s.shell.clone(), rows: 40, cols: 120 }).unwrap();
        assert_eq!(a.pty.size(), (40, 120));
        k.hub.shell_close(&s.shell).unwrap();
        assert!(a.pty.wait_exit(Duration::from_secs(5)));
        assert!(k.hub.shell_list(&t.id).unwrap().is_empty());
        assert!(k.hub.shell_close(&s.shell).is_err());
    }

    #[test]
    fn shell_is_not_a_live_session() {
        let k = kit();
        let t = k.create(AgentKind::Claude, "a").task;
        let s = open(&k, &t.id, 1).unwrap();
        let pty = k.hub.shell_pty(&s.shell).unwrap();
        let root = pty.root().expect("the shell's process");
        // Not a live session: no session, no view saying live, nothing the hub ties to a task
        // by process tree or terminal.
        assert!(k.hub.lock().live.is_empty());
        assert!(!k.hub.get_task(&t.id).unwrap().live);
        assert_eq!(k.hub.status().live_sessions, 0);
        assert_eq!(k.hub.task_of_tree(root), None);
        assert_eq!(k.hub.task_of_tty(root), None);
        // Its process carries no marker: what identify makes of it is decided by its cwd
        // alone (the worktree: an agent with no task, the documented limit), never Agent{task}.
        let who = k.hub.identify(&Caller::Peer(root));
        assert!(
            matches!(who, crate::hub::dna::Identity::Agent { task: None } | crate::hub::dna::Identity::Unverified(_)),
            "{who:?}"
        );
        k.hub.shell_close(&s.shell).unwrap();
    }

    #[test]
    fn fifth_shell_refused() {
        let k = kit();
        let t = k.create(AgentKind::Claude, "a").task;
        for n in 1..=4 {
            open(&k, &t.id, n).unwrap();
        }
        // Number 1 runs already; 5 and 0 are not numbers; a fifth is refused either way.
        assert!(matches!(open(&k, &t.id, 1), Err(HubError::Conflict(_))));
        assert!(matches!(open(&k, &t.id, 5), Err(HubError::Invalid(_))));
        assert!(matches!(open(&k, &t.id, 0), Err(HubError::Invalid(_))));
        assert_eq!(k.hub.shell_list(&t.id).unwrap().iter().map(|s| s.n).collect::<Vec<_>>(), [1, 2, 3, 4]);
        // Another task has its own four.
        let u = k.create(AgentKind::Claude, "b").task;
        open(&k, &u.id, 1).unwrap();
        k.hub.close_all_shells();
        assert!(k.hub.shell_list(&t.id).unwrap().is_empty());
    }

    #[test]
    fn archive_kills_shells() {
        let k = kit();
        let t = k.create(AgentKind::Claude, "a").task;
        let u = k.create(AgentKind::Claude, "b").task;
        let s = open(&k, &t.id, 1).unwrap();
        let other = open(&k, &u.id, 1).unwrap();
        let pty = k.hub.shell_pty(&s.shell).unwrap();
        k.hub.archive(&t.id).unwrap();
        assert!(pty.wait_exit(Duration::from_secs(5)), "the archived task's shell ended");
        assert!(k.hub.shell_pty(&s.shell).is_err());
        // Another task's shell keeps running; an archived task opens none.
        assert_eq!(k.hub.shell_list(&u.id).unwrap(), vec![other]);
        assert!(matches!(open(&k, &t.id, 2), Err(HubError::Conflict(_))));
        k.hub.close_all_shells();
    }

    #[test]
    fn daemon_shutdown_kills_shells() {
        let k = kit();
        let t = k.create(AgentKind::Claude, "a").task;
        let s = open(&k, &t.id, 1).unwrap();
        let pty = k.hub.shell_pty(&s.shell).unwrap();
        k.hub.shutdown();
        assert!(pty.wait_exit(Duration::from_secs(5)));
        assert!(open(&k, &t.id, 1).is_err());
    }

    #[test]
    fn a_shell_that_exits_is_forgotten() {
        let k = kit();
        let t = k.create(AgentKind::Claude, "a").task;
        let s = open(&k, &t.id, 1).unwrap();
        let a = k.hub.shell_attach(&ShellAttach { shell: s.shell.clone(), rows: 24, cols: 80 }).unwrap();
        a.pty.write_within(b"exit\n", INPUT_TIMEOUT).unwrap();
        assert!(a.pty.wait_exit(Duration::from_secs(5)));
        // Its stream ends.
        let deadline = Instant::now() + Duration::from_secs(5);
        while a.output.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok() {}
        while !k.hub.shell_list(&t.id).unwrap().is_empty() {
            assert!(Instant::now() < deadline, "the exited shell is still listed");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Its number is free again.
        open(&k, &t.id, 1).unwrap();
        k.hub.close_all_shells();
    }
}
