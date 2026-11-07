//! How to launch interactive Claude Code for a task (pure data; spawning is Plan 2b).
//! Rules from docs/spikes/FINDINGS.md §4: use the real binary (skip wrapper shims), scrub
//! nested-session variables but never auth ones, pass `--permission-mode manual`, and add
//! hooks through `--settings` (additive to the user's ~/.claude).
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{CoreError, Result};

/// PATH entries that hold wrapper shims rather than the real `claude` binary.
pub const KNOWN_SHIM_DIRS: [&str; 1] = [".cmuxterm/cmux-cli-shims"];

/// Inherited variables that make a child `claude` believe it is nested inside another
/// Claude Code session (or a terminal wrapper's integration). Auth variables are kept.
///
/// Beyond the obvious session ids/sockets, a few look harmless but are scrubbed on purpose:
/// `CLAUDE_EFFORT` and `AI_AGENT` are set by a parent agent and would silently change the
/// child's reasoning effort / agent-detection behaviour instead of using the user's own
/// configuration, `CLAUDE_PID`/`CMUX_CLAUDE_PID` point at the parent process, and
/// `CLAUDE_CODE_EXECPATH` would make the child re-exec the parent's binary (possibly a shim).
pub const NESTED_SESSION_ENV: [&str; 13] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    "CMUX_CLAUDE_PID",
];

/// Prefixes of terminal-wrapper integration variables, scrubbed when present.
pub const SCRUB_PREFIXES: [&str; 4] = ["CLAUDE_CODE_MESSAGING_", "CMUX_CLAUDE_", "CMUX_AGENT_", "VSCODE_"];

/// The launching terminal's multiplexer / editor identity. The agent runs in Mirante's PTY,
/// not in that terminal: inheriting these makes it talk to the wrong tmux pane or IDE.
/// (`TERM_PROGRAM` itself is overwritten with `mirante` for the agent, see [`AGENT_TERM_ENV`].)
pub const TERMINAL_ENV: [&str; 3] = ["TMUX", "TMUX_PANE", "TERM_PROGRAM_VERSION"];

/// Never scrubbed, whatever the lists above say: credentials and the user's config dir.
pub const PROTECTED_PREFIXES: [&str; 4] = ["ANTHROPIC_", "OPENAI_", "CODEX_", "CLAUDE_CODE_OAUTH"];
pub const PROTECTED_ENV: [&str; 1] = ["CLAUDE_CONFIG_DIR"];

/// What the agent's PTY is: Mirante's own terminal emulation.
pub const AGENT_TERM_ENV: [(&str, &str); 3] =
    [("TERM", "xterm-256color"), ("COLORTERM", "truecolor"), ("TERM_PROGRAM", "mirante")];

/// Hook events registered without a matcher, and tool events registered with `"*"`.
/// `StopFailure` (a turn ended by an API error) and `PostToolUseFailure` (a tool call that
/// failed) are forwarded like the rest; the status engine ignores names it does not know.
pub const PLAIN_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStop",
    "SessionEnd",
    "PreCompact",
];
pub const TOOL_EVENTS: [&str; 4] = ["PreToolUse", "PostToolUse", "PostToolUseFailure", "PermissionRequest"];

/// Env var that scopes the hook to a Mirante task; `mirante-hook` does nothing without it.
pub const TASK_ENV: &str = "MIRANTE_TASK_ID";
/// Env var naming the Mirante session the agent runs in (forwarded by `mirante-hook`).
pub const SESSION_ENV: &str = "MIRANTE_SESSION_ID";

/// Credentials and config that no scrub list may remove.
pub fn is_protected(key: &str) -> bool {
    PROTECTED_ENV.contains(&key) || PROTECTED_PREFIXES.iter().any(|p| key.starts_with(p))
}

fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn is_shim_dir(dir: &Path) -> bool {
    let s = dir.to_string_lossy();
    KNOWN_SHIM_DIRS.iter().any(|shim| s.contains(shim))
}

