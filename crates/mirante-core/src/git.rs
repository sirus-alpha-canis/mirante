//! The one way the daemon runs `git`. Task worktrees are written by agents, and the daemon
//! runs git there outside the agents' sandbox, as the user: a prompt-injected agent could
//! plant repo config or attributes that make git execute a command (`core.fsmonitor`,
//! hooks, clean/smudge filters, diff drivers, `diff.external`, a pager, an ssh command, a
//! credential helper). Every invocation therefore goes through [`Git`], which:
//!
//! - prefixes [`HARDENING`] (`-c` overrides beat every config file, and git passes them on
//!   to the git processes it spawns itself, e.g. the `status` inside `worktree remove`);
//! - neutralizes every filter driver the repository's config names (`filter.<x>.clean`,
//!   `.smudge`, `.process` set to empty, `.required=false`), read with `git config`, which
//!   executes nothing, within a time limit: when they cannot be read the invocation refuses
//!   to run (fail closed). [`Git::checkout`] skips this step (see below);
//! - clears the `GIT_*` variables that redirect git or name programs, and sets
//!   [`ENV`] (no optional locks, no prompt, no pager, no system config, `LC_ALL=C`, and
//!   no replace refs or grafts: an agent can plant `refs/replace/*` or `info/grafts` in the
//!   shared repository to make one commit read as another, so a stale anchor would read
//!   as fresh and a task diff would hide changes).
//!
//! Diff commands add `--no-ext-diff --no-textconv` themselves (see the daemon's `diff`).
//!
//! Residual risk, by design: `git worktree add` ([`Git::checkout`]) keeps filters, so a
//! repository using Git LFS still checks out real files. It runs the smudge filters of the
//! repository's config on the base commit's attributes: the user's own setup, unless an
//! agent earlier rewrote the shared `.git/config` and the new task's base carries
//! attributes it committed. Hooks are off there too. Submodules' own config is not scanned
//! for filter names.
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// `-c` overrides on every git invocation. Each key is valid on git 2.44.
pub const HARDENING: &[&str] = &[
    "core.fsmonitor=false",
    "core.useReplaceRefs=false",
    "core.pager=cat",
    "color.ui=never",
    "diff.external=",
    "core.hooksPath=/dev/null",
    "core.sshCommand=",
    "credential.helper=",
    "protocol.allow=never",
    "filter.lfs.process=",
    "submodule.recurse=false",
];

/// Environment set on every git invocation.
pub const ENV: &[(&str, &str)] = &[
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_PAGER", "cat"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_NO_REPLACE_OBJECTS", "1"),
    ("GIT_GRAFT_FILE", "/dev/null"),
    ("LC_ALL", "C"),
];

/// Inherited variables that would point git at another repository, inject config, or name
/// a program to run. Removed from every invocation.
const ENV_CLEARED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_REPLACE_REF_BASE",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_ATTR_SOURCE",
    "GIT_EXTERNAL_DIFF",
    "GIT_DIFF_OPTS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PROXY_COMMAND",
];

/// The inherited `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>` among `names` (git ignores
/// them once `GIT_CONFIG_COUNT` is gone; they are removed anyway).
fn indexed_config_vars(names: impl Iterator<Item = OsString>) -> Vec<OsString> {
    names
        .filter(|n| n.to_str().is_some_and(|n| n.starts_with("GIT_CONFIG_KEY_") || n.starts_with("GIT_CONFIG_VALUE_")))
        .collect()
}

/// One git invocation. Build with [`Git::new`] (or [`Git::checkout`], [`Git::global`]),
/// add arguments, then run.
#[derive(Debug, Clone)]
pub struct Git {
    dir: Option<PathBuf>,
    /// Extra `-c` overrides (the neutralized filters).
    overrides: Vec<String>,
    args: Vec<OsString>,
    /// Why this invocation must not run: the repository's filters could not be listed (in
    /// time), so they could not be neutralized.
    refused: Option<String>,
}

/// Longest the filter listing (`git config`, which executes nothing) may take.
const FILTER_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

impl Git {
    /// git in `dir`, with the filters `dir`'s config names neutralized.
    pub fn new(dir: &Path) -> Git {
        Git::checkout(dir).neutralize_filters_of(dir)
    }

    /// git in `dir` with the generic hardening only: filters stay (`git worktree add`).
    pub fn checkout(dir: &Path) -> Git {
        Git { dir: Some(dir.to_path_buf()), overrides: Vec::new(), args: Vec::new(), refused: None }
    }

    /// git outside any repository (e.g. `check-ref-format`).
    pub fn global() -> Git {
        Git { dir: None, overrides: Vec::new(), args: Vec::new(), refused: None }
    }

