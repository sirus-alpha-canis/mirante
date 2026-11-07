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
//!   executes nothing. [`Git::checkout`] skips this step (see below);
//! - clears the `GIT_*` variables that redirect git or name programs, and sets
//!   [`ENV`] (no optional locks, no prompt, no pager, no system config, `LC_ALL=C`).
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
    "GIT_CONFIG",
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

/// One git invocation. Build with [`Git::new`] (or [`Git::checkout`], [`Git::global`]),
/// add arguments, then run.
#[derive(Debug, Clone)]
pub struct Git {
    dir: Option<PathBuf>,
    /// Extra `-c` overrides (the neutralized filters).
    overrides: Vec<String>,
    args: Vec<OsString>,
}

impl Git {
    /// git in `dir`, with the filters `dir`'s config names neutralized.
    pub fn new(dir: &Path) -> Git {
        Git::checkout(dir).neutralize_filters_of(dir)
    }

    /// git in `dir` with the generic hardening only: filters stay (`git worktree add`).
    pub fn checkout(dir: &Path) -> Git {
        Git { dir: Some(dir.to_path_buf()), overrides: Vec::new(), args: Vec::new() }
    }

    /// git outside any repository (e.g. `check-ref-format`).
    pub fn global() -> Git {
        Git { dir: None, overrides: Vec::new(), args: Vec::new() }
    }

    /// Also neutralizes the filters named by `other`'s config (a worktree with its own
    /// `config.worktree`).
    pub fn neutralize_filters_of(mut self, other: &Path) -> Git {
        for name in filter_names(other) {
            for key in ["clean", "smudge", "process"] {
                self.overrides.push(format!("filter.{name}.{key}="));
            }
            self.overrides.push(format!("filter.{name}.required=false"));
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

    /// The hardened [`Command`], stdin closed.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        for var in ENV_CLEARED {
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

    pub fn output(&self) -> io::Result<Output> {
        self.command().output()
    }
}

/// Names of the filter drivers `dir`'s config defines (any scope git reads there).
fn filter_names(dir: &Path) -> Vec<String> {
    let out = Git::checkout(dir).args(["config", "-z", "--name-only", "--get-regexp", r"^filter\."]).output();
    let Ok(out) = out else {
        return Vec::new();
    };
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
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn plain(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(ok.status.success(), "git {args:?}");
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
        assert!(filter_names(dir.path()).iter().any(|n| n == "evil"));
        assert!(filter_names(dir.path()).iter().any(|n| n == "a.b"));
        let out = Git::new(dir.path()).args(["config", "--get", "filter.evil.clean"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "");
        let out = Git::new(dir.path()).args(["config", "--get", "filter.a.b.required"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
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
}