/// True when `bin`, after following symlinks, lives in a shim directory (a symlink in a
/// clean PATH entry can still point at a wrapper). Unresolvable paths count as shims.
fn resolves_into_shim_dir(bin: &Path) -> bool {
    match std::fs::canonicalize(bin) {
        Ok(real) => real.parent().is_none_or(is_shim_dir),
        Err(_) => true,
    }
}

/// POSIX single-quote escaping: the result is one literal shell word.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The real `claude`: `override_bin` (from `MIRANTE_CLAUDE_BIN`) when set, else the first
/// executable `claude` in `path_var` outside known shim directories.
pub fn resolve_claude_bin(override_bin: Option<&str>, path_var: Option<&str>) -> Result<PathBuf> {
    if let Some(bin) = override_bin.filter(|b| !b.is_empty()) {
        let p = PathBuf::from(bin);
        return if is_executable(&p) {
            Ok(p)
        } else {
            Err(CoreError::ClaudeNotFound(format!("MIRANTE_CLAUDE_BIN={bin} is not an executable file")))
        };
    }
    let path_var = path_var.unwrap_or_default();
    std::env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty() && !is_shim_dir(dir))
        .map(|dir| dir.join("claude"))
        .find(|p| is_executable(p) && !resolves_into_shim_dir(p))
        .ok_or_else(|| CoreError::ClaudeNotFound(format!("no claude outside shim dirs in PATH={path_var}")))
}

/// `resolve_claude_bin` with the process's own `MIRANTE_CLAUDE_BIN` and `PATH`.
pub fn resolve_claude_bin_from_env() -> Result<PathBuf> {
    let over = std::env::var("MIRANTE_CLAUDE_BIN").ok();
    let path = std::env::var("PATH").ok();
    resolve_claude_bin(over.as_deref(), path.as_deref())
}

/// Variables to remove before spawning an agent: the nested-session and terminal lists
/// plus any present variable with a wrapper prefix, never a protected one. `present` is
/// the parent's variable names.
pub fn env_to_scrub<I: IntoIterator<Item = String>>(present: I) -> Vec<String> {
    let mut keys: Vec<String> = NESTED_SESSION_ENV.iter().chain(TERMINAL_ENV.iter()).map(|k| k.to_string()).collect();
    for k in present {
        if SCRUB_PREFIXES.iter().any(|p| k.starts_with(p)) && !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys.retain(|k| !is_protected(k));
    keys
}

/// Variables to remove before spawning the daemon (which outlives the terminal it was
/// started from and spawns every agent): the agent list plus `TERM_PROGRAM`.
pub fn daemon_env_to_scrub<I: IntoIterator<Item = String>>(present: I) -> Vec<String> {
    let mut keys = env_to_scrub(present);
    keys.push("TERM_PROGRAM".to_string());
    keys
}

/// Settings passed with `claude --settings <file>`: the hook program at `hook_cmd` (a path,
/// shell-quoted here because Claude runs hook commands through a shell) for every event and
/// the given allowlist. Additive: the user's ~/.claude hooks, skills and MCP servers still load.
pub fn settings_json(hook_cmd: &str, allow: &[String]) -> Value {
    let handler = json!([{ "type": "command", "command": shell_quote(hook_cmd) }]);
    let mut hooks = serde_json::Map::new();
    for name in PLAIN_EVENTS {
        hooks.insert(name.to_string(), json!([{ "hooks": handler.clone() }]));
    }
    for name in TOOL_EVENTS {
        hooks.insert(name.to_string(), json!([{ "matcher": "*", "hooks": handler.clone() }]));
    }
    json!({ "hooks": Value::Object(hooks), "permissions": { "allow": allow } })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchInput<'a> {
    pub claude_bin: &'a Path,
    pub worktree: &'a Path,
    pub settings_path: &'a Path,
    pub task_id: &'a str,
    /// Mirante's session id (`MIRANTE_SESSION_ID`, echoed back by the hook).
    pub session_id: &'a str,
    /// Native session id to resume (`--resume <id>`), from `Conversation::native_session_id`.
    pub resume: Option<&'a str>,
    /// Names of the variables in the parent environment (for prefix scrubbing).
    pub parent_env: Vec<String>,
}

/// Everything a PTY spawner needs, with no I/O done yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env_remove: Vec<String>,
    pub env_set: Vec<(String, String)>,
}