    /// Also neutralizes the filters named by `other`'s config (a worktree with its own
    /// `config.worktree`).
    pub fn neutralize_filters_of(self, other: &Path) -> Git {
        self.neutralize_within(other, FILTER_LIMIT)
    }

    /// [`Git::neutralize_filters_of`] with the listing limited to `limit`. A listing that
    /// times out leaves the filters unknown: this invocation then refuses to run.
    pub fn neutralize_within(mut self, other: &Path, limit: std::time::Duration) -> Git {
        match filter_names_within(other, limit) {
            Ok(names) => {
                for name in names {
                    for key in ["clean", "smudge", "process"] {
                        self.overrides.push(format!("filter.{name}.{key}="));
                    }
                    self.overrides.push(format!("filter.{name}.required=false"));
                }
            }
            Err(e) => self.refused = Some(format!("the repository's filter config could not be read: {e}")),
        }
        self
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Git {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Git
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// The hardened [`Command`], stdin closed. Refused (see [`Git::neutralize_within`]):
    /// a git that only rejects an unknown option and exits, reading no config.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        if self.refused.is_some() {
            cmd.envs(ENV.iter().copied()).arg("--mirante-refused-unknown-filters").stdin(Stdio::null());
            return cmd;
        }
        for var in ENV_CLEARED {
            cmd.env_remove(var);
        }
        for var in indexed_config_vars(std::env::vars_os().map(|(k, _)| k)) {
            cmd.env_remove(var);
        }
        cmd.envs(ENV.iter().copied());
        if let Some(dir) = &self.dir {
            cmd.arg("-C").arg(dir);
        }
        for o in HARDENING.iter().map(|s| s.to_string()).chain(self.overrides.iter().cloned()) {
            cmd.arg("-c").arg(o);
        }
        cmd.args(&self.args).stdin(Stdio::null());
        cmd
    }

    fn check_refused(&self) -> io::Result<()> {
        match &self.refused {
            Some(why) => Err(io::Error::other(why.clone())),
            None => Ok(()),
        }
    }

    pub fn output(&self) -> io::Result<Output> {
        self.check_refused()?;
        self.command().output()
    }

    /// [`Git::output`], killed after `limit` (`ErrorKind::TimedOut`): for reads a person
    /// waits on (a huge or hung repository must not hold the caller forever).
    pub fn output_timeout(&self, limit: std::time::Duration) -> io::Result<Output> {
        use std::io::Read;
        self.check_refused()?;
        let mut child = self.command().stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let drain = |r: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut r) = r {
                    let _ = r.read_to_end(&mut buf);
                }
                buf
            })
        };
        let out = drain(child.stdout.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
        let err = drain(child.stderr.take().map(|r| Box::new(r) as Box<dyn Read + Send>));
        let deadline = std::time::Instant::now() + limit;
        // Short polls first: most git reads end within a few milliseconds.
        let mut pause = std::time::Duration::from_micros(250);
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("git took longer than {limit:?}")));
            }
            std::thread::sleep(pause);
            pause = (pause * 2).min(std::time::Duration::from_millis(5));
        };
        Ok(Output { status, stdout: out.join().unwrap_or_default(), stderr: err.join().unwrap_or_default() })
    }
}

