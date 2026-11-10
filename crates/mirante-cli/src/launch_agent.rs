//! The optional per-user LaunchAgent `com.mirante.daemon` (macOS): `mirante daemon
//! install-agent` writes `~/Library/LaunchAgents/com.mirante.daemon.plist` and loads it in
//! the `gui/<uid>` domain; `uninstall-agent` unloads and deletes it.
//!
//! Why: a process inherits the macOS "responsible process" of whatever started it, and
//! privacy prompts (TCC: "would like to access files in your Documents folder") are asked
//! for and remembered against that process. A daemon the desktop app starts is attributed to
//! Mirante.app, so every file the daemon, its agents and their hooks touch under ~/Documents
//! asks in the app's name. A daemon launchd starts is responsible for itself: it asks once,
//! as `mirante-daemon`, and the grant sticks while its signature stays the same (see
//! `scripts/dev-sign.sh`). The agent also starts the daemon at login (`RunAtLoad`).
//!
//! When the agent is installed for this `MIRANTE_HOME`, every start (the CLI's, the app's
//! "Start daemon", `daemon restart`) asks launchd to start the job instead of spawning the
//! daemon itself (see `daemon_ctl::start_daemon_inner`).
//!
//! The plist holds the daemon path, `MIRANTE_HOME`, the log path, and PATH, LANG and
//! `MIRANTE_*` from the environment `install-agent` runs in: never tokens or keys (the file is
//! world-readable, as launchd wants it). Run `install-agent` again after changing those.
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use mirante_ipc::Paths;

/// The job's label (and the plist's file name).
pub const LABEL: &str = "com.mirante.daemon";

/// `<user home>/Library/LaunchAgents/com.mirante.daemon.plist`.
pub fn plist_path(user_home: &Path) -> PathBuf {
    user_home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

/// The variables written into the plist: PATH, LANG and `MIRANTE_*` (not `MIRANTE_HOME`,
/// which is the job's own, nor the session markers of an agent's environment), sorted.
pub fn agent_env(vars: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
    let session_markers = [
        mirante_core::claude_launch::TASK_ENV,
        mirante_core::claude_launch::SESSION_ENV,
        mirante_core::claude_launch::AGENT_ENV,
    ];
    let mut out: Vec<(String, String)> = vars
        .into_iter()
        .filter(|(k, v)| match k.as_str() {
            "PATH" | "LANG" => !v.is_empty(),
            k if k == mirante_ipc::paths::HOME_ENV || session_markers.contains(&k) => false,
            k => k.starts_with("MIRANTE_"),
        })
        .collect();
    out.sort();
    out
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&")
}

fn string(s: &str) -> String {
    format!("<string>{}</string>", xml_escape(s))
}

/// The plist: runs `bin` with `MIRANTE_HOME` = `paths.home()` plus `env`, from that home,
/// logging to `paths.log()`; started at load (login) and on demand, never restarted by launchd
/// when it exits (`daemon.shutdown` and `daemon restart` stop it on purpose).
pub fn plist(bin: &Path, paths: &Paths, env: &[(String, String)]) -> String {
    let home = paths.home().to_string_lossy();
    let log = paths.log().to_string_lossy().into_owned();
    let mut vars = format!("\t\t<key>{}</key>\n\t\t{}\n", mirante_ipc::paths::HOME_ENV, string(&home));
    for (k, v) in env {
        vars.push_str(&format!("\t\t<key>{}</key>\n\t\t{}\n", xml_escape(k), string(v)));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `mirante daemon install-agent`; removed by `mirante daemon uninstall-agent`. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	{label}
	<key>ProgramArguments</key>
	<array>
		{bin}
	</array>
	<key>EnvironmentVariables</key>
	<dict>
{vars}	</dict>
	<key>WorkingDirectory</key>
	{home}
	<key>StandardOutPath</key>
	{log}
	<key>StandardErrorPath</key>
	{log}
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<false/>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
        label = string(LABEL),
        bin = string(&bin.to_string_lossy()),
        home = string(&home),
        log = string(&log),
    )
}

/// The `<string>` right after `<key>{key}</key>` in a plist this module wrote.
fn value_after(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("<key>{key}</key>"))?;
    let rest = &text[at..];
    let open = rest.find("<string>")? + "<string>".len();
    let close = rest[open..].find("</string>")?;
    Some(xml_unescape(&rest[open..open + close]))
}

/// The `MIRANTE_HOME` a plist runs its daemon with.
pub fn plist_home(text: &str) -> Option<PathBuf> {
    value_after(text, mirante_ipc::paths::HOME_ENV).map(PathBuf::from)
}

/// The daemon binary a plist runs.
pub fn plist_program(text: &str) -> Option<PathBuf> {
    value_after(text, "ProgramArguments").map(PathBuf::from)
}

