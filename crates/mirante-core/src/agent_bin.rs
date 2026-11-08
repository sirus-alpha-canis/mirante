//! Finding the real agent binaries (`claude`, `codex`) whatever environment the daemon was
//! started from. A daemon started by the desktop app may have macOS's minimal PATH, so after
//! the explicit override and PATH come the directories installers use:
//!
//! 1. `MIRANTE_CLAUDE_BIN` / `MIRANTE_CODEX_BIN` (must be an executable file);
//! 2. the first match in PATH;
//! 3. well-known directories: `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`,
//!    `~/.cargo/bin`, then (Claude) `~/.claude/local`, then nvm's `~/.nvm/versions/node/*/bin`,
//!    newest Node version first (npm installs `codex`, and sometimes `claude`, there).
//!
//! In steps 2 and 3 a candidate inside (or resolving into) a known wrapper-shim directory is
//! skipped: a terminal app's shim would inject its own hooks into the agent.
//!
//! An npm-installed agent is a `#!/usr/bin/env node` script, so when the binary's directory
//! is not on PATH the agent's PATH gets that directory first ([`child_path`]): nvm keeps
//! `node` next to it.
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// PATH entries that hold wrapper shims rather than the real agent binaries.
pub const KNOWN_SHIM_DIRS: [&str; 1] = [".cmuxterm/cmux-cli-shims"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn exe(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    /// The variable that names the binary explicitly.
    pub fn env_var(self) -> &'static str {
        match self {
            Agent::Claude => "MIRANTE_CLAUDE_BIN",
            Agent::Codex => "MIRANTE_CODEX_BIN",
        }
    }
}

/// Where a binary was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinSource {
    /// `MIRANTE_CLAUDE_BIN` / `MIRANTE_CODEX_BIN`.
    Override(&'static str),
    Path,
    /// A well-known install directory (not on PATH).
    WellKnown,
}

impl fmt::Display for BinSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinSource::Override(var) => write!(f, "{var}"),
            BinSource::Path => write!(f, "PATH"),
            BinSource::WellKnown => write!(f, "well-known directory"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBin {
    pub path: PathBuf,
    pub source: BinSource,
    /// Executables passed over because they are (or resolve into) a wrapper shim, in the
    /// order they were met (`mirante doctor` names them).
    pub skipped_shims: Vec<PathBuf>,
}

/// What resolution looks at; the process's own values in [`Lookup::from_env`].
#[derive(Debug, Clone, Default)]
pub struct Lookup {
    pub override_bin: Option<String>,
    pub path_var: Option<String>,
    pub home: Option<PathBuf>,
    /// Also search the well-known install directories (step 3).
    pub well_known: bool,
}

impl Lookup {
    pub fn from_env(agent: Agent) -> Lookup {
        Lookup {
            override_bin: std::env::var(agent.env_var()).ok(),
            path_var: std::env::var("PATH").ok(),
            home: std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from),
            well_known: true,
        }
    }
}

pub fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

pub fn is_shim_dir(dir: &Path) -> bool {
    let s = dir.to_string_lossy();
    KNOWN_SHIM_DIRS.iter().any(|shim| s.contains(shim))
}

/// True when `bin`, after following symlinks, lives in a shim directory (a symlink in a
/// clean directory can still point at a wrapper). Unresolvable paths count as shims.
pub fn resolves_into_shim_dir(bin: &Path) -> bool {
    match std::fs::canonicalize(bin) {
        Ok(real) => real.parent().is_none_or(is_shim_dir),
        Err(_) => true,
    }
}

/// `~/.nvm/versions/node/*/bin`, newest Node version first (`v22.14.0` before `v18.20.3`).
pub fn nvm_bin_dirs(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".nvm/versions/node");
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut versions: Vec<(Vec<u64>, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let v = name.strip_prefix('v')?.split('.').map(|n| n.parse::<u64>().ok()).collect::<Option<Vec<_>>>()?;
            Some((v, e.path().join("bin")))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, dir)| dir).collect()
}