/// Names of the filter drivers `dir`'s config defines (any scope git reads there), read
/// within `limit`: `Err` when git could not be run or took longer.
fn filter_names_within(dir: &Path, limit: std::time::Duration) -> io::Result<Vec<String>> {
    let out =
        Git::checkout(dir).args(["config", "-z", "--name-only", "--get-regexp", r"^filter\."]).output_timeout(limit)?;
    let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter_map(|key| {
            let rest = key.strip_prefix("filter.")?;
            let (name, _) = rest.rsplit_once('.')?;
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn plain(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(ok.status.success(), "git {args:?}");
    }

    /// The filter listing is time-limited; when it cannot be read, the repository's filters
    /// are unknown, so nothing is run there (fail closed).
    #[test]
    fn a_filter_listing_that_times_out_refuses_to_run_git() {
        let d = TempDir::new("git-filters");
        plain(d.path(), &["init", "-q"]);
        assert!(filter_names_within(d.path(), std::time::Duration::ZERO).is_err());
        let g = Git::checkout(d.path()).neutralize_within(d.path(), std::time::Duration::ZERO).args(["status"]);
        let e = g.output().unwrap_err();
        assert!(e.to_string().contains("filter"), "{e}");
        assert!(g.output_timeout(std::time::Duration::from_secs(5)).is_err());
        // A direct command runs nothing either (git refuses an unknown option).
        let out = g.command().output().unwrap();
        assert!(!out.status.success());
        // In time: it runs.
        let ok = Git::new(d.path()).args(["status"]).output().unwrap();
        assert!(ok.status.success());
    }

    #[test]
    fn output_timeout_kills_a_slow_git_and_returns_a_fast_one() {
        let ok = Git::global().arg("version").output_timeout(std::time::Duration::from_secs(20)).unwrap();
        assert!(ok.status.success() && String::from_utf8_lossy(&ok.stdout).starts_with("git version"));
        // A zero limit stands for a git that takes too long: it is killed, never awaited.
        let e = Git::global().arg("version").output_timeout(std::time::Duration::ZERO).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn every_hardening_key_is_accepted_by_git() {
        let out = Git::global().args(["config", "--get", "core.hooksPath"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "/dev/null");
        let ver = Git::global().arg("version").output().unwrap();
        assert!(ver.status.success(), "{}", String::from_utf8_lossy(&ver.stderr));
    }

    #[test]
    fn filters_named_in_config_are_neutralized() {
        let dir = TempDir::new("git-filters");
        plain(dir.path(), &["init", "-q"]);
        plain(dir.path(), &["config", "filter.evil.clean", "touch PWNED"]);
        plain(dir.path(), &["config", "filter.a.b.smudge", "touch PWNED"]);
        assert!(filter_names_within(dir.path(), FILTER_LIMIT).unwrap().iter().any(|n| n == "evil"));
        assert!(filter_names_within(dir.path(), FILTER_LIMIT).unwrap().iter().any(|n| n == "a.b"));
        let out = Git::new(dir.path()).args(["config", "--get", "filter.evil.clean"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "");
        let out = Git::new(dir.path()).args(["config", "--get", "filter.a.b.required"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
    }

    #[test]
    fn replace_refs_and_grafts_are_ignored() {
        let dir = TempDir::new("git-replace");
        let c = |args: &[&str]| plain(dir.path(), &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat());
        plain(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("a"), "old\n").unwrap();
        c(&["add", "a"]);
        c(&["commit", "-q", "-m", "old"]);
        std::fs::write(dir.path().join("a"), "new\n").unwrap();
        c(&["commit", "-q", "-am", "new"]);
        // A planted replace ref makes HEAD read as its parent to plain git.
        c(&["replace", "HEAD", "HEAD~1"]);
        let shown = Git::new(dir.path()).args(["cat-file", "-p", "HEAD:a"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&shown.stdout), "new\n");
        let diff = Git::new(dir.path()).args(["diff", "--name-only", "HEAD~1", "HEAD"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&diff.stdout).trim(), "a");
        // A planted graft file is ignored too: HEAD keeps its parent.
        std::fs::create_dir_all(dir.path().join(".git/info")).unwrap();
        let head = Git::new(dir.path()).args(["rev-parse", "HEAD"]).output().unwrap();
        std::fs::write(dir.path().join(".git/info/grafts"), head.stdout).unwrap();
        let count = Git::new(dir.path()).args(["rev-list", "--count", "HEAD"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2");
        let cmd = Git::global().command();
        assert!(cmd.get_envs().any(|(k, v)| k == "GIT_REPLACE_REF_BASE" && v.is_none()));
    }

    #[test]
    fn inherited_git_dir_is_ignored() {
        let dir = TempDir::new("git-env");
        plain(dir.path(), &["init", "-q", "-b", "trunk"]);
        let cmd = Git::new(dir.path()).args(["symbolic-ref", "--short", "HEAD"]).command();
        let cleared: Vec<_> = cmd.get_envs().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_owned()).collect();
        assert!(cleared.iter().any(|k| k == "GIT_DIR") && cleared.iter().any(|k| k == "GIT_EXTERNAL_DIFF"));
        assert!(cmd.get_envs().any(|(k, v)| k == "GIT_OPTIONAL_LOCKS" && v == Some(OsStr::new("0"))));
    }

    #[test]
    fn inherited_config_files_and_indexed_config_are_cleared() {
        let cmd = Git::global().command();
        let cleared: Vec<_> = cmd.get_envs().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_owned()).collect();
        for var in ["GIT_CONFIG_GLOBAL", "GIT_CONFIG_SYSTEM", "GIT_CONFIG_COUNT"] {
            assert!(cleared.iter().any(|k| k == var), "{var} is inherited");
        }
        let env = ["PATH", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0", "GIT_CONFIG_KEY_12", "GIT_CONFIG_NOSYSTEM"];
        let indexed = indexed_config_vars(env.iter().map(OsString::from));
        assert_eq!(indexed, ["GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0", "GIT_CONFIG_KEY_12"]);
    }
}
