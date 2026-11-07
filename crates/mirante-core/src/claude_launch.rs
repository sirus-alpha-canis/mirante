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
pub const SCRUB_PREFIXES: [&str; 3] = ["CLAUDE_CODE_MESSAGING_", "CMUX_CLAUDE_", "CMUX_AGENT_"];

/// Hook events registered without a matcher, and tool events registered with `"*"`.
pub const PLAIN_EVENTS: [&str; 7] =
    ["SessionStart", "UserPromptSubmit", "Notification", "Stop", "SubagentStop", "SessionEnd", "PreCompact"];
pub const TOOL_EVENTS: [&str; 3] = ["PreToolUse", "PostToolUse", "PermissionRequest"];

/// Env var that scopes the hook to a Mirante task; `mirante-hook` does nothing without it.
pub const TASK_ENV: &str = "MIRANTE_TASK_ID";

fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn is_shim_dir(dir: &Path) -> bool {
    let s = dir.to_string_lossy();
    KNOWN_SHIM_DIRS.iter().any(|shim| s.contains(shim))
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
        .find(|p| is_executable(p))
        .ok_or_else(|| CoreError::ClaudeNotFound(format!("no claude outside shim dirs in PATH={path_var}")))
}

/// `resolve_claude_bin` with the process's own `MIRANTE_CLAUDE_BIN` and `PATH`.
pub fn resolve_claude_bin_from_env() -> Result<PathBuf> {
    let over = std::env::var("MIRANTE_CLAUDE_BIN").ok();
    let path = std::env::var("PATH").ok();
    resolve_claude_bin(over.as_deref(), path.as_deref())
}

/// Variables to remove before spawning: the nested-session list plus any present
/// variable with a wrapper prefix. `present` is the parent's variable names.
pub fn env_to_scrub<I: IntoIterator<Item = String>>(present: I) -> Vec<String> {
    let mut keys: Vec<String> = NESTED_SESSION_ENV.iter().map(|k| k.to_string()).collect();
    for k in present {
        if SCRUB_PREFIXES.iter().any(|p| k.starts_with(p)) && !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

/// Settings passed with `claude --settings <file>`: `hook_cmd` for every event and the
/// given allowlist. Additive: the user's ~/.claude hooks, skills and MCP servers still load.
pub fn settings_json(hook_cmd: &str, allow: &[String]) -> Value {
    let handler = json!([{ "type": "command", "command": hook_cmd }]);
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
        env_set: vec![(TASK_ENV.to_string(), input.task_id.to_string())],
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
        ]
        .map(String::from);
        let keys = env_to_scrub(present);
        for k in
            ["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CMUX_CLAUDE_HOOKS", "CMUX_AGENT_X", "CLAUDE_CODE_MESSAGING_FOO"]
        {
            assert!(keys.iter().any(|x| x == k), "{k} missing");
        }
        for k in
            ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CONFIG_DIR", "HOME"]
        {
            assert!(!keys.iter().any(|x| x == k), "{k} must be kept");
        }
        let unique: std::collections::HashSet<&String> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }

    #[test]
    fn settings_register_every_event_with_the_hook_command() {
        let s = settings_json("/opt/mirante/bin/mirante-hook", &["Bash(cargo test:*)".to_string()]);
        for name in PLAIN_EVENTS.iter().chain(TOOL_EVENTS.iter()) {
            assert_eq!(s["hooks"][*name][0]["hooks"][0]["command"], "/opt/mirante/bin/mirante-hook", "{name}");
        }
        assert_eq!(s["hooks"]["PermissionRequest"][0]["matcher"], "*");
        assert!(s["hooks"]["Stop"][0].get("matcher").is_none());
        assert_eq!(s["permissions"]["allow"], json!(["Bash(cargo test:*)"]));
        assert!(s["permissions"].get("defaultMode").is_none());
    }

    #[test]
    fn launch_spec_for_new_and_resumed_sessions() {
        let input = LaunchInput {
            claude_bin: Path::new("/usr/local/bin/claude"),
            worktree: Path::new("/wt/app/feat"),
            settings_path: Path::new("/m/settings/t1.json"),
            task_id: "t1",
            resume: None,
            parent_env: vec!["CMUX_AGENT_Y".into()],
        };
        let spec = launch_spec(input.clone());
        assert_eq!(spec.program, PathBuf::from("/usr/local/bin/claude"));
        assert_eq!(spec.args, vec!["--settings", "/m/settings/t1.json", "--permission-mode", "manual"]);
        assert_eq!(spec.cwd, PathBuf::from("/wt/app/feat"));
        assert!(spec.env_remove.contains(&"CMUX_AGENT_Y".to_string()));
        assert_eq!(spec.env_set, vec![("MIRANTE_TASK_ID".to_string(), "t1".to_string())]);
        assert!(!spec.args.iter().any(|a| a.contains("dangerously") || a == "--bare" || a == "-p"));

        let resumed = launch_spec(LaunchInput { resume: Some("sess-9"), ..input });
        assert_eq!(&resumed.args[4..], ["--resume", "sess-9"]);
    }
}