/// The installed plist when it runs the daemon of `paths` (macOS only; another home's agent
/// is not this daemon's).
pub fn installed_for(paths: &Paths, user_home: Option<&Path>) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let plist = plist_path(user_home?);
    let text = std::fs::read_to_string(&plist).ok()?;
    (plist_home(&text)? == paths.home()).then_some(plist)
}

/// [`installed_for`] with `$HOME`.
pub fn installed(paths: &Paths) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    installed_for(paths, home.as_deref())
}

/// What is asked of launchd (the test seam).
pub trait Launchd {
    /// The job is loaded (running or not).
    fn loaded(&self) -> bool;
    fn bootstrap(&self, plist: &Path) -> io::Result<()>;
    /// Starts the job unless it runs.
    fn kickstart(&self) -> io::Result<()>;
    /// Unloads the job (stopping its daemon if it runs); not loaded is not an error.
    fn bootout(&self) -> io::Result<()>;
}

/// `/bin/launchctl` in the `gui/<uid>` domain.
pub struct Launchctl {
    domain: String,
}

impl Launchctl {
    pub fn gui() -> Launchctl {
        // SAFETY: getuid cannot fail.
        Launchctl { domain: format!("gui/{}", unsafe { libc::getuid() }) }
    }

    fn target(&self) -> String {
        format!("{}/{LABEL}", self.domain)
    }

    fn run(&self, args: &[&str]) -> io::Result<std::process::Output> {
        Command::new("/bin/launchctl").args(args).stdin(Stdio::null()).output()
    }

    fn ok(&self, args: &[&str]) -> io::Result<()> {
        let out = self.run(args)?;
        if out.status.success() {
            return Ok(());
        }
        let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(io::Error::other(format!("launchctl {} failed ({}): {why}", args.join(" "), out.status)))
    }
}

impl Launchd for Launchctl {
    fn loaded(&self) -> bool {
        self.run(&["print", &self.target()]).is_ok_and(|o| o.status.success())
    }

    fn bootstrap(&self, plist: &Path) -> io::Result<()> {
        self.ok(&["bootstrap", &self.domain, &plist.to_string_lossy()])
    }

    fn kickstart(&self) -> io::Result<()> {
        self.ok(&["kickstart", &self.target()])
    }

    fn bootout(&self) -> io::Result<()> {
        if !self.loaded() {
            return Ok(());
        }
        self.ok(&["bootout", &self.target()])
    }
}

/// Has launchd start the job: loads `plist` when it is not loaded (which starts it:
/// `RunAtLoad`), then kickstarts it (a no-op when it runs).
pub fn start(launchd: &dyn Launchd, plist: &Path) -> io::Result<()> {
    if !launchd.loaded() {
        launchd.bootstrap(plist)?;
    }
    launchd.kickstart()
}