/// The well-known install directories for `agent`, in search order.
pub fn well_known_dirs(agent: Agent, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".local/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    if let Some(h) = home {
        dirs.push(h.join(".cargo/bin"));
        if agent == Agent::Claude {
            dirs.push(h.join(".claude/local"));
        }
        dirs.extend(nvm_bin_dirs(h));
    }
    dirs
}

fn usable(candidate: &Path) -> bool {
    candidate.parent().is_some_and(|d| !is_shim_dir(d))
        && is_executable(candidate)
        && !resolves_into_shim_dir(candidate)
}

/// The real binary for `agent` (see the module docs for the order). The error says what was
/// tried.
pub fn resolve(agent: Agent, l: &Lookup) -> Result<ResolvedBin, String> {
    let var = agent.env_var();
    if let Some(bin) = l.override_bin.as_deref().filter(|b| !b.is_empty()) {
        let p = PathBuf::from(bin);
        return if is_executable(&p) {
            Ok(ResolvedBin { path: p, source: BinSource::Override(var), skipped_shims: Vec::new() })
        } else {
            Err(format!("{var}={bin} is not an executable file"))
        };
    }
    let path_var = l.path_var.as_deref().unwrap_or_default();
    let mut skipped_shims = Vec::new();
    let mut pick = |p: &PathBuf| {
        if usable(p) {
            return true;
        }
        if is_executable(p) && !skipped_shims.contains(p) {
            skipped_shims.push(p.clone());
        }
        false
    };
    if let Some(p) = std::env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(agent.exe()))
        .find(&mut pick)
    {
        return Ok(ResolvedBin { path: p, source: BinSource::Path, skipped_shims });
    }
    let well_known = if l.well_known { well_known_dirs(agent, l.home.as_deref()) } else { Vec::new() };
    if let Some(p) = well_known.into_iter().map(|d| d.join(agent.exe())).find(&mut pick) {
        return Ok(ResolvedBin { path: p, source: BinSource::WellKnown, skipped_shims });
    }
    Err(format!(
        "no {exe} outside shim dirs in PATH={path_var} or the usual install directories (~/.local/bin, \
         /opt/homebrew/bin, /usr/local/bin, ~/.cargo/bin, ~/.nvm/versions/node/*/bin); set {var}",
        exe = agent.exe()
    ))
}

