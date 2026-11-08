//! The user's login-shell environment, for a daemon started from the desktop app.
//!
//! An app opened from Finder or the Dock gets macOS's minimal PATH and none of the user's
//! exports, so a daemon it starts cannot find `claude`, `codex` or `node`. Once per app run
//! the user's shell (`$SHELL`, only when listed in `/etc/shells`, else `/bin/zsh`) runs as a
//! login, interactive shell (`-l -i -c`, so `.zprofile` and `.zshrc` both apply) with stdin
//! closed and a hard timeout, printing its environment NUL-delimited between two sentinels
//! (whatever the rc files print around it is ignored).
//!
//! Only PATH and `MIRANTE_*` (never `MIRANTE_HOME`: the app's socket decides the home) are
//! taken, plus HOME, USER and LANG when the app itself lacks them. Nothing else from the
//! shell reaches the daemon: no tokens, no keys, no `DYLD_*`.
//!
//! When the shell fails, times out or prints no PATH, the app's own PATH is used with the
//! usual install directories appended (`~/.local/bin`, `/opt/homebrew/bin`,
//! `/usr/local/bin`, `~/.cargo/bin`). The daemon also searches those directories (and
//! nvm's) for the agent binaries itself (`mirante_core::agent_bin`).
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, OnceLock};
use std::time::Duration;

/// Longest the login shell may take.
pub const LOGIN_ENV_TIMEOUT: Duration = Duration::from_secs(3);
/// The shell used when `$SHELL` is missing or not a listed login shell.
pub const FALLBACK_SHELL: &str = "/bin/zsh";
pub const BEGIN: &str = "__MIRANTE_LOGIN_ENV_BEGIN__";
pub const END: &str = "__MIRANTE_LOGIN_ENV_END__";
/// Most output read from the shell (an rc file could print anything).
const MAX_OUTPUT: u64 = 1 << 20;
/// Taken from the shell only when the app's own environment lacks them.
const IF_MISSING: [&str; 3] = ["HOME", "USER", "LANG"];

/// `$SHELL` when it is an absolute path listed in `etc_shells`, else [`FALLBACK_SHELL`].
pub fn pick_shell(shell: Option<&OsStr>, etc_shells: &str) -> PathBuf {
    let listed = |s: &str| etc_shells.lines().map(str::trim).any(|l| !l.starts_with('#') && l == s);
    match shell.and_then(OsStr::to_str) {
        Some(s) if s.starts_with('/') && listed(s) => PathBuf::from(s),
        _ => PathBuf::from(FALLBACK_SHELL),
    }
}

/// What the shell runs: the environment, NUL-delimited, between the sentinels.
pub fn script() -> String {
    format!("printf '%s\\0' {BEGIN}; /usr/bin/env -0; printf '%s\\0' {END}")
}

/// The `KEY=value` records between the sentinels, or `None` when the output has no complete
/// frame (the shell died or was cut off).
pub fn parse_dump(out: &[u8]) -> Option<Vec<(String, String)>> {
    let begin = [BEGIN.as_bytes(), b"\0"].concat();
    let start = out.windows(begin.len()).position(|w| w == begin.as_slice())? + begin.len();
    let mut vars = Vec::new();
    for rec in out[start..].split(|b| *b == 0) {
        if rec == END.as_bytes() {
            return Some(vars);
        }
        let Ok(rec) = std::str::from_utf8(rec) else { continue };
        if let Some((k, v)) = rec.split_once('=') {
            if !k.is_empty() {
                vars.push((k.to_string(), v.to_string()));
            }
        }
    }
    None
}

/// The variables a daemon may take from the login shell (see the module docs). `present`
/// says whether the app's own environment has a variable.
pub fn select(dump: &[(String, String)], present: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    dump.iter()
        .filter(|(k, v)| match k.as_str() {
            "PATH" => !v.is_empty(),
            "MIRANTE_HOME" => false,
            k if k.starts_with("MIRANTE_") => true,
            k if IF_MISSING.contains(&k) => !present(k),
            _ => false,
        })
        .cloned()
        .collect()
}

