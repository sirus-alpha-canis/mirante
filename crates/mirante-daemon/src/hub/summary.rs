//! Thread summaries by the person's own Claude CLI (founder decision 3, Spike S2): on a
//! person's click only, never automatically; headless and tool-less, with exactly the
//! invocation S2 validated on Claude 2.1.285:
//!
//! ```text
//! env -i HOME USER PATH LANG claude -p --tools "" --setting-sources "" --strict-mcp-config
//!   --mcp-config '{"mcpServers":{}}' --no-session-persistence --disable-slash-commands
//!   --permission-mode plan --permission-prompts none --system-prompt <DATA-fenced prompt>
//!   --max-budget-usd 0.10 --model haiku --output-format json <prompt> < /dev/null
//! ```
//!
//! in a dedicated empty directory, killed after 60 s. `--settings` is never passed (hooks
//! given that way run even with `--setting-sources ""`). Before the first summary of each
//! Claude version, a probe run of the same invocation (stream-json, "Reply OK.") must show
//! no tool, no MCP server, no slash command and no skill offered; the verdict is kept per
//! binary (path, version, size, modification time, in the store) and summaries are refused when it fails. Codex is never used: no invocation of it
//! was reliably tool-less (S2: NO-GO). The result is agent text: redacted, capped, shown as
//! text.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mirante_core::agent_bin::{self, Agent, Lookup};
use mirante_core::model::{new_id, Task};
use mirante_core::threads::{summary_request, ThreadLink, MAX_SUMMARY};
use mirante_ipc::types::SummariesSetting;
use serde_json::{json, Value};

use super::{Hub, HubError, Result};