/// Writes the plist (mode 0644, through a temp file) for `bin` and `paths`.
pub fn write_plist(at: &Path, bin: &Path, paths: &Paths, env: &[(String, String)]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = at.parent().ok_or_else(|| io::Error::other("the plist path has no folder"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = at.with_extension("plist.tmp");
    let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o644).open(&tmp)?;
    f.write_all(plist(bin, paths, env).as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, at)
}

/// The refusal for an agent's own process (an intent check, as `confirm`'s): installing a
/// login item is the person's decision. `env` is this process's environment.
pub fn person_only(env: &dyn Fn(&str) -> Option<String>, has_tty: bool) -> Result<(), String> {
    let markers = [
        mirante_core::claude_launch::TASK_ENV,
        mirante_core::claude_launch::SESSION_ENV,
        mirante_core::claude_launch::AGENT_ENV,
    ];
    if markers.iter().any(|k| env(k).is_some_and(|v| !v.is_empty())) {
        return Err("this runs inside a Mirante agent session: a person runs it in their own terminal".into());
    }
    if !has_tty {
        return Err("run this in a terminal (it changes what starts at login)".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;
    use std::cell::RefCell;

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn the_plist_runs_the_daemon_with_its_home_log_and_safe_env_only() {
        let env = agent_env(kv(&[
            ("PATH", "/Users/me/.local/bin:/usr/bin"),
            ("MIRANTE_CLAUDE_BIN", "/opt/homebrew/bin/claude"),
            ("MIRANTE_HOME", "/elsewhere"),
            ("MIRANTE_TASK_ID", "t1"),
            ("ANTHROPIC_API_KEY", "sk-secret"),
            ("OPENAI_API_KEY", "sk-secret"),
            ("LANG", "en_US.UTF-8"),
            ("TERM_PROGRAM", "iTerm"),
        ]));
        assert_eq!(
            env,
            kv(&[
                ("LANG", "en_US.UTF-8"),
                ("MIRANTE_CLAUDE_BIN", "/opt/homebrew/bin/claude"),
                ("PATH", "/Users/me/.local/bin:/usr/bin"),
            ])
        );
        let paths = Paths::new(Path::new("/Users/me/.mirante"));
        let text = plist(Path::new("/Users/me/.local/bin/mirante-daemon"), &paths, &env);
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"), "{text}");
        assert!(text.contains("<key>Label</key>\n\t<string>com.mirante.daemon</string>"), "{text}");
        assert_eq!(plist_program(&text), Some(PathBuf::from("/Users/me/.local/bin/mirante-daemon")));
        assert_eq!(plist_home(&text), Some(PathBuf::from("/Users/me/.mirante")));
        assert_eq!(value_after(&text, "StandardErrorPath").as_deref(), Some("/Users/me/.mirante/daemon.log"));
        assert_eq!(value_after(&text, "StandardOutPath").as_deref(), Some("/Users/me/.mirante/daemon.log"));
        assert_eq!(value_after(&text, "WorkingDirectory").as_deref(), Some("/Users/me/.mirante"));
        assert_eq!(value_after(&text, "MIRANTE_CLAUDE_BIN").as_deref(), Some("/opt/homebrew/bin/claude"));
        assert!(text.contains("<key>RunAtLoad</key>\n\t<true/>"), "{text}");
        assert!(text.contains("<key>KeepAlive</key>\n\t<false/>"), "{text}");
        assert!(!text.contains("sk-secret") && !text.contains("t1") && !text.contains("/elsewhere"), "{text}");
        assert_eq!(text.matches("<key>MIRANTE_HOME</key>").count(), 1);
    }

    #[test]
    fn odd_characters_are_escaped_and_read_back() {
        let paths = Paths::new(Path::new("/Users/a&b/<x> \"q\"/.mirante"));
        let text = plist(Path::new("/opt/m&m/mirante-daemon"), &paths, &kv(&[("MIRANTE_X", "a<b")]));
        assert!(text.contains("/Users/a&amp;b/&lt;x&gt; &quot;q&quot;/.mirante"), "{text}");
        assert_eq!(plist_home(&text), Some(PathBuf::from("/Users/a&b/<x> \"q\"/.mirante")));
        assert_eq!(plist_program(&text), Some(PathBuf::from("/opt/m&m/mirante-daemon")));
        assert_eq!(value_after(&text, "MIRANTE_X").as_deref(), Some("a<b"));
    }

    #[test]
    fn plutil_accepts_the_plist() {
        if !Path::new("/usr/bin/plutil").exists() {
            return;
        }
        let d = TestDir::new();
        let p = d.path().join("x.plist");
        let paths = Paths::new(&d.path().join("h"));
        write_plist(&p, Path::new("/x/mirante-daemon"), &paths, &kv(&[("PATH", "/usr/bin:/bin")])).unwrap();
        let out = Command::new("/usr/bin/plutil").arg("-lint").arg(&p).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[test]
    fn the_agent_is_this_daemons_only_when_it_runs_this_home() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join(".mirante"));
        assert_eq!(installed_for(&paths, Some(d.path())), None);
        let p = plist_path(d.path());
        assert!(p.ends_with("Library/LaunchAgents/com.mirante.daemon.plist"));
        write_plist(&p, Path::new("/x/mirante-daemon"), &paths, &[]).unwrap();
        let expected = cfg!(target_os = "macos").then(|| p.clone());
        assert_eq!(installed_for(&paths, Some(d.path())), expected);
        assert_eq!(installed_for(&Paths::new(&d.path().join("other")), Some(d.path())), None);
        assert_eq!(installed_for(&paths, None), None);
    }

    #[derive(Default)]
    struct FakeLaunchd {
        loaded: bool,
        calls: RefCell<Vec<String>>,
    }

    impl Launchd for FakeLaunchd {
        fn loaded(&self) -> bool {
            self.loaded
        }
        fn bootstrap(&self, plist: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push(format!("bootstrap {}", plist.display()));
            Ok(())
        }
        fn kickstart(&self) -> io::Result<()> {
            self.calls.borrow_mut().push("kickstart".into());
            Ok(())
        }
        fn bootout(&self) -> io::Result<()> {
            self.calls.borrow_mut().push("bootout".into());
            Ok(())
        }
    }

    #[test]
    fn start_loads_the_job_when_needed_then_kickstarts_it() {
        let cold = FakeLaunchd::default();
        start(&cold, Path::new("/p.plist")).unwrap();
        assert_eq!(*cold.calls.borrow(), vec!["bootstrap /p.plist".to_string(), "kickstart".into()]);
        let warm = FakeLaunchd { loaded: true, ..Default::default() };
        start(&warm, Path::new("/p.plist")).unwrap();
        assert_eq!(*warm.calls.borrow(), vec!["kickstart".to_string()]);
    }

    #[test]
    fn only_a_person_on_a_terminal_installs_the_agent() {
        let none = |_: &str| None;
        assert_eq!(person_only(&none, true), Ok(()));
        assert!(person_only(&none, false).unwrap_err().contains("terminal"));
        let agent = |k: &str| (k == "MIRANTE_TASK_ID").then(|| "t1".to_string());
        assert!(person_only(&agent, true).unwrap_err().contains("agent session"));
    }
}