/// The PATH an agent at `bin` should run with: `path_var` with `bin`'s directory first when
/// it is not already there (an npm-installed agent needs the `node` next to it), else
/// `None` (keep the inherited PATH). The directory is the one `bin` was found in, not where
/// a symlink points (nvm's `bin/codex` links into `lib/node_modules`).
pub fn child_path(bin: &Path, path_var: Option<&str>) -> Option<String> {
    let dir = bin.parent().filter(|d| !d.as_os_str().is_empty())?;
    let path_var = path_var.unwrap_or_default();
    if std::env::split_paths(path_var).any(|p| p == dir) {
        return None;
    }
    let mut parts = vec![dir.to_path_buf()];
    parts.extend(std::env::split_paths(path_var).filter(|p| !p.as_os_str().is_empty()));
    std::env::join_paths(parts).ok().and_then(|s| s.into_string().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn bin(dir: &Path, name: &str, mode: u32) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    fn lookup(over: Option<&Path>, path: &[&Path], home: &Path) -> Lookup {
        Lookup {
            override_bin: over.map(|p| p.to_string_lossy().into_owned()),
            path_var: Some(std::env::join_paths(path).unwrap().into_string().unwrap()),
            home: Some(home.to_path_buf()),
            well_known: true,
        }
    }

    #[test]
    fn order_is_override_then_path_then_well_known() {
        let d = TempDir::new("agent-order");
        let home = d.path().join("home");
        let local = bin(&home.join(".local/bin"), "claude", 0o755);
        let on_path = bin(&d.path().join("p"), "claude", 0o755);
        let custom = bin(&d.path().join("c"), "my-claude", 0o755);

        let r = resolve(Agent::Claude, &lookup(Some(&custom), &[&d.path().join("p")], &home)).unwrap();
        assert_eq!((r.path, r.source), (custom, BinSource::Override("MIRANTE_CLAUDE_BIN")));
        let r = resolve(Agent::Claude, &lookup(None, &[&d.path().join("p")], &home)).unwrap();
        assert_eq!((r.path, r.source), (on_path, BinSource::Path));
        // A Finder-launched daemon: PATH without the user's dirs.
        let r = resolve(Agent::Claude, &lookup(None, &[&d.path().join("empty")], &home)).unwrap();
        assert_eq!((r.path, r.source), (local, BinSource::WellKnown));
    }

    #[test]
    fn a_bad_override_is_an_error_not_a_fallback() {
        let d = TempDir::new("agent-override");
        let home = d.path().join("home");
        bin(&home.join(".local/bin"), "codex", 0o755);
        let bad = bin(d.path(), "codex-noexec", 0o644);
        let e = resolve(Agent::Codex, &lookup(Some(&bad), &[], &home)).unwrap_err();
        assert!(e.contains("MIRANTE_CODEX_BIN=") && e.contains("not an executable"), "{e}");
        let e =
            resolve(Agent::Codex, &Lookup { home: Some(d.path().join("nobody")), ..Lookup::default() }).unwrap_err();
        assert!(e.contains("no codex") && e.contains("MIRANTE_CODEX_BIN"), "{e}");
    }

    #[test]
    fn shims_are_skipped_on_path_and_by_symlink() {
        let d = TempDir::new("agent-shim");
        let home = d.path().join("home");
        let shim = home.join(".cmuxterm/cmux-cli-shims/abc");
        bin(&shim, "claude", 0o755);
        let linked = d.path().join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(shim.join("claude"), linked.join("claude")).unwrap();
        let real = bin(&home.join(".local/bin"), "claude", 0o755);
        let r = resolve(Agent::Claude, &lookup(None, &[&shim, &linked], &home)).unwrap();
        assert_eq!((r.path, r.source), (real, BinSource::WellKnown));
        assert_eq!(r.skipped_shims, vec![shim.join("claude"), linked.join("claude")]);
    }

    #[test]
    fn codex_under_nvm_is_found_in_the_newest_node_that_has_it() {
        let d = TempDir::new("agent-nvm");
        let home = d.path().join("home");
        let nvm = home.join(".nvm/versions/node");
        bin(&nvm.join("v9.11.2/bin"), "codex", 0o755);
        let want = bin(&nvm.join("v22.14.0/bin"), "codex", 0o755);
        bin(&nvm.join("v18.20.3/bin"), "codex", 0o755);
        // The newest Node has no codex installed.
        std::fs::create_dir_all(nvm.join("v24.1.0/bin")).unwrap();
        std::fs::create_dir_all(nvm.join("system")).unwrap();
        assert_eq!(nvm_bin_dirs(&home)[0], nvm.join("v24.1.0/bin"));
        let r = resolve(Agent::Codex, &lookup(None, &[&d.path().join("none")], &home)).unwrap();
        assert_eq!((r.path, r.source), (want, BinSource::WellKnown));
    }

    #[test]
    fn well_known_order() {
        let h = Path::new("/h");
        let dirs = well_known_dirs(Agent::Claude, Some(h));
        let want: Vec<PathBuf> =
            ["/h/.local/bin", "/opt/homebrew/bin", "/usr/local/bin", "/h/.cargo/bin", "/h/.claude/local"]
                .iter()
                .map(PathBuf::from)
                .collect();
        assert_eq!(dirs, want);
        assert!(!well_known_dirs(Agent::Codex, Some(h)).contains(&PathBuf::from("/h/.claude/local")));
    }

    #[test]
    fn child_path_puts_the_binary_dir_first_only_when_missing() {
        let nvm = Path::new("/h/.nvm/versions/node/v22.14.0/bin/codex");
        assert_eq!(
            child_path(nvm, Some("/usr/bin:/bin")).as_deref(),
            Some("/h/.nvm/versions/node/v22.14.0/bin:/usr/bin:/bin")
        );
        assert_eq!(child_path(Path::new("/usr/bin/codex"), Some("/usr/bin:/bin")), None);
        assert_eq!(child_path(nvm, None).as_deref(), Some("/h/.nvm/versions/node/v22.14.0/bin"));
    }
}