/// Longest a summary run may take before it is killed.
pub const SUMMARY_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest `claude --version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(15);
/// The most one run may spend of the person's Claude usage (Claude stops there).
pub const MAX_BUDGET_USD: &str = "0.10";
/// The model a summary uses (≈ $0.01 a summary in S2).
pub const MODEL: &str = "haiku";
/// Most bytes of arguments one run is handed (the prompt rides in argv; the system's
/// `ARG_MAX` is 1 MiB on macOS, environment included): a longer request is refused.
pub const ARGS_MAX: usize = 256 * 1024;
/// The settings key (`none` | `claude`).
pub const SETTINGS_KEY: &str = "summaries";
/// Most stdout a run may produce that is read.
const OUTPUT_MAX: u64 = 1024 * 1024;

/// What a summary run gave back.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub cost_usd: Option<f64>,
}

/// The arguments after the binary: S2's clean invocation, `output` being `json` (a summary)
/// or `stream-json` (the probe, which reads the `init` line).
pub fn claude_args(system: &str, prompt: &str, output: &str) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--tools",
        "",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        r#"{"mcpServers":{}}"#,
        "--no-session-persistence",
        "--disable-slash-commands",
        "--permission-mode",
        "plan",
        "--permission-prompts",
        "none",
        "--system-prompt",
        system,
        "--max-budget-usd",
        MAX_BUDGET_USD,
        "--model",
        MODEL,
        "--output-format",
        output,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if output == "stream-json" {
        a.push("--verbose".into());
    }
    a.push(prompt.to_string());
    a
}

/// The only environment a run gets: `HOME`, `USER`, `PATH` (the binary's folder, then
/// `/usr/bin:/bin`) and `LANG`.
pub fn clean_env(bin: &Path) -> Vec<(String, String)> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut path = String::new();
    if let Some(dir) = bin.parent().filter(|d| !d.as_os_str().is_empty()) {
        path.push_str(&dir.to_string_lossy());
        path.push(':');
    }
    path.push_str("/usr/bin:/bin");
    let mut env = vec![("PATH".to_string(), path), ("LANG".to_string(), var("LANG").unwrap_or("en_US.UTF-8".into()))];
    for k in ["HOME", "USER"] {
        if let Some(v) = var(k) {
            env.push((k.to_string(), v));
        }
    }
    env
}

/// Runs `bin args` in `cwd` with [`clean_env`], stdin closed, in a process group of its
/// own that is killed whole after `timeout` (a child it started cannot outlive it). Refused
/// before it starts when the arguments pass [`ARGS_MAX`]. Returns stdout (at most
/// [`OUTPUT_MAX`] bytes), or why it failed.
fn run(bin: &Path, args: &[String], cwd: &Path, timeout: Duration) -> std::result::Result<String, String> {
    use std::os::unix::process::CommandExt;
    let size: usize = args.iter().map(|a| a.len() + 1).sum();
    if size > ARGS_MAX {
        return Err(format!("the request is {size} bytes; at most {ARGS_MAX} are handed to Claude"));
    }
    let mut child = Command::new(bin)
        .args(args)
        .process_group(0)
        .current_dir(cwd)
        .env_clear()
        .envs(clean_env(bin))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", bin.display()))?;
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut stdout).take(OUTPUT_MAX).read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut stderr).take(16 * 1024).read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() >= deadline => {
                // The whole group: its own children too (their pipes would hold the reads).
                if let Ok(pgid) = i32::try_from(child.id()) {
                    // SAFETY: plain syscall; the group is the child's own (`process_group(0)`).
                    unsafe { libc::kill(-pgid, libc::SIGKILL) };
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("stopped after {} s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if !status.success() && out.trim().is_empty() {
        let why = mirante_core::redact::redact(err.trim());
        return Err(format!("exited with {status}: {}", why.chars().take(400).collect::<String>()));
    }
    Ok(out)
}

/// Whether a probe's stream-json output shows a session with nothing to act with: its
/// `init` line lists no tool, no MCP server, no slash command and no skill.
pub fn probe_passed(stream: &str) -> bool {
    let empty = |v: &Value, k: &str| v.get(k).is_none_or(|x| x.as_array().is_some_and(|a| a.is_empty()));
    stream.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).any(|v| {
        v["type"] == "system"
            && v["subtype"] == "init"
            && v["tools"].as_array().is_some_and(|a| a.is_empty())
            && v["mcp_servers"].as_array().is_some_and(|a| a.is_empty())
            && empty(&v, "slash_commands")
            && empty(&v, "skills")
    })
}

/// A summary run's `--output-format json` result: its text (agent text: control characters
/// but line breaks removed, redacted, capped) and what it reported it cost.
pub fn parse_result(out: &str) -> std::result::Result<Run, String> {
    let v: Value = out
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l.trim()).ok().filter(|v| v["type"] == "result"))
        .ok_or_else(|| "Claude gave no result".to_string())?;
    let text = v["result"].as_str().unwrap_or_default();
    if v["is_error"] == true || v["subtype"] != "success" {
        let why: String = mirante_core::redact::redact(text).chars().take(300).collect();
        return Err(format!("Claude did not summarize ({}): {why}", v["subtype"].as_str().unwrap_or("error")));
    }
    let clean: String = text.chars().filter(|c| !c.is_control() || *c == '\n').collect();
    let text: String = mirante_core::redact::redact(clean.trim()).chars().take(MAX_SUMMARY).collect();
    if text.trim().is_empty() {
        return Err("Claude gave an empty summary".into());
    }
    Ok(Run { text, cost_usd: v["total_cost_usd"].as_f64() })
}

/// A dedicated empty folder for one run, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(root: &Path) -> std::io::Result<Scratch> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(root)?;
        let dir = root.join(new_id());
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Hub {
    /// The person's `summaries` setting (`none` unless the settings file says `claude`).
    pub fn summaries_setting(&self) -> SummariesSetting {
        let v = std::fs::read_to_string(self.paths.settings())
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .unwrap_or(Value::Null);
        match v[SETTINGS_KEY].as_str() {
            Some("claude") => SummariesSetting::Claude,
            _ => SummariesSetting::None,
        }
    }

    /// Tests: the Claude binary summaries run (a fake that records what it was given).
    pub fn set_summary_bin(&self, bin: PathBuf) {
        *self.summary_bin.lock().unwrap_or_else(|p| p.into_inner()) = Some(bin);
    }

    fn summary_claude(&self) -> Result<PathBuf> {
        if let Some(b) = self.summary_bin.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            return Ok(b);
        }
        agent_bin::resolve(Agent::Claude, &Lookup::from_env(Agent::Claude)).map(|r| r.path).map_err(HubError::Invalid)
    }

    /// Whether the tool-less probe passed for this binary (`version`, and the file as it is
    /// on disk: canonical path, size, modification time). Run once per binary, the verdict
    /// kept in the store, never in a file an agent could write.
    fn probe(&self, bin: &Path, version: &str, root: &Path) -> Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let canon = std::fs::canonicalize(bin).map_err(|e| HubError::Conflict(format!("{}: {e}", bin.display())))?;
        let meta = std::fs::metadata(&canon).map_err(|e| HubError::Conflict(format!("{}: {e}", canon.display())))?;
        let key = canon.to_string_lossy().into_owned();
        let (size, mtime) = (i64::try_from(meta.size()).unwrap_or(i64::MAX), meta.mtime());
        if let Some(ok) = self.lock().store.summary_probe(&key, version, size, mtime)? {
            return Ok(ok);
        }
        let dir = Scratch::new(root).map_err(|e| HubError::Conflict(format!("summary folder: {e}")))?;
        let system = "Reply with the single word OK. You have no tools.";
        let out = run(bin, &claude_args(system, "Reply OK.", "stream-json"), &dir.0, SUMMARY_TIMEOUT)
            .map_err(|e| HubError::Conflict(format!("the tool-less check of Claude {version} did not run: {e}")))?;
        let ok = probe_passed(&out);
        let mut st = self.lock();
        if let Err(e) = st.store.set_summary_probe(&key, version, size, mtime, ok) {
            st.log_error(None, "recording the summary probe", &e);
        }
        let _ = st.event(None, "summaries_probe", json!({ "version": version, "ok": ok }));
        st.publish();
        Ok(ok)
    }

    /// `task.summarize { with: claude }`: refused unless the setting is `claude` and the
    /// probe passed for the installed version; one run at a time.
    pub(crate) fn summarize_with_claude(&self, task: &Task, link: &ThreadLink, answers: &[String]) -> Result<Run> {
        if self.summaries_setting() != SummariesSetting::Claude {
            return Err(HubError::Conflict(
                "summaries by your Claude CLI are off: turn them on in Settings (Thread summaries), then ask again"
                    .into(),
            ));
        }
        let Ok(_one) = self.summary_gate.try_lock() else {
            return Err(HubError::Conflict("a summary is already running: wait for it".into()));
        };
        let bin = self.summary_claude()?;
        let root = self.paths.tmp().join("summarize");
        let dir = Scratch::new(&root).map_err(|e| HubError::Conflict(format!("summary folder: {e}")))?;
        let version = run(&bin, &["--version".to_string()], &dir.0, VERSION_TIMEOUT)
            .map_err(|e| HubError::Conflict(format!("claude --version: {e}")))?;
        let version = version.lines().next().unwrap_or_default().trim().to_string();
        if version.is_empty() {
            return Err(HubError::Conflict("claude --version said nothing".into()));
        }
        if !self.probe(&bin, &version, &root)? {
            return Err(HubError::Conflict(format!(
                "Claude {version} did not pass the tool-less check (it offered a tool, an MCP server, a slash \
                 command or a skill): summaries by Claude are refused for this version"
            )));
        }
        let texts: Vec<&str> = answers
            .iter()
            .map(String::as_str)
            .chain([task.goal.as_str(), task.notes.as_str(), task.title.as_str()])
            .collect();
        let marker = mirante_core::groups::marker_for(texts.iter().copied());
        let (system, prompt) = summary_request(task, link, answers, &marker);
        let out = run(&bin, &claude_args(&system, &prompt, "json"), &dir.0, SUMMARY_TIMEOUT)
            .map_err(|e| HubError::Conflict(format!("the summary did not finish: {e}")))?;
        parse_result(&out).map_err(HubError::Conflict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_invocation_is_s2s_and_never_passes_settings() {
        let a = claude_args("SYS", "<<DATA-x>>", "json");
        let want: Vec<String> = [
            "-p",
            "--tools",
            "",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            r#"{"mcpServers":{}}"#,
            "--no-session-persistence",
            "--disable-slash-commands",
            "--permission-mode",
            "plan",
            "--permission-prompts",
            "none",
            "--system-prompt",
            "SYS",
            "--max-budget-usd",
            "0.10",
            "--model",
            "haiku",
            "--output-format",
            "json",
            "<<DATA-x>>",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(a, want);
        assert!(!a.iter().any(|x| x == "--settings"));
        let p = claude_args("S", "Reply OK.", "stream-json");
        assert_eq!(&p[p.len() - 3..], ["stream-json", "--verbose", "Reply OK."]);
    }

    #[test]
    fn the_probe_needs_an_init_with_nothing_to_act_with() {
        let ok = r#"{"type":"system","subtype":"init","tools":[],"mcp_servers":[],"slash_commands":[],"skills":[]}
{"type":"result","subtype":"success","result":"OK"}"#;
        assert!(probe_passed(ok));
        assert!(!probe_passed(r#"{"type":"system","subtype":"init","tools":["Bash"],"mcp_servers":[]}"#));
        assert!(!probe_passed(r#"{"type":"system","subtype":"init","tools":[],"mcp_servers":[{"name":"x"}]}"#));
        assert!(!probe_passed(r#"{"type":"system","subtype":"init","tools":[],"mcp_servers":[],"skills":["s"]}"#));
        assert!(!probe_passed(r#"{"type":"result","subtype":"success"}"#));
        assert!(!probe_passed("not json"));
    }

    #[test]
    fn a_result_is_text_redacted_and_capped() {
        let r = parse_result(
            r#"{"type":"result","subtype":"success","is_error":false,"result":"Did X.\u0007 token ghp_abcdefghijklmnopqrstuvwxyz0123456789","total_cost_usd":0.0106}"#,
        )
        .unwrap();
        assert!(
            r.text.starts_with("Did X.") && !r.text.contains('\u{7}') && !r.text.contains("ghp_abcdef"),
            "{}",
            r.text
        );
        assert_eq!(r.cost_usd, Some(0.0106));
        let long = format!(r#"{{"type":"result","subtype":"success","result":"{}"}}"#, "a".repeat(5000));
        assert_eq!(parse_result(&long).unwrap().text.chars().count(), MAX_SUMMARY);
        assert!(
            parse_result(r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"result":""}"#).is_err()
        );
        assert!(parse_result("").is_err());
    }

    #[test]
    fn the_environment_is_minimal() {
        let env = clean_env(Path::new("/opt/homebrew/bin/claude"));
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.iter().all(|k| ["PATH", "LANG", "HOME", "USER"].contains(k)), "{keys:?}");
        assert_eq!(env[0].1, "/opt/homebrew/bin:/usr/bin:/bin");
    }

    /// Review M1: a request too long for argv is refused before anything runs.
    #[test]
    fn a_request_past_the_argument_cap_never_runs() {
        let dir = std::env::temp_dir().join(format!("mirante-summary-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("echo-ran");
        let mark = dir.join("ran");
        mirante_core::test_script::write_sh(&bin, &format!("touch '{}'", mark.display()), 0o755);
        let e = run(&bin, &["x".repeat(ARGS_MAX + 1)], &dir, Duration::from_secs(5)).unwrap_err();
        assert!(e.contains("at most"), "{e}");
        assert!(!mark.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review M1: on timeout the run's whole process group goes, its children too.
    #[test]
    fn a_hung_runs_children_are_killed_with_it() {
        let dir = std::env::temp_dir().join(format!("mirante-summary-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("spawns");
        let pidfile = dir.join("child.pid");
        mirante_core::test_script::write_sh(
            &bin,
            &format!("sleep 60 & echo $! > '{}'; wait", pidfile.display()),
            0o755,
        );
        let t = Instant::now();
        assert!(run(&bin, &[], &dir, Duration::from_millis(500)).unwrap_err().contains("stopped after"));
        assert!(t.elapsed() < Duration::from_secs(10), "the child's open pipe did not hold the run");
        let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        // SAFETY: signal 0 only checks whether the process exists. An orphan is reaped by
        // launchd shortly after it dies: allow it a moment.
        let alive = || unsafe { libc::kill(pid, 0) } == 0;
        let until = Instant::now() + Duration::from_secs(3);
        while alive() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(), "the child {pid} outlived the run");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_that_hangs_is_killed_at_its_limit() {
        let dir = std::env::temp_dir().join(format!("mirante-summary-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("slow");
        mirante_core::test_script::write_sh(&bin, "sleep 30", 0o755);
        let t = Instant::now();
        let e = run(&bin, &[], &dir, Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("stopped after"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
