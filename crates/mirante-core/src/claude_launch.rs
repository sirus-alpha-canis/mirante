//! How to launch interactive Claude Code for a task (pure data; spawning is Plan 2b).
//! Rules from docs/spikes/FINDINGS.md §4: use the real binary (skip wrapper shims), scrub
//! nested-session variables but never auth ones, pass `--permission-mode manual`, and add
//! hooks through `--settings` (additive to the user's ~/.claude).
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{CoreError, Result};

pub use crate::agent_bin::KNOWN_SHIM_DIRS;
use crate::agent_bin::{self, Agent, Lookup};

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
/// Env var every Mirante-launched agent (Claude and Codex) runs with, set to its task id.
/// Children inherit it, so a process that left the agent's process tree (double fork,
/// `setsid`, a tmux server, `cd /`) still carries it: the daemon reads it from a socket
/// peer's environment to refuse memory review from Mirante-launched agents and to tie a
/// proposal to its task. Defense in depth, not a boundary: a process that scrubs its
/// environment before connecting loses it.
pub const AGENT_ENV: &str = "MIRANTE_TASK";

/// Credentials and config that no scrub list may remove.
pub fn is_protected(key: &str) -> bool {
    PROTECTED_ENV.contains(&key) || PROTECTED_PREFIXES.iter().any(|p| key.starts_with(p))
}

/// POSIX single-quote escaping: the result is one literal shell word.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The real `claude` from an explicit override and PATH only (no well-known directories):
/// `override_bin` (from `MIRANTE_CLAUDE_BIN`) when set, else the first executable `claude`
/// in `path_var` outside known shim directories.
pub fn resolve_claude_bin(override_bin: Option<&str>, path_var: Option<&str>) -> Result<PathBuf> {
    let l = Lookup {
        override_bin: override_bin.map(str::to_string),
        path_var: path_var.map(str::to_string),
        home: None,
        well_known: false,
    };
    agent_bin::resolve(Agent::Claude, &l).map(|r| r.path).map_err(CoreError::ClaudeNotFound)
}