pub fn launch_spec(input: LaunchInput<'_>) -> LaunchSpec {
    let mut args = vec![
        "--settings".to_string(),
        input.settings_path.to_string_lossy().into_owned(),
        "--permission-mode".to_string(),
        "manual".to_string(),
    ];
    if let Some(id) = input.resume {
        args.push("--resume".to_string());
        args.push(id.to_string());
    }
    LaunchSpec {
        program: input.claude_bin.to_path_buf(),
        args,
        cwd: input.worktree.to_path_buf(),
        env_remove: env_to_scrub(input.parent_env),
        env_set: [(TASK_ENV, input.task_id), (SESSION_ENV, input.session_id)]
            .into_iter()
            .chain(AGENT_TERM_ENV)
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn fake_bin(dir: &Path, name: &str, mode: u32) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    #[test]
    fn skips_shim_dirs_and_non_executables() {
        let d = TempDir::new("launch-path");
        let shim = d.path().join("home/.cmuxterm/cmux-cli-shims");
        let noexec = d.path().join("noexec");
        let real = d.path().join("real/bin");
        fake_bin(&shim, "claude", 0o755);
        fake_bin(&noexec, "claude", 0o644);
        let want = fake_bin(&real, "claude", 0o755);
        let path = std::env::join_paths([&shim, &noexec, &d.path().join("missing"), &real]).unwrap();
        assert_eq!(resolve_claude_bin(None, path.to_str()).unwrap(), want);
    }

    #[test]
    fn override_wins_but_must_be_executable() {
        let d = TempDir::new("launch-override");
        let custom = fake_bin(d.path(), "my-claude", 0o755);
        assert_eq!(resolve_claude_bin(custom.to_str(), Some("")).unwrap(), custom);
        let bad = fake_bin(d.path(), "not-exec", 0o644);
        assert!(matches!(resolve_claude_bin(bad.to_str(), None), Err(CoreError::ClaudeNotFound(_))));
        assert!(matches!(resolve_claude_bin(None, Some("")), Err(CoreError::ClaudeNotFound(_))));
    }

    #[test]
    fn scrubs_nested_and_wrapper_vars_but_never_auth() {
        let present = [
            "CMUX_CLAUDE_HOOKS",
            "CMUX_AGENT_X",
            "CLAUDE_CODE_MESSAGING_FOO",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CONFIG_DIR",
            "HOME",
            "VSCODE_PID",
            "TMUX",
            "TERM_PROGRAM",
            "OPENAI_API_KEY",
            "CODEX_HOME",
        ]
        .map(String::from);
        let keys = env_to_scrub(present.clone());
        for k in [
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CMUX_CLAUDE_HOOKS",
            "CMUX_AGENT_X",
            "CLAUDE_CODE_MESSAGING_FOO",
            "VSCODE_PID",
            "TMUX",
            "TMUX_PANE",
            "TERM_PROGRAM_VERSION",
        ] {
            assert!(keys.iter().any(|x| x == k), "{k} missing");
        }
        let kept = [
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CONFIG_DIR",
            "HOME",
            "OPENAI_API_KEY",
            "CODEX_HOME",
            // Overwritten with `mirante` for the agent, not removed.
            "TERM_PROGRAM",
        ];
        for k in kept {
            assert!(!keys.iter().any(|x| x == k), "{k} must be kept");
        }
        let unique: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
        let daemon = daemon_env_to_scrub(present);
        assert!(daemon.iter().any(|k| k == "TERM_PROGRAM") && daemon.iter().any(|k| k == "VSCODE_PID"));
        assert!(!daemon.iter().any(|k| is_protected(k)));
        assert!(kept[..7].iter().all(|k| !daemon.iter().any(|x| x == k)));
    }

    #[test]
    fn settings_register_every_event_with_the_hook_command() {
        let s = settings_json("/opt/mirante/bin/mirante-hook", &["Bash(cargo test:*)".to_string()]);
        for name in PLAIN_EVENTS.iter().chain(TOOL_EVENTS.iter()) {
            assert_eq!(s["hooks"][*name][0]["hooks"][0]["command"], "'/opt/mirante/bin/mirante-hook'", "{name}");
        }
        assert_eq!(s["hooks"]["PermissionRequest"][0]["matcher"], "*");
        assert_eq!(s["hooks"]["PostToolUseFailure"][0]["matcher"], "*");
        assert!(s["hooks"]["StopFailure"][0].get("matcher").is_none());
        assert!(s["hooks"]["Stop"][0].get("matcher").is_none());
        assert_eq!(s["permissions"]["allow"], json!(["Bash(cargo test:*)"]));
        assert!(s["permissions"].get("defaultMode").is_none());
    }

    #[test]
    fn hook_command_is_shell_quoted() {
        let path = "/Users/o'brien/My Apps/mirante-hook";
        let s = settings_json(path, &[]);
        let cmd = s["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, r#"'/Users/o'\''brien/My Apps/mirante-hook'"#);
        // A real shell must read it back as exactly one word equal to the path.
        let out = std::process::Command::new("sh").args(["-c", &format!("printf '%s' {cmd}")]).output().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), path);
        assert_eq!(shell_quote("$(x) `y`"), "'$(x) `y`'");
    }

    #[test]
    fn symlink_into_a_shim_dir_is_skipped() {
        let d = TempDir::new("launch-symlink");
        let shim = d.path().join("home/.cmuxterm/cmux-cli-shims");
        let clean = d.path().join("clean");
        let real = d.path().join("real");
        let shim_bin = fake_bin(&shim, "claude", 0o755);
        std::fs::create_dir_all(&clean).unwrap();
        std::os::unix::fs::symlink(&shim_bin, clean.join("claude")).unwrap();
        let want = fake_bin(&real, "claude", 0o755);
        let path = std::env::join_paths([&clean, &real]).unwrap();
        assert_eq!(resolve_claude_bin(None, path.to_str()).unwrap(), want);
    }

    #[test]
    fn launch_spec_for_new_and_resumed_sessions() {
        let input = LaunchInput {
            claude_bin: Path::new("/usr/local/bin/claude"),
            worktree: Path::new("/wt/app/feat"),
            settings_path: Path::new("/m/settings/t1.json"),
            task_id: "t1",
            session_id: "s1",
            resume: None,
            parent_env: vec!["CMUX_AGENT_Y".into()],
        };
        let spec = launch_spec(input.clone());
        assert_eq!(spec.program, PathBuf::from("/usr/local/bin/claude"));
        assert_eq!(spec.args, vec!["--settings", "/m/settings/t1.json", "--permission-mode", "manual"]);
        assert_eq!(spec.cwd, PathBuf::from("/wt/app/feat"));
        assert!(spec.env_remove.contains(&"CMUX_AGENT_Y".to_string()));
        let env: Vec<(&str, &str)> = spec.env_set.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(
            env,
            [
                ("MIRANTE_TASK_ID", "t1"),
                ("MIRANTE_SESSION_ID", "s1"),
                ("TERM", "xterm-256color"),
                ("COLORTERM", "truecolor"),
                ("TERM_PROGRAM", "mirante"),
            ]
        );
        assert!(!spec.env_remove.iter().any(|k| spec.env_set.iter().any(|(s, _)| s == k)));
        assert!(!spec.args.iter().any(|a| a.contains("dangerously") || a == "--bare" || a == "-p"));

        let resumed = launch_spec(LaunchInput { resume: Some("sess-9"), ..input });
        assert_eq!(&resumed.args[4..], ["--resume", "sess-9"]);
    }
}