/// The app's PATH with the usual install directories appended (those not already on it).
pub fn fallback_path(path: Option<&str>, home: Option<&Path>) -> String {
    let mut dirs: Vec<PathBuf> =
        std::env::split_paths(path.unwrap_or_default()).filter(|p| !p.as_os_str().is_empty()).collect();
    let mut extra: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        extra.push(h.join(".local/bin"));
    }
    extra.push("/opt/homebrew/bin".into());
    extra.push("/usr/local/bin".into());
    if let Some(h) = home {
        extra.push(h.join(".cargo/bin"));
    }
    for d in extra {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    std::env::join_paths(dirs).ok().and_then(|s| s.into_string().ok()).unwrap_or_default()
}

/// Runs `shell -l -i -c <script>` (stdin closed, stderr dropped, own process group) and
/// returns its output, or `None` on failure or after `timeout` (the group is killed).
pub fn run_shell(shell: &Path, timeout: Duration) -> Option<Vec<u8>> {
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let pid = child.id() as i32;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let ok = (&mut stdout).take(MAX_OUTPUT).read_to_end(&mut out).is_ok();
        let _ = tx.send(ok.then_some(out));
    });
    let got = rx.recv_timeout(timeout);
    if got.is_err() {
        // SAFETY: plain syscall; the group is the shell's own (process_group(0) above), so
        // only the shell and what it started are signalled.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    // Reaped off this thread: a stuck child must not hold up the caller.
    std::thread::spawn(move || {
        let _ = child.kill();
        let _ = child.wait();
    });
    got.ok().flatten()
}

/// The variables for a daemon started by the app: from `shell` when it answers with a
/// PATH, else the fallback PATH. `present` is the app's own environment.
pub fn resolve(
    shell: &Path,
    timeout: Duration,
    present: impl Fn(&str) -> bool,
    app_path: Option<&str>,
    home: Option<&Path>,
) -> Vec<(String, String)> {
    let mut vars =
        run_shell(shell, timeout).and_then(|out| parse_dump(&out)).map(|d| select(&d, &present)).unwrap_or_default();
    if !vars.iter().any(|(k, _)| k == "PATH") {
        vars.push(("PATH".to_string(), fallback_path(app_path, home)));
    }
    vars
}