/// The real `claude` for this process: `MIRANTE_CLAUDE_BIN`, PATH, then the well-known
/// install directories (see [`agent_bin`]).
pub fn resolve_claude_bin_from_env() -> Result<PathBuf> {
    agent_bin::resolve(Agent::Claude, &Lookup::from_env(Agent::Claude))
        .map(|r| r.path)
        .map_err(CoreError::ClaudeNotFound)
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

/// Name of Mirante's MCP server in an agent's config. Claude's tool ids are
/// `mcp__mirante-memory__<tool>` (a hyphen is kept as is in tool ids and permission rules).
/// Not plain `mirante`, so a server of that name in the user's own Claude or Codex config
/// is less likely to collide with it (Codex merges `-c` overrides into a same-named entry).
pub const MCP_SERVER: &str = "mirante-memory";

/// The per-task MCP config passed with `claude --mcp-config <file>`: one stdio server,
/// `mirante-mcp` at `bin`, scoped to `task` and to the daemon under `home`, carrying the
/// agent marker ([`AGENT_ENV`]) whatever the client passes down. Additive like
/// `--settings`: the user's own MCP servers still load (no `--strict-mcp-config`), and
/// nothing is written to `~/.claude`.
pub fn mcp_json(bin: &Path, task: &str, home: &Path) -> Value {
    json!({ "mcpServers": { MCP_SERVER: {
        "type": "stdio",
        "command": bin.to_string_lossy(),
        "args": [],
        "env": { TASK_ENV: task, "MIRANTE_HOME": home.to_string_lossy(), AGENT_ENV: task },
    }}})
}

/// The memory tools Claude may call without asking: the read-only ones. `memory_propose`
/// is not here: Claude asks the user once per call, and the proposal still goes to review.
pub fn memory_allow() -> Vec<String> {
    ["memory_search", "memory_get"].iter().map(|t| format!("mcp__{MCP_SERVER}__{t}")).collect()
}

/// The name of the per-session plugin Mirante generates for a task's on-demand skills;
/// Claude invokes them as `mirante-skills:<name>`.
pub const PLUGIN_NAME: &str = "mirante-skills";

/// `.claude-plugin/plugin.json` of that plugin: a name, a version and a description,
/// nothing else (no hooks, MCP servers, commands or agents: the plugin only carries
/// `skills/`).
pub fn plugin_json() -> Value {
    json!({ "name": PLUGIN_NAME, "version": "0.0.0", "description": "Skills Mirante injected for this task" })
}

/// How the always-on skills reach Claude's system prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlwaysOn<'a> {
    /// `--append-system-prompt-file <path>` (a flag Claude 2.1.285 accepts but does not
    /// list in `--help`).
    File(&'a Path),
    /// `--append-system-prompt <text>`, for a Claude that refuses the file flag.
    Inline(&'a str),
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
    /// The per-task MCP config ([`mcp_json`]), only for repos with memory enabled.
    pub mcp_config: Option<&'a Path>,
    /// The per-session plugin holding the on-demand skills (`--plugin-dir`).
    pub plugin_dir: Option<&'a Path>,
    /// The always-on skills' text.
    pub always: Option<AlwaysOn<'a>>,
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
    let mut args = vec!["--settings".to_string(), input.settings_path.to_string_lossy().into_owned()];
    // Skills before `--mcp-config`, which is variadic (see below).
    if let Some(dir) = input.plugin_dir {
        args.push("--plugin-dir".to_string());
        args.push(dir.to_string_lossy().into_owned());
    }
    match input.always {
        Some(AlwaysOn::File(f)) => {
            args.push("--append-system-prompt-file".to_string());
            args.push(f.to_string_lossy().into_owned());
        }
        Some(AlwaysOn::Inline(text)) => {
            args.push("--append-system-prompt".to_string());
            args.push(text.to_string());
        }
        None => {}
    }
    if let Some(mcp) = input.mcp_config {
        // Variadic in Claude's CLI: every argument after it must be a `--flag value` pair,
        // or the value would be read as another config.
        args.push("--mcp-config".to_string());
        args.push(mcp.to_string_lossy().into_owned());
    }
    args.push("--permission-mode".to_string());
    args.push("manual".to_string());
    if let Some(id) = input.resume {
        args.push("--resume".to_string());
        args.push(id.to_string());
    }
    LaunchSpec {
        program: input.claude_bin.to_path_buf(),
        args,
        cwd: input.worktree.to_path_buf(),
        env_remove: env_to_scrub(input.parent_env),
        env_set: [(TASK_ENV, input.task_id), (SESSION_ENV, input.session_id), (AGENT_ENV, input.task_id)]
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
    use std::os::unix::fs::PermissionsExt;

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
            mcp_config: None,
            plugin_dir: None,
            always: None,
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
                ("MIRANTE_TASK", "t1"),
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

    fn input() -> LaunchInput<'static> {
        LaunchInput {
            claude_bin: Path::new("/usr/local/bin/claude"),
            worktree: Path::new("/wt/app/feat"),
            settings_path: Path::new("/m/settings/t1.json"),
            task_id: "t1",
            session_id: "s1",
            resume: Some("sess-9"),
            parent_env: vec![],
            mcp_config: None,
            plugin_dir: None,
            always: None,
        }
    }

    #[test]
    fn mcp_json_scopes_the_server_to_the_task() {
        let v = mcp_json(Path::new("/opt/m/mirante-mcp"), "t1", Path::new("/h/.mirante"));
        assert_eq!(v["mcpServers"]["mirante-memory"]["type"], "stdio");
        assert_eq!(v["mcpServers"]["mirante-memory"]["command"], "/opt/m/mirante-mcp");
        assert_eq!(v["mcpServers"]["mirante-memory"]["args"], json!([]));
        assert_eq!(v["mcpServers"]["mirante-memory"]["env"]["MIRANTE_TASK_ID"], "t1");
        assert_eq!(v["mcpServers"]["mirante-memory"]["env"]["MIRANTE_HOME"], "/h/.mirante");
        assert_eq!(v["mcpServers"]["mirante-memory"]["env"]["MIRANTE_TASK"], "t1");
        assert_eq!(v["mcpServers"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn launch_with_mcp_config_adds_one_flag_never_strict_and_nothing_positional_after() {
        let spec = launch_spec(LaunchInput { mcp_config: Some(Path::new("/m/s/t1/mcp.json")), ..input() });
        let i = spec.args.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!(spec.args[i - 2..i], ["--settings", "/m/settings/t1.json"]);
        assert_eq!(spec.args[i + 1], "/m/s/t1/mcp.json");
        // `--mcp-config` is variadic: only `--flag value` pairs may follow it.
        assert_eq!((spec.args.len() - i) % 2, 0, "{:?}", spec.args);
        assert!(spec.args[i + 2..].iter().step_by(2).all(|a| a.starts_with("--")), "{:?}", spec.args);
        assert_eq!(spec.args.iter().filter(|a| *a == "--mcp-config").count(), 1);
        assert!(!spec.args.iter().any(|a| a == "--strict-mcp-config"));
        assert!(!launch_spec(input()).args.iter().any(|a| a == "--mcp-config"));
    }

    #[test]
    fn memory_read_tools_are_pre_allowed_and_propose_is_not() {
        let s = settings_json("/h", &memory_allow());
        assert_eq!(
            s["permissions"]["allow"],
            json!(["mcp__mirante-memory__memory_search", "mcp__mirante-memory__memory_get"])
        );
        assert!(!memory_allow().iter().any(|t| t.contains("propose")));
    }
    /// `--mcp-config` is variadic: after it, only `--flag value` pairs.
    fn assert_pairs_after_mcp(args: &[String]) {
        let i = args.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!((args.len() - i) % 2, 0, "{args:?}");
        assert!(args[i + 2..].iter().step_by(2).all(|a| a.starts_with("--")), "{args:?}");
    }

    #[test]
    fn plugin_dir_and_always_file_flags_come_before_mcp_config_and_are_pairs() {
        let spec = launch_spec(LaunchInput {
            mcp_config: Some(Path::new("/m/s/t1/mcp.json")),
            plugin_dir: Some(Path::new("/m/s/t1/skills/s1/claude-plugin")),
            always: Some(AlwaysOn::File(Path::new("/m/s/t1/skills/s1/always.md"))),
            ..input()
        });
        let at = |f: &str| spec.args.iter().position(|a| a == f).unwrap_or_else(|| panic!("{f}: {:?}", spec.args));
        let (plugin, always, mcp) = (at("--plugin-dir"), at("--append-system-prompt-file"), at("--mcp-config"));
        assert_eq!(spec.args[plugin + 1], "/m/s/t1/skills/s1/claude-plugin");
        assert_eq!(spec.args[always + 1], "/m/s/t1/skills/s1/always.md");
        assert!(plugin < mcp && always < mcp, "{:?}", spec.args);
        assert_eq!(spec.args.iter().filter(|a| *a == "--plugin-dir").count(), 1);
        assert_pairs_after_mcp(&spec.args);
        // Every flag before `--mcp-config` is a pair too: nothing positional anywhere.
        assert!(spec.args.iter().step_by(2).all(|a| a.starts_with("--")), "{:?}", spec.args);

        // The inline fallback (a Claude without the hidden flag) is a pair as well.
        let text = "<mirante-skills>\n--not-a-flag\n</mirante-skills>\n";
        let spec = launch_spec(LaunchInput {
            mcp_config: Some(Path::new("/m/mcp.json")),
            always: Some(AlwaysOn::Inline(text)),
            ..input()
        });
        let i = spec.args.iter().position(|a| a == "--append-system-prompt").unwrap();
        assert_eq!(spec.args[i + 1], text);
        assert!(i < spec.args.iter().position(|a| a == "--mcp-config").unwrap());
        assert!(!spec.args.iter().any(|a| a == "--append-system-prompt-file"));
        assert_pairs_after_mcp(&spec.args);
    }

    #[test]
    fn no_skills_no_flags() {
        for spec in [launch_spec(input()), launch_spec(LaunchInput { mcp_config: Some(Path::new("/m")), ..input() })] {
            assert!(
                !spec.args.iter().any(|a| a == "--plugin-dir" || a.starts_with("--append-system-prompt")),
                "{:?}",
                spec.args
            );
        }
    }

    #[test]
    fn plugin_json_names_mirante_skills() {
        let v = plugin_json();
        assert_eq!(v["name"], "mirante-skills");
        assert_eq!(v["name"], PLUGIN_NAME);
        assert_eq!(v["version"], "0.0.0");
        assert_eq!(v["description"], "Skills Mirante injected for this task");
        // Nothing that would load code or grant anything: no hooks, MCP servers, commands
        // or agents declared.
        assert_eq!(v.as_object().unwrap().len(), 3, "{v}");
    }
}