/// [`resolve`] for this process, computed once per run.
pub fn daemon_env() -> &'static [(String, String)] {
    static CACHE: OnceLock<Vec<(String, String)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let etc = std::fs::read_to_string("/etc/shells").unwrap_or_default();
        let shell = pick_shell(std::env::var_os("SHELL").as_deref(), &etc);
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
        let app_path = std::env::var("PATH").ok();
        resolve(&shell, LOGIN_ENV_TIMEOUT, |k| std::env::var_os(k).is_some(), app_path.as_deref(), home.as_deref())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;
    use std::time::Instant;

    const SHELLS: &str = "# List of acceptable shells\n/bin/bash\n/bin/zsh\n/opt/homebrew/bin/fish\n";

    #[test]
    fn only_a_listed_absolute_shell_is_used() {
        assert_eq!(pick_shell(Some(OsStr::new("/bin/bash")), SHELLS), PathBuf::from("/bin/bash"));
        assert_eq!(
            pick_shell(Some(OsStr::new("/opt/homebrew/bin/fish")), SHELLS),
            PathBuf::from("/opt/homebrew/bin/fish")
        );
        for bad in ["/tmp/evil", "bash", "", "# List of acceptable shells"] {
            assert_eq!(pick_shell(Some(OsStr::new(bad)), SHELLS), PathBuf::from(FALLBACK_SHELL), "{bad}");
        }
        assert_eq!(pick_shell(None, SHELLS), PathBuf::from(FALLBACK_SHELL));
    }

    fn framed(records: &[&str]) -> Vec<u8> {
        let mut out = b"Last login: today\nwelcome banner".to_vec();
        out.extend_from_slice(BEGIN.as_bytes());
        out.push(0);
        for r in records {
            out.extend_from_slice(r.as_bytes());
            out.push(0);
        }
        out.extend_from_slice(END.as_bytes());
        out.push(0);
        out.extend_from_slice(b"goodbye\n");
        out
    }

    #[test]
    fn parses_only_inside_the_frame_and_keeps_newlines_in_values() {
        let out = framed(&["PATH=/a:/b", "MULTI=line1\nline2", "EQ=a=b", "=nokey"]);
        let vars = parse_dump(&out).unwrap();
        assert_eq!(
            vars,
            vec![("PATH".into(), "/a:/b".into()), ("MULTI".into(), "line1\nline2".into()), ("EQ".into(), "a=b".into())]
        );
        // Cut off before the end sentinel: nothing is trusted.
        let cut = &out[..out.len() - END.len() - 10];
        assert_eq!(parse_dump(cut), None);
        assert_eq!(parse_dump(b"no frame at all"), None);
    }

    #[test]
    fn the_filter_passes_path_and_mirante_vars_only() {
        let dump: Vec<(String, String)> = [
            ("PATH", "/Users/me/.local/bin:/usr/bin"),
            ("MIRANTE_CLAUDE_BIN", "/Users/me/.local/bin/claude"),
            ("MIRANTE_CLAUDE_INLINE_ANSWERS", "1"),
            ("MIRANTE_HOME", "/elsewhere"),
            ("MIRANTEX", "no"),
            ("ANTHROPIC_API_KEY", "sk-ant-secret"),
            ("OPENAI_API_KEY", "sk-secret"),
            ("GITHUB_TOKEN", "ghp_secret"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
            ("SSH_AUTH_SOCK", "/tmp/ssh"),
            ("DYLD_INSERT_LIBRARIES", "/tmp/x.dylib"),
            ("LD_PRELOAD", "/tmp/x.so"),
            ("NODE_OPTIONS", "--require /tmp/x.js"),
            ("CLAUDECODE", "1"),
            ("HOME", "/Users/me"),
            ("USER", "me"),
            ("LANG", "en_US.UTF-8"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .to_vec();
        let app_has = |k: &str| k == "HOME" || k == "USER";
        let got = select(&dump, app_has);
        let keys: Vec<&str> = got.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["PATH", "MIRANTE_CLAUDE_BIN", "MIRANTE_CLAUDE_INLINE_ANSWERS", "LANG"]);
        assert!(!got.iter().any(|(_, v)| v.contains("secret")));
        // An empty PATH is no PATH.
        let empty = vec![("PATH".to_string(), String::new())];
        assert!(select(&empty, |_| false).is_empty());
    }

    #[test]
    fn fallback_appends_the_usual_dirs_once() {
        let home = Path::new("/Users/me");
        assert_eq!(
            fallback_path(Some("/usr/bin:/bin:/opt/homebrew/bin"), Some(home)),
            "/usr/bin:/bin:/opt/homebrew/bin:/Users/me/.local/bin:/usr/local/bin:/Users/me/.cargo/bin"
        );
        assert_eq!(fallback_path(None, None), "/opt/homebrew/bin:/usr/local/bin");
    }

    fn fake_shell(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        // Called as `fake-shell -l -i -c <script>`: the script is $4. Warmed up once, so
        // macOS's first-exec scan is not charged to the timeouts below.
        mirante_core::test_script::write_sh(&p, body, 0o755);
        p
    }

    #[test]
    fn a_login_shell_that_answers_supplies_path_and_mirante_vars() {
        let d = TestDir::new();
        let shell = fake_shell(
            d.path(),
            "answers",
            "echo 'rc noise'; export PATH=/from/login:/usr/bin MIRANTE_CODEX_BIN=/x/codex SECRET_TOKEN=s3cret; eval \"$4\"",
        );
        let vars = resolve(&shell, Duration::from_secs(5), |_| true, Some("/usr/bin"), None);
        assert!(vars.contains(&("PATH".into(), "/from/login:/usr/bin".into())), "{vars:?}");
        assert!(vars.contains(&("MIRANTE_CODEX_BIN".into(), "/x/codex".into())), "{vars:?}");
        assert!(!vars.iter().any(|(k, v)| k == "SECRET_TOKEN" || v.contains("s3cret")), "{vars:?}");
    }

    #[test]
    fn a_hanging_or_failing_shell_falls_back_in_time() {
        let d = TestDir::new();
        let hang = fake_shell(d.path(), "hangs", "sleep 30");
        let t = Instant::now();
        let vars = resolve(&hang, Duration::from_millis(300), |_| true, Some("/usr/bin"), Some(Path::new("/Users/me")));
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        assert_eq!(
            vars,
            vec![(
                "PATH".into(),
                "/usr/bin:/Users/me/.local/bin:/opt/homebrew/bin:/usr/local/bin:/Users/me/.cargo/bin".into()
            )]
        );
        let fail = fake_shell(d.path(), "fails", "exit 1");
        let vars = resolve(&fail, Duration::from_secs(5), |_| true, Some("/usr/bin"), None);
        assert_eq!(vars.len(), 1);
        assert!(run_shell(Path::new("/nonexistent/shell"), Duration::from_secs(1)).is_none());
    }
}
