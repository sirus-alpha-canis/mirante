# Mirante — Week 1 Spikes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bootstrap the Mirante repo and answer the four week-1 risk questions with measured evidence. The questions are S1 mid-run messaging, S2 quota, S3 hook status and S4 resume. The daemon/app plan (Plan 2) is written from those answers.

**Architecture:**
- One Rust workspace with a single `spike-kit` crate.
- **Pure logic** (hook event model, status mapping, delivery classification, transcript usage, Codex capture) is unit-tested with TDD.
- **Thin binaries drive the real CLIs:**
  - `mirante-hook`: the hook sink that Claude Code calls.
  - `spike`: utilities.
  - `spike-claude-stream`: headless `claude -p` with stream-json.
  - `spike-claude-pty`: interactive `claude` in a PTY with hooks.
  - `spike-codex`: a scripted `codex app-server` JSON-RPC driver.
- Results go to `docs/spikes/FINDINGS.md`.

**Tech Stack:** Rust (stable), serde/serde_json, anyhow, portable-pty 0.8. Claude Code ≥ 2.1, Codex CLI (`codex app-server`).

**Spec:** `docs/spec/2026-11-07-cockpit-prototype-design.md` (copied in Task 1).

**Safety rule for every spike:**
- Spikes run only inside a throwaway sandbox repo (`~/.mirante-spike/sandbox`).
- Tool permissions are limited to an explicit allowlist (`Bash(sleep:*)`, `Bash(echo:*)`).
- Never use `--dangerously-skip-permissions`.

---

## File structure

```
mirante/
  Cargo.toml                      # workspace
  .gitignore
  LICENSE                         # Apache-2.0
  THIRD_PARTY_NOTICES.md
  README.md
  docs/spec/2026-11-07-cockpit-prototype-design.md
  docs/plans/2026-11-07-week1-spikes.md   # this file
  docs/spikes/FINDINGS.md         # results + go/no-go per spike
  scripts/make-sandbox.sh         # throwaway git repo for spikes
  scripts/codex-steer.jsonl       # JSON-RPC script for S1/Codex
  crates/spike-kit/
    Cargo.toml
    src/lib.rs
    src/util.rs                   # now_ms, jsonl read/write
    src/hook.rs                   # HookEvent, LoggedEvent, settings_json
    src/status.rs                 # Status, next_status, timeline
    src/delivery.rs               # Delivery, classify_hooks/stream/codex
    src/transcript.rs             # Usage, sum_transcript, last_assistant_text
    src/stream.rs                 # user_line (stream-json input)
    src/codex.rs                  # capture ids, fill script templates
    src/bin/mirante-hook.rs
    src/bin/spike.rs
    src/bin/spike-claude-stream.rs
    src/bin/spike-claude-pty.rs
    src/bin/spike-codex.rs
```

---

### Task 0: Toolchain prerequisites (founder runs these)

**Files:** none

- [ ] **Step 1: Install a current Rust via rustup.** Homebrew's rust 1.81 is too old for some current crates and for Tauri later.

```bash
brew uninstall rust 2>/dev/null; brew install rustup && rustup-init -y && source "$HOME/.cargo/env"
rustc --version
```
Expected: `rustc 1.9x.x` (any current stable).

- [ ] **Step 2: Install and log in to the Codex CLI.** S1 and S4 for Codex need it.

```bash
npm install -g @openai/codex && codex login && codex --version
```
Expected: a version string. If the founder skips Codex for now, Tasks 10–11 are deferred and the findings must say so.

---

> **Note (2026-11-07):** the library code in Tasks 1–6 below is the original plan. Commit `407361e` supersedes it: atomic JSONL appends, a `Delivery::Inconclusive` variant, events sorted by timestamp, per-field max token dedupe, structural Codex matching, `codex::capture_last`, and `fill` returning `Result`. The source in `crates/spike-kit` is authoritative; do not re-apply these code blocks.

### Task 1: Bootstrap the repo

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `LICENSE`, `THIRD_PARTY_NOTICES.md`, `README.md`, `docs/spec/2026-11-07-cockpit-prototype-design.md`, `crates/spike-kit/Cargo.toml`, `crates/spike-kit/src/lib.rs`, `crates/spike-kit/src/util.rs`

- [ ] **Step 1: Workspace and crate manifests**

`Cargo.toml`:
```toml
[workspace]
members = ["crates/spike-kit"]
resolver = "2"
```

`crates/spike-kit/Cargo.toml`:
```toml
[package]
name = "spike-kit"
version = "0.1.0"
edition = "2021"
publish = false
license = "Apache-2.0"

[dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
portable-pty = "0.8"
```

`.gitignore`:
```
/target
*.log
.DS_Store
```

- [ ] **Step 2: License, notices, README, spec copy**

```bash
cd ~/Documents/Develop/mirante
curl -sL https://www.apache.org/licenses/LICENSE-2.0.txt -o LICENSE
mkdir -p docs/spec docs/spikes scripts
cp ../projectDNA/docs/superpowers/specs/2026-11-07-cockpit-prototype-design.md docs/spec/
```

`THIRD_PARTY_NOTICES.md`:
```markdown
# Third-party notices

Mirante's own code is licensed under Apache-2.0 (see LICENSE).

No third-party source code is included. Design patterns were studied from
MIT/Apache projects (T3 Code, Paradev, Emdash, vibe-kanban, opencode) and
reimplemented from prose descriptions. GPL/AGPL/BUSL projects were used for
ideas only. Dependencies are listed in Cargo.lock under their own licenses.
```

`README.md`:
```markdown
# Mirante

Local-first cockpit for coding agents: dispatch Claude Code and Codex in parallel,
see every task's real status, talk to any agent mid-run, and never lose context.

Status: week-1 spikes. See `docs/spec/` and `docs/spikes/FINDINGS.md`.
```

- [ ] **Step 3: lib.rs and util.rs with a failing test**

`crates/spike-kit/src/lib.rs`:
```rust
pub mod util;
```

`crates/spike-kit/src/util.rs`:
```rust
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn append_jsonl(path: &Path, value: &Value) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{value}")?;
    Ok(())
}

pub fn read_jsonl(path: &Path) -> anyhow::Result<Vec<Value>> {
    Ok(parse_jsonl(&fs::read_to_string(path)?))
}

pub fn parse_jsonl(text: &str) -> Vec<Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jsonl_skips_blank_and_invalid_lines() {
        let v = parse_jsonl("{\"a\":1}\n\nnot json\n{\"a\":2}\n");
        assert_eq!(v.len(), 2);
        assert_eq!(v[1]["a"], 2);
    }

    #[test]
    fn append_then_read_roundtrips() {
        let path = std::env::temp_dir().join(format!("mirante-util-{}.jsonl", now_ms()));
        append_jsonl(&path, &serde_json::json!({"x": 1})).unwrap();
        append_jsonl(&path, &serde_json::json!({"x": 2})).unwrap();
        let v = read_jsonl(&path).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["x"], 1);
        let _ = std::fs::remove_file(path);
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p spike-kit`
Expected: `test result: ok. 2 passed`

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "chore: bootstrap mirante workspace, license, spec and spike-kit util"
```

---

### Task 2: Hook event model and Claude settings generator

**Files:**
- Create: `crates/spike-kit/src/hook.rs`
- Modify: `crates/spike-kit/src/lib.rs`

- [ ] **Step 1: Write the module with failing tests**

`crates/spike-kit/src/lib.rs`:
```rust
pub mod hook;
pub mod util;
```

`crates/spike-kit/src/hook.rs`:
```rust
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Subset of the JSON Claude Code writes to a hook's stdin. Unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookEvent {
    pub hook_event_name: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedEvent {
    pub ts_ms: u64,
    #[serde(default)]
    pub task_id: Option<String>,
    pub event: HookEvent,
}

pub const PLAIN_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionEnd",
    "PreCompact",
];
pub const TOOL_EVENTS: [&str; 3] = ["PreToolUse", "PostToolUse", "PermissionRequest"];

/// Settings file passed with `claude --settings <file>`: registers `hook_cmd` for every
/// event and allows only the listed tool patterns. Global ~/.claude config is untouched.
pub fn settings_json(hook_cmd: &str, allow: &[&str]) -> Value {
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

/// What `mirante-hook` appends to the log: a LoggedEvent, or the raw text if unparseable.
pub fn logged_from_stdin(raw: &str, task_id: Option<String>, ts_ms: u64) -> Value {
    match serde_json::from_str::<HookEvent>(raw) {
        Ok(event) => serde_json::to_value(LoggedEvent { ts_ms, task_id, event }).unwrap_or(Value::Null),
        Err(_) => json!({ "ts_ms": ts_ms, "task_id": task_id, "raw": raw }),
    }
}

pub fn load_events(lines: &[Value], task_id: Option<&str>) -> Vec<LoggedEvent> {
    lines
        .iter()
        .filter_map(|v| serde_json::from_value::<LoggedEvent>(v.clone()).ok())
        .filter(|e| task_id.map_or(true, |t| e.task_id.as_deref() == Some(t)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hook_payload_and_ignores_unknown_fields() {
        let raw = r#"{"hook_event_name":"Stop","session_id":"s1","transcript_path":"/t.jsonl","stop_hook_active":false}"#;
        let e: LoggedEvent = serde_json::from_value(logged_from_stdin(raw, Some("t1".into()), 42)).unwrap();
        assert_eq!(e.event.hook_event_name, "Stop");
        assert_eq!(e.event.session_id.as_deref(), Some("s1"));
        assert_eq!(e.event.transcript_path.as_deref(), Some("/t.jsonl"));
        assert_eq!(e.task_id.as_deref(), Some("t1"));
        assert_eq!(e.ts_ms, 42);
    }

    #[test]
    fn keeps_raw_text_when_payload_is_not_a_hook_event() {
        let v = logged_from_stdin("not json", None, 7);
        assert_eq!(v["raw"], "not json");
        assert_eq!(v["ts_ms"], 7);
    }

    #[test]
    fn settings_register_every_event_with_the_hook_command() {
        let s = settings_json("/bin/mirante-hook", &["Bash(sleep:*)"]);
        for name in PLAIN_EVENTS.iter().chain(TOOL_EVENTS.iter()) {
            assert_eq!(s["hooks"][*name][0]["hooks"][0]["command"], "/bin/mirante-hook", "{name}");
        }
        assert_eq!(s["hooks"]["PreToolUse"][0]["matcher"], "*");
        assert!(s["hooks"]["Stop"][0].get("matcher").is_none());
        assert_eq!(s["permissions"]["allow"][0], "Bash(sleep:*)");
    }

    #[test]
    fn load_events_filters_by_task_and_skips_raw_lines() {
        let lines = vec![
            logged_from_stdin(r#"{"hook_event_name":"Stop"}"#, Some("a".into()), 1),
            logged_from_stdin(r#"{"hook_event_name":"Stop"}"#, Some("b".into()), 2),
            logged_from_stdin("garbage", Some("a".into()), 3),
        ];
        assert_eq!(load_events(&lines, Some("a")).len(), 1);
        assert_eq!(load_events(&lines, None).len(), 2);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spike-kit hook`
Expected: `4 passed`

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(spike-kit): hook event model and scoped claude settings generator"
```

---

### Task 3: Status mapping and timeline (S3 logic)

**Files:**
- Create: `crates/spike-kit/src/status.rs`
- Modify: `crates/spike-kit/src/lib.rs`

- [ ] **Step 1: Write module with failing tests**

`crates/spike-kit/src/lib.rs`:
```rust
pub mod hook;
pub mod status;
pub mod util;
```

`crates/spike-kit/src/status.rs`:
```rust
use serde::Serialize;

use crate::hook::{HookEvent, LoggedEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Working,
    NeedsYou,
    Ended,
}

/// Deterministic mapping from one hook event to the task status. Unknown or
/// neutral events (SubagentStop, PreCompact, anything new) keep the current status.
pub fn next_status(current: Option<Status>, ev: &HookEvent) -> Option<Status> {
    match ev.hook_event_name.as_str() {
        "SessionStart" => Some(Status::Idle),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(Status::Working),
        "PermissionRequest" | "Notification" => Some(Status::NeedsYou),
        "Stop" => Some(Status::Idle),
        "SessionEnd" => Some(Status::Ended),
        _ => current,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub ts_ms: u64,
    pub event: String,
    pub status: Status,
}

/// Only the events that changed the status.
pub fn timeline(events: &[LoggedEvent]) -> Vec<Transition> {
    let mut current = None;
    let mut out = Vec::new();
    for e in events {
        let next = next_status(current, &e.event);
        if next != current {
            if let Some(status) = next {
                out.push(Transition { ts_ms: e.ts_ms, event: e.event.hook_event_name.clone(), status });
            }
            current = next;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(name: &str) -> HookEvent {
        HookEvent {
            hook_event_name: name.into(),
            session_id: None,
            transcript_path: None,
            source: None,
            message: None,
            tool_name: None,
        }
    }

    fn logged(ts: u64, name: &str) -> LoggedEvent {
        LoggedEvent { ts_ms: ts, task_id: None, event: ev(name) }
    }

    #[test]
    fn maps_each_known_event() {
        assert_eq!(next_status(None, &ev("SessionStart")), Some(Status::Idle));
        assert_eq!(next_status(None, &ev("UserPromptSubmit")), Some(Status::Working));
        assert_eq!(next_status(None, &ev("PreToolUse")), Some(Status::Working));
        assert_eq!(next_status(None, &ev("PostToolUse")), Some(Status::Working));
        assert_eq!(next_status(None, &ev("PermissionRequest")), Some(Status::NeedsYou));
        assert_eq!(next_status(None, &ev("Notification")), Some(Status::NeedsYou));
        assert_eq!(next_status(None, &ev("Stop")), Some(Status::Idle));
        assert_eq!(next_status(None, &ev("SessionEnd")), Some(Status::Ended));
    }

    #[test]
    fn neutral_and_unknown_events_keep_current_status() {
        assert_eq!(next_status(Some(Status::Working), &ev("SubagentStop")), Some(Status::Working));
        assert_eq!(next_status(Some(Status::Working), &ev("PreCompact")), Some(Status::Working));
        assert_eq!(next_status(Some(Status::Idle), &ev("SomethingNew")), Some(Status::Idle));
    }

    #[test]
    fn timeline_keeps_only_changes() {
        let events = vec![
            logged(1, "SessionStart"),
            logged(2, "UserPromptSubmit"),
            logged(3, "PreToolUse"),
            logged(4, "PostToolUse"),
            logged(5, "Notification"),
            logged(6, "PostToolUse"),
            logged(7, "Stop"),
            logged(8, "SessionEnd"),
        ];
        let t: Vec<(u64, Status)> = timeline(&events).iter().map(|t| (t.ts_ms, t.status)).collect();
        assert_eq!(
            t,
            vec![
                (1, Status::Idle),
                (2, Status::Working),
                (5, Status::NeedsYou),
                (6, Status::Working),
                (7, Status::Idle),
                (8, Status::Ended),
            ]
        );
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spike-kit status`
Expected: `3 passed`

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(spike-kit): deterministic hook->status mapping and timeline"
```

---

### Task 4: Delivery classification and stream-json input (S1 logic)

**Files:**
- Create: `crates/spike-kit/src/delivery.rs`, `crates/spike-kit/src/stream.rs`
- Modify: `crates/spike-kit/src/lib.rs`

- [ ] **Step 1: Write modules with failing tests**

`crates/spike-kit/src/lib.rs`:
```rust
pub mod delivery;
pub mod hook;
pub mod status;
pub mod stream;
pub mod util;
```

`crates/spike-kit/src/stream.rs`:
```rust
use serde_json::json;

/// One line of `claude -p --input-format stream-json` input.
pub fn user_line(text: &str) -> String {
    json!({
        "type": "user",
        "message": { "role": "user", "content": [ { "type": "text", "text": text } ] }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_line_is_single_line_json_with_text() {
        let l = user_line("oi\nmundo");
        assert!(!l.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"][0]["text"], "oi\nmundo");
    }
}
```

`crates/spike-kit/src/delivery.rs`:
```rust
use serde_json::Value;

use crate::hook::LoggedEvent;

/// How a second message, sent while the first turn was still running, was delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Acted on before the first turn ended (true mid-run steering).
    Steered,
    /// Acted on only after the first turn ended.
    Queued,
    /// Never acted on.
    Never,
}

fn decide(marker_at: Option<u64>, first_end: u64) -> Delivery {
    match marker_at {
        None => Delivery::Never,
        Some(t) if t < first_end => Delivery::Steered,
        Some(_) => Delivery::Queued,
    }
}

/// Interactive PTY session: was the 2nd UserPromptSubmit before the 1st Stop?
pub fn classify_hooks(events: &[LoggedEvent]) -> Delivery {
    let prompts: Vec<u64> = events
        .iter()
        .filter(|e| e.event.hook_event_name == "UserPromptSubmit")
        .map(|e| e.ts_ms)
        .collect();
    if prompts.len() < 2 {
        return Delivery::Never;
    }
    let first_stop = events
        .iter()
        .filter(|e| e.event.hook_event_name == "Stop" && e.ts_ms > prompts[0])
        .map(|e| e.ts_ms)
        .min()
        .unwrap_or(u64::MAX);
    if prompts[1] < first_stop {
        Delivery::Steered
    } else {
        Delivery::Queued
    }
}

/// Headless stream-json log ({ts, dir, line}): did an assistant message containing
/// `marker` appear before the first `result` event?
pub fn classify_stream(log: &[Value], marker: &str) -> Delivery {
    let out = |v: &&Value| v["dir"] == "out";
    let first_result = log
        .iter()
        .filter(out)
        .filter(|v| v["line"]["type"] == "result")
        .filter_map(|v| v["ts"].as_u64())
        .min()
        .unwrap_or(u64::MAX);
    let marker_at = log
        .iter()
        .filter(out)
        .filter(|v| v["line"]["type"] == "assistant" && v["line"]["message"]["content"].to_string().contains(marker))
        .filter_map(|v| v["ts"].as_u64())
        .min();
    decide(marker_at, first_result)
}

/// Codex app-server log ({ts, dir, line}): did an agentMessage containing `marker`
/// appear before the first `turn/completed` notification?
pub fn classify_codex(log: &[Value], marker: &str) -> Delivery {
    let out = |v: &&Value| v["dir"] == "out";
    let first_done = log
        .iter()
        .filter(out)
        .filter(|v| v["line"]["method"] == "turn/completed")
        .filter_map(|v| v["ts"].as_u64())
        .min()
        .unwrap_or(u64::MAX);
    let marker_at = log
        .iter()
        .filter(out)
        .filter(|v| {
            let s = v["line"].to_string();
            s.contains(marker) && s.contains("agentMessage")
        })
        .filter_map(|v| v["ts"].as_u64())
        .min();
    decide(marker_at, first_done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::HookEvent;
    use serde_json::json;

    fn logged(ts: u64, name: &str) -> LoggedEvent {
        LoggedEvent {
            ts_ms: ts,
            task_id: None,
            event: HookEvent {
                hook_event_name: name.into(),
                session_id: None,
                transcript_path: None,
                source: None,
                message: None,
                tool_name: None,
            },
        }
    }

    #[test]
    fn hooks_second_prompt_before_first_stop_is_steered() {
        let e = vec![logged(1, "UserPromptSubmit"), logged(5, "UserPromptSubmit"), logged(9, "Stop")];
        assert_eq!(classify_hooks(&e), Delivery::Steered);
    }

    #[test]
    fn hooks_second_prompt_after_first_stop_is_queued() {
        let e = vec![logged(1, "UserPromptSubmit"), logged(5, "Stop"), logged(6, "UserPromptSubmit"), logged(9, "Stop")];
        assert_eq!(classify_hooks(&e), Delivery::Queued);
    }

    #[test]
    fn hooks_single_prompt_is_never() {
        assert_eq!(classify_hooks(&[logged(1, "UserPromptSubmit")]), Delivery::Never);
    }

    fn out(ts: u64, line: serde_json::Value) -> serde_json::Value {
        json!({"ts": ts, "dir": "out", "line": line})
    }

    #[test]
    fn stream_marker_before_first_result_is_steered() {
        let log = vec![
            json!({"ts": 1, "dir": "in", "line": {"type": "user", "message": {"content": "BANANA"}}}),
            out(5, json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "BANANA"}]}})),
            out(9, json!({"type": "result"})),
        ];
        assert_eq!(classify_stream(&log, "BANANA"), Delivery::Steered);
    }

    #[test]
    fn stream_marker_after_first_result_is_queued() {
        let log = vec![
            out(5, json!({"type": "result"})),
            out(7, json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "BANANA"}]}})),
        ];
        assert_eq!(classify_stream(&log, "BANANA"), Delivery::Queued);
    }

    #[test]
    fn stream_without_marker_is_never() {
        assert_eq!(classify_stream(&[out(5, json!({"type": "result"}))], "BANANA"), Delivery::Never);
    }

    #[test]
    fn codex_agent_message_before_turn_completed_is_steered() {
        let log = vec![
            out(3, json!({"method": "item/completed", "params": {"item": {"type": "userMessage", "text": "BANANA"}}})),
            out(4, json!({"method": "item/completed", "params": {"item": {"type": "agentMessage", "text": "BANANA"}}})),
            out(8, json!({"method": "turn/completed"})),
        ];
        assert_eq!(classify_codex(&log, "BANANA"), Delivery::Steered);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spike-kit`
Expected: all pass, including the 7 `delivery::tests::*` and 1 `stream::tests::*` tests.

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(spike-kit): mid-run delivery classifiers and stream-json input line"
```

---

### Task 5: Transcript usage and last answer (S2 + S4 logic)

**Files:**
- Create: `crates/spike-kit/src/transcript.rs`
- Modify: `crates/spike-kit/src/lib.rs`

- [ ] **Step 1: Write module with failing tests**

`crates/spike-kit/src/lib.rs`:
```rust
pub mod delivery;
pub mod hook;
pub mod status;
pub mod stream;
pub mod transcript;
pub mod util;
```

`crates/spike-kit/src/transcript.rs`:
```rust
use std::collections::HashSet;

use serde_json::Value;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
}

/// Sums token usage of assistant messages in a Claude Code transcript (JSONL).
/// One API message can span several lines with the same message.id: counted once.
pub fn sum_transcript(lines: &[Value], session_id: Option<&str>) -> Usage {
    let mut seen = HashSet::new();
    let mut u = Usage::default();
    for v in lines {
        if v["type"] != "assistant" {
            continue;
        }
        if let Some(sid) = session_id {
            if v["sessionId"].as_str() != Some(sid) {
                continue;
            }
        }
        if let Some(id) = v["message"]["id"].as_str() {
            if !seen.insert(id.to_string()) {
                continue;
            }
        }
        let m = &v["message"]["usage"];
        let f = |k: &str| m[k].as_u64().unwrap_or(0);
        u.input += f("input_tokens");
        u.output += f("output_tokens");
        u.cache_creation += f("cache_creation_input_tokens");
        u.cache_read += f("cache_read_input_tokens");
    }
    u
}

/// Text of the last assistant message that has any text block.
pub fn last_assistant_text(lines: &[Value]) -> Option<String> {
    lines.iter().rev().filter(|v| v["type"] == "assistant").find_map(|v| {
        let text: String = v["message"]["content"]
            .as_array()?
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (!text.trim().is_empty()).then_some(text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant(sid: &str, id: &str, input: u64, output: u64, text: &str) -> Value {
        json!({
            "type": "assistant",
            "sessionId": sid,
            "message": {
                "id": id,
                "content": [{"type": "text", "text": text}],
                "usage": {"input_tokens": input, "output_tokens": output,
                          "cache_creation_input_tokens": 1, "cache_read_input_tokens": 2}
            }
        })
    }

    #[test]
    fn sums_once_per_message_id_and_filters_session() {
        let lines = vec![
            assistant("s1", "m1", 10, 5, "a"),
            assistant("s1", "m1", 10, 5, "a-dup"),
            assistant("s1", "m2", 20, 7, "b"),
            assistant("s2", "m3", 999, 999, "other"),
            json!({"type": "user", "message": {"content": "hi"}}),
        ];
        let u = sum_transcript(&lines, Some("s1"));
        assert_eq!(u, Usage { input: 30, output: 12, cache_creation: 2, cache_read: 4 });
        assert_eq!(u.total(), 48);
    }

    #[test]
    fn last_assistant_text_skips_tool_only_messages() {
        let lines = vec![
            assistant("s", "m1", 0, 0, "JABUTICABA"),
            json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Bash"}]}}),
        ];
        assert_eq!(last_assistant_text(&lines).as_deref(), Some("JABUTICABA"));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spike-kit transcript`
Expected: `2 passed`

- [ ] **Step 3: Commit**

```bash
git add -A && git commit -m "feat(spike-kit): transcript usage summing and last assistant text"
```

---

### Task 6: Codex capture helpers (S1/Codex logic)

**Files:**
- Create: `crates/spike-kit/src/codex.rs`
- Modify: `crates/spike-kit/src/lib.rs`

- [ ] **Step 1: Write module with failing tests**

`crates/spike-kit/src/lib.rs`:
```rust
pub mod codex;
pub mod delivery;
pub mod hook;
pub mod status;
pub mod stream;
pub mod transcript;
pub mod util;
```

`crates/spike-kit/src/codex.rs`:
```rust
use serde_json::Value;

/// Where a thread id / turn id may appear in app-server traffic ({ts, dir, line} log).
/// Verified against the generated schema in Task 10; extend if the schema differs.
pub const THREAD_POINTERS: [&str; 3] = ["/line/result/thread/id", "/line/result/threadId", "/line/params/thread/id"];
pub const TURN_POINTERS: [&str; 3] = ["/line/result/turn/id", "/line/params/turn/id", "/line/result/turnId"];

pub fn capture(log: &[Value], pointers: &[&str]) -> Option<String> {
    log.iter()
        .filter(|v| v["dir"] == "out")
        .find_map(|v| pointers.iter().find_map(|p| v.pointer(p).and_then(Value::as_str).map(str::to_string)))
}

/// Fills $CWD, $THREAD and $TURN placeholders in a script message.
pub fn fill(template: &str, cwd: &str, thread: Option<&str>, turn: Option<&str>) -> String {
    template
        .replace("$CWD", cwd)
        .replace("$THREAD", thread.unwrap_or(""))
        .replace("$TURN", turn.unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn captures_first_matching_pointer_from_output_only() {
        let log = vec![
            json!({"dir": "in", "line": {"result": {"thread": {"id": "wrong"}}}}),
            json!({"dir": "out", "line": {"id": 2, "result": {"thread": {"id": "th_1"}}}}),
            json!({"dir": "out", "line": {"method": "turn/started", "params": {"turn": {"id": "tu_9"}}}}),
        ];
        assert_eq!(capture(&log, &THREAD_POINTERS).as_deref(), Some("th_1"));
        assert_eq!(capture(&log, &TURN_POINTERS).as_deref(), Some("tu_9"));
    }

    #[test]
    fn fill_replaces_placeholders() {
        let t = r#"{"threadId":"$THREAD","expectedTurnId":"$TURN","cwd":"$CWD"}"#;
        assert_eq!(
            fill(t, "/tmp/x", Some("th_1"), Some("tu_9")),
            r#"{"threadId":"th_1","expectedTurnId":"tu_9","cwd":"/tmp/x"}"#
        );
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spike-kit codex`
Expected: `2 passed`

- [ ] **Step 3: Run the whole suite**

Run: `cargo test -p spike-kit`
Expected: `test result: ok. 32 passed` (3 util + 5 hook + 3 status + 13 delivery + 1 stream + 3 transcript + 4 codex).

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(spike-kit): codex app-server id capture and script templating"
```

---

### Task 7: Binaries `mirante-hook` and `spike`, plus the sandbox script

**Files:**
- Create: `crates/spike-kit/src/bin/mirante-hook.rs`, `crates/spike-kit/src/bin/spike.rs`, `scripts/make-sandbox.sh`

- [ ] **Step 1: Hook sink**

`crates/spike-kit/src/bin/mirante-hook.rs`:
```rust
//! Called by Claude Code for every hook event. Appends one JSON line to
//! $MIRANTE_HOOK_LOG (default ~/.mirante-spike/hooks.jsonl). Never fails, never blocks.
use std::io::Read;
use std::path::PathBuf;

use spike_kit::{hook, util};

fn main() {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let log = std::env::var("MIRANTE_HOOK_LOG").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".mirante-spike/hooks.jsonl")
    });
    let task = std::env::var("MIRANTE_TASK_ID").ok();
    let _ = util::append_jsonl(&log, &hook::logged_from_stdin(&raw, task, util::now_ms()));
}
```

- [ ] **Step 2: Utility CLI**

`crates/spike-kit/src/bin/spike.rs`:
```rust
use std::path::Path;

use anyhow::{bail, Context, Result};
use spike_kit::{delivery, hook, status, transcript, util};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("settings") => {
            let hook_bin = args.get(1).context("usage: spike settings <hook-bin-abs-path> <out.json>")?;
            let out = args.get(2).context("missing <out.json>")?;
            let s = hook::settings_json(hook_bin, &["Bash(sleep:*)", "Bash(echo:*)"]);
            std::fs::write(out, serde_json::to_string_pretty(&s)?)?;
            println!("wrote {out}");
        }
        Some("timeline") => {
            let log = args.get(1).context("usage: spike timeline <hooks.jsonl> [task-id]")?;
            let events = hook::load_events(&util::read_jsonl(Path::new(log))?, args.get(2).map(String::as_str));
            for t in status::timeline(&events) {
                println!("{:>14}  {:<18} -> {:?}", t.ts_ms, t.event, t.status);
            }
            println!("mid-run delivery (hooks): {:?}", delivery::classify_hooks(&events));
        }
        Some("usage") => {
            let path = args.get(1).context("usage: spike usage <transcript.jsonl> [session-id]")?;
            let u = transcript::sum_transcript(&util::read_jsonl(Path::new(path))?, args.get(2).map(String::as_str));
            println!("{u:?} total={}", u.total());
        }
        Some(cmd @ ("classify-stream" | "classify-codex")) => {
            let log = args.get(1).context("missing <log.jsonl>")?;
            let marker = args.get(2).map(String::as_str).unwrap_or("BANANA");
            let lines = util::read_jsonl(Path::new(log))?;
            let d = if cmd == "classify-stream" {
                delivery::classify_stream(&lines, marker)
            } else {
                delivery::classify_codex(&lines, marker)
            };
            println!("{d:?}");
        }
        _ => bail!("commands: settings | timeline | usage | classify-stream | classify-codex"),
    }
    Ok(())
}
```

- [ ] **Step 3: Sandbox script**

`scripts/make-sandbox.sh`:
```bash
#!/usr/bin/env bash
# Creates a throwaway git repo for spikes. Prints its path.
set -euo pipefail
dir="${1:-$HOME/.mirante-spike/sandbox}"
rm -rf "$dir"
mkdir -p "$dir"
cd "$dir"
git init -q
echo "# mirante spike sandbox" > README.md
git add README.md
git -c user.email=spike@local -c user.name=spike commit -qm init
echo "$dir"
```

```bash
chmod +x scripts/make-sandbox.sh
```

- [ ] **Step 4: Build and smoke-test the hook sink**

```bash
cargo build -p spike-kit --bins
echo '{"hook_event_name":"Stop","session_id":"s1"}' | MIRANTE_TASK_ID=smoke MIRANTE_HOOK_LOG=/tmp/mirante-smoke.jsonl ./target/debug/mirante-hook
./target/debug/spike timeline /tmp/mirante-smoke.jsonl smoke
./target/debug/spike settings "$PWD/target/debug/mirante-hook" "$HOME/.mirante-spike/settings.json"
```
Expected: the timeline prints one line ending `Stop -> Idle` and `mid-run delivery (hooks): Inconclusive`. Then `wrote /Users/.../.mirante-spike/settings.json`.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(spike-kit): mirante-hook sink, spike utility CLI, sandbox script"
```

---

### Task 8: S3 + S1(Claude interactive) + S4 — `spike-claude-pty`

**Files:**
- Create: `crates/spike-kit/src/bin/spike-claude-pty.rs`

- [ ] **Step 1: Write the PTY driver**

`crates/spike-kit/src/bin/spike-claude-pty.rs`:
```rust
//! Drives interactive `claude` in a PTY with scoped hooks.
//! steer:  sends a 2nd message while the 1st turn runs; classifies via hook timing (S1 + S3).
//! resume: sets a secret, exits, resumes by session id and asks for the secret (S4).
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use spike_kit::{delivery, hook, status, transcript, util};

const FIRST: &str = "Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma chamada por vez, com N de 1 a 5. Ao terminar, diga FIM.";
const SECOND: &str = "Mudança de planos: pare o que está fazendo e responda apenas a palavra BANANA.";
const SECRET_SET: &str = "A palavra secreta é JABUTICABA. Responda apenas OK.";
const SECRET_ASK: &str = "Qual é a palavra secreta? Responda só a palavra.";

struct Session {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    screen: Arc<Mutex<String>>,
}

impl Session {
    fn start(cwd: &str, settings: &str, hooks_log: &Path, task: &str, extra: &[&str]) -> Result<Session> {
        let pair = native_pty_system().openpty(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 })?;
        let mut cmd = CommandBuilder::new("claude");
        for a in extra {
            cmd.arg(a);
        }
        cmd.arg("--settings");
        cmd.arg(settings);
        cmd.cwd(cwd);
        cmd.env("MIRANTE_TASK_ID", task);
        cmd.env("MIRANTE_HOOK_LOG", hooks_log);
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pair.master.take_writer()?));
        let screen = Arc::new(Mutex::new(String::new()));
        let (w2, s2) = (writer.clone(), screen.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                // Answer cursor-position queries so the TUI does not stall.
                if chunk.contains("\x1b[6n") {
                    if let Ok(mut w) = w2.lock() {
                        let _ = w.write_all(b"\x1b[1;1R");
                    }
                }
                if let Ok(mut s) = s2.lock() {
                    s.push_str(&chunk);
                }
            }
        });
        Ok(Session { _master: pair.master, child, writer, screen })
    }

    fn type_line(&self, text: &str) -> Result<()> {
        {
            let mut w = self.writer.lock().unwrap();
            w.write_all(text.as_bytes())?;
            w.flush()?;
        }
        std::thread::sleep(Duration::from_millis(400));
        let mut w = self.writer.lock().unwrap();
        w.write_all(b"\r")?;
        w.flush()?;
        Ok(())
    }

    fn boot(&self) -> Result<()> {
        std::thread::sleep(Duration::from_secs(6));
        if self.screen.lock().unwrap().contains("trust") {
            self.type_line("")?; // accept the default "Yes, proceed" for the sandbox folder
            std::thread::sleep(Duration::from_secs(3));
        }
        Ok(())
    }

    fn finish(mut self) -> String {
        let _ = self.type_line("/exit");
        std::thread::sleep(Duration::from_secs(3));
        let _ = self.child.kill();
        let screen = self.screen.lock().unwrap().clone();
        screen
    }
}

fn wait_for(hooks_log: &Path, task: &str, event: &str, count: usize, timeout: Duration) -> Result<Vec<hook::LoggedEvent>> {
    let start = Instant::now();
    loop {
        let events = if hooks_log.exists() {
            hook::load_events(&util::read_jsonl(hooks_log)?, Some(task))
        } else {
            Vec::new()
        };
        let seen = events.iter().filter(|e| e.event.hook_event_name == event).count();
        if seen >= count || start.elapsed() > timeout {
            return Ok(events);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn print_timeline(events: &[hook::LoggedEvent]) {
    for t in status::timeline(events) {
        println!("{:>14}  {:<18} -> {:?}", t.ts_ms, t.event, t.status);
    }
}

fn steer(cwd: &str, settings: &str, hooks_log: &Path, delay: u64) -> Result<()> {
    let task = format!("steer-{}", util::now_ms());
    let s = Session::start(cwd, settings, hooks_log, &task, &[])?;
    s.boot()?;
    s.type_line(FIRST)?;
    std::thread::sleep(Duration::from_secs(delay));
    s.type_line(SECOND)?;
    let events = wait_for(hooks_log, &task, "Stop", 2, Duration::from_secs(240))?;
    let screen = s.finish();
    std::fs::write(hooks_log.with_extension(format!("{task}.screen.txt")), &screen)?;
    print_timeline(&events);
    println!("mid-run delivery (pty+hooks): {:?}", delivery::classify_hooks(&events));
    Ok(())
}

fn resume(cwd: &str, settings: &str, hooks_log: &Path) -> Result<()> {
    let task = format!("resume-{}", util::now_ms());
    let s = Session::start(cwd, settings, hooks_log, &task, &[])?;
    s.boot()?;
    s.type_line(SECRET_SET)?;
    let events = wait_for(hooks_log, &task, "Stop", 1, Duration::from_secs(120))?;
    let sid = events
        .iter()
        .find(|e| e.event.hook_event_name == "SessionStart")
        .and_then(|e| e.event.session_id.clone())
        .context("no SessionStart with session_id: hooks are not firing")?;
    let _ = s.finish(); // simulates closing the app

    let s2 = Session::start(cwd, settings, hooks_log, &task, &["--resume", sid.as_str()])?;
    s2.boot()?;
    s2.type_line(SECRET_ASK)?;
    let events = wait_for(hooks_log, &task, "Stop", 2, Duration::from_secs(120))?;
    let screen = s2.finish();
    std::fs::write(hooks_log.with_extension(format!("{task}.screen.txt")), &screen)?;

    let resumed = events
        .iter()
        .any(|e| e.event.hook_event_name == "SessionStart" && e.event.source.as_deref() == Some("resume"));
    let transcript_path = events
        .iter()
        .rev()
        .find_map(|e| e.event.transcript_path.clone())
        .context("no transcript_path in hook payloads")?;
    let answer = transcript::last_assistant_text(&util::read_jsonl(Path::new(&transcript_path))?).unwrap_or_default();
    print_timeline(&events);
    println!("session id: {sid}");
    println!("SessionStart source=resume seen: {resumed}");
    println!("answer after resume: {answer:?}");
    println!("S4 {}", if resumed && answer.contains("JABUTICABA") { "PASS" } else { "FAIL" });
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: spike-claude-pty <cwd> <settings.json> <hooks.jsonl> <delay-secs> [steer|resume]";
    let cwd = args.first().context(usage)?;
    let settings = args.get(1).context(usage)?;
    let hooks_log = PathBuf::from(args.get(2).context(usage)?);
    let delay: u64 = args.get(3).context(usage)?.parse()?;
    match args.get(4).map(String::as_str).unwrap_or("steer") {
        "steer" => steer(cwd, settings, &hooks_log, delay),
        "resume" => resume(cwd, settings, &hooks_log),
        other => bail!("unknown mode {other}"),
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p spike-kit --bins`
Expected: builds with no errors. Fix any type mismatch reported by the compiler before continuing; the libraries' API is listed at docs.rs/portable-pty/0.8.

- [ ] **Step 3: Validate the settings file is accepted**

```bash
SANDBOX=$(scripts/make-sandbox.sh)
cd "$SANDBOX" && claude --settings "$HOME/.mirante-spike/settings.json" -p "diga oi" ; cd -
```
Expected: Claude answers. If it rejects `PermissionRequest` as an unknown hook event, remove it from `TOOL_EVENTS` in `hook.rs`, fix the settings test, re-run Task 7 Step 4, and record that in FINDINGS.

- [ ] **Step 4: Run S1 (interactive) + S3**

```bash
SANDBOX=$(scripts/make-sandbox.sh)
./target/debug/spike-claude-pty "$SANDBOX" "$HOME/.mirante-spike/settings.json" "$HOME/.mirante-spike/hooks.jsonl" 6 steer
```
Expected: a status timeline, then `mid-run delivery (pty+hooks): Steered | Queued | Never | Inconclusive`. Open the `*.screen.txt` next to the hooks log and confirm by eye what the agent did with BANANA. Copy both into FINDINGS.

- [ ] **Step 5: Run S4**

```bash
SANDBOX=$(scripts/make-sandbox.sh)
./target/debug/spike-claude-pty "$SANDBOX" "$HOME/.mirante-spike/settings.json" "$HOME/.mirante-spike/hooks.jsonl" 0 resume
```
Expected: `S4 PASS`, with `SessionStart source=resume seen: true`. Any FAIL goes to FINDINGS with the printed lines.

- [ ] **Step 6: S3 manual accuracy check (50 transitions)**

```bash
SANDBOX=$(scripts/make-sandbox.sh); cd "$SANDBOX"
MIRANTE_TASK_ID=manual MIRANTE_HOOK_LOG="$HOME/.mirante-spike/hooks.jsonl" claude --settings "$HOME/.mirante-spike/settings.json"
```
In that session, do a varied mix:
- prompts;
- a command outside the allowlist, to force a permission prompt, then approve it and deny another;
- let it sit idle 60+ s, to trigger the idle Notification;
- a subagent task;
- `/exit`.

Note what the screen showed at each moment. Then run `cd - && ./target/debug/spike timeline ~/.mirante-spike/hooks.jsonl manual` and compare. Count wrong statuses out of at least 50 transitions.

**Special attention:** an idle-wait `Notification` would map to `needs_you`. Record whether that is right or whether it must be split by `message`.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat(spikes): interactive claude PTY driver for steer, status and resume"
```

---

### Task 9: S1(Claude headless) + S2 — `spike-claude-stream`

**Files:**
- Create: `crates/spike-kit/src/bin/spike-claude-stream.rs`

- [ ] **Step 1: Write the driver**

`crates/spike-kit/src/bin/spike-claude-stream.rs`:
```rust
//! Drives `claude -p` with stream-json in/out. Sends a 2nd message while the 1st
//! turn runs and classifies whether it steered the running turn (S1 headless).
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use spike_kit::{delivery, stream, util};

const FIRST: &str = "Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma chamada por vez, com N de 1 a 5. Ao terminar, diga FIM.";
const SECOND: &str = "Mudança de planos: pare o que está fazendo e responda apenas a palavra BANANA.";

fn send(stdin: &mut impl Write, out: &Path, text: &str) -> Result<()> {
    let line = stream::user_line(text);
    writeln!(stdin, "{line}")?;
    stdin.flush()?;
    util::append_jsonl(out, &json!({"ts": util::now_ms(), "dir": "in", "line": serde_json::from_str::<Value>(&line)?}))?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: spike-claude-stream <cwd> <out.jsonl> <delay-secs>";
    let cwd = args.first().context(usage)?.clone();
    let out = PathBuf::from(args.get(1).context(usage)?);
    let delay: u64 = args.get(2).context(usage)?.parse()?;

    let mut child = Command::new("claude")
        .args([
            "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
            "--allowedTools", "Bash(sleep:*)", "Bash(echo:*)",
        ])
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to start claude")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;

    let (tx, rx) = mpsc::channel::<()>();
    let out_reader = out.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            let parsed: Value = serde_json::from_str(&line).unwrap_or(Value::String(line.clone()));
            let is_result = parsed["type"] == "result";
            let _ = util::append_jsonl(&out_reader, &json!({"ts": util::now_ms(), "dir": "out", "line": parsed}));
            if is_result {
                let _ = tx.send(());
            }
        }
    });

    send(&mut stdin, &out, FIRST)?;
    std::thread::sleep(Duration::from_secs(delay));
    send(&mut stdin, &out, SECOND)?;

    let mut results = 0;
    while results < 2 {
        match rx.recv_timeout(Duration::from_secs(240)) {
            Ok(()) => results += 1,
            Err(_) => break,
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();

    let log = util::read_jsonl(&out)?;
    println!("results seen: {results}");
    println!("mid-run delivery (stream-json): {:?}", delivery::classify_stream(&log, "BANANA"));
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p spike-kit --bins`
Expected: no errors.

- [ ] **Step 3: Run S1 headless**

```bash
SANDBOX=$(scripts/make-sandbox.sh)
./target/debug/spike-claude-stream "$SANDBOX" "$HOME/.mirante-spike/stream.jsonl" 6
```
Expected: `results seen: 1|2` and `mid-run delivery (stream-json): Steered | Queued | Never | Inconclusive`. Record both in FINDINGS.

- [ ] **Step 4: Run S2 (quota) — same 3 tasks, both modes**

For each mode, start from a fresh sandbox and run these three prompts:
1. "Liste os arquivos e diga quantos são."
2. "Crie hello.txt com o texto oi."
3. "Execute `sleep 1 && echo ok` e diga o resultado."

The two modes:
- **Interactive:** `claude --settings ~/.mirante-spike/settings.json` in the sandbox. Type the three prompts, then `/exit`.
- **Headless:** run `claude -p --allowedTools "Bash(sleep:*)" "Bash(echo:*)" "Write" "Bash(ls:*)" "<prompt>"` once per prompt in the sandbox.

Then sum the tokens per session:
```bash
ls -t ~/.claude/projects/*mirante-spike-sandbox*/*.jsonl | head -6
./target/debug/spike usage <transcript.jsonl>
```
Also note the plan meter shown by `/usage` inside an interactive `claude`, before and after each mode.

Record in FINDINGS:
- the per-mode totals;
- the meter delta;
- whether the Claude Code terms of use permit automated `-p` use on a subscription. Check the current docs page and quote it.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(spikes): headless stream-json steering driver"
```

---

### Task 10: S1(Codex) — `spike-codex` (requires Task 0 Step 2)

**Files:**
- Create: `crates/spike-kit/src/bin/spike-codex.rs`, `scripts/codex-steer.jsonl`

- [ ] **Step 1: Generate the app-server schema and confirm method names**

```bash
mkdir -p ~/.mirante-spike/codex-schema
codex app-server generate-json-schema --out ~/.mirante-spike/codex-schema || codex app-server --help
grep -rhoE '"(initialize|initialized|thread/start|turn/start|turn/steer|turn/interrupt|turn/completed|turn/started)"' ~/.mirante-spike/codex-schema | sort -u
```
Expected: the method names used in `scripts/codex-steer.jsonl` below appear in the schema.

If a name or param differs, edit the script lines and `THREAD_POINTERS`/`TURN_POINTERS` in `codex.rs`. The params to check are `threadId`, `input`, `expectedTurnId`, `approvalPolicy` and `sandbox`. Record the real names in FINDINGS.

- [ ] **Step 2: Script**

`scripts/codex-steer.jsonl` (one message per line; `delay_ms` = wait before sending):
```
{"delay_ms":0,"msg":{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"mirante-spike","version":"0.1.0"}}}}
{"delay_ms":500,"msg":{"jsonrpc":"2.0","method":"initialized"}}
{"delay_ms":500,"msg":{"jsonrpc":"2.0","id":2,"method":"thread/start","params":{"cwd":"$CWD","approvalPolicy":"never","sandbox":"workspace-write"}}}
{"delay_ms":2000,"msg":{"jsonrpc":"2.0","id":3,"method":"turn/start","params":{"threadId":"$THREAD","input":[{"type":"text","text":"Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma chamada por vez, com N de 1 a 5. Ao terminar, diga FIM."}]}}}
{"delay_ms":8000,"msg":{"jsonrpc":"2.0","id":4,"method":"turn/steer","params":{"threadId":"$THREAD","expectedTurnId":"$TURN","input":[{"type":"text","text":"Mudança de planos: pare o que está fazendo e responda apenas a palavra BANANA."}]}}}
```
`approvalPolicy: never` + `workspace-write` sandbox is spike-only. It is limited to the throwaway sandbox repo and runs only `sleep`/`echo`. The product never auto-approves.

- [ ] **Step 3: Driver**

`crates/spike-kit/src/bin/spike-codex.rs`:
```rust
//! Plays a JSON-RPC script against `codex app-server` over stdio, logging all traffic
//! with timestamps. $CWD/$THREAD/$TURN placeholders are filled from captured ids.
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use spike_kit::{codex, delivery, util};

fn wait_capture(
    out: &Path,
    pick: fn(&[Value], &[&str]) -> Option<String>,
    pointers: &[&str],
    timeout: Duration,
) -> Result<Option<String>> {
    let start = Instant::now();
    loop {
        if out.exists() {
            if let Some(id) = pick(&util::read_jsonl(out)?, pointers) {
                return Ok(Some(id));
            }
        }
        if start.elapsed() > timeout {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: spike-codex <cwd> <script.jsonl> <out.jsonl> <wait-secs>";
    let cwd = args.first().context(usage)?.clone();
    let script = util::read_jsonl(Path::new(args.get(1).context(usage)?))?;
    let out = PathBuf::from(args.get(2).context(usage)?);
    let wait: u64 = args.get(3).context(usage)?.parse()?;

    let mut child = Command::new("codex")
        .arg("app-server")
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to start codex app-server")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let out_reader = out.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            let parsed: Value = serde_json::from_str(&line).unwrap_or(Value::String(line.clone()));
            let _ = util::append_jsonl(&out_reader, &json!({"ts": util::now_ms(), "dir": "out", "line": parsed}));
        }
    });

    for step in &script {
        std::thread::sleep(Duration::from_millis(step["delay_ms"].as_u64().unwrap_or(0)));
        let template = step["msg"].to_string();
        let thread = if template.contains("$THREAD") {
            wait_capture(&out, codex::capture, &codex::THREAD_POINTERS, Duration::from_secs(15))?
        } else {
            None
        };
        let turn = if template.contains("$TURN") {
            wait_capture(&out, codex::capture_last, &codex::TURN_POINTERS, Duration::from_secs(15))?
        } else {
            None
        };
        let msg = codex::fill(&template, &cwd, thread.as_deref(), turn.as_deref()).map_err(anyhow::Error::msg)?;
        writeln!(stdin, "{msg}")?;
        stdin.flush()?;
        util::append_jsonl(&out, &json!({"ts": util::now_ms(), "dir": "in", "line": serde_json::from_str::<Value>(&msg)?}))?;
    }

    std::thread::sleep(Duration::from_secs(wait));
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    println!("mid-run delivery (codex): {:?}", delivery::classify_codex(&util::read_jsonl(&out)?, "BANANA"));
    Ok(())
}
```

- [ ] **Step 4: Build and run**

```bash
cargo build -p spike-kit --bins
SANDBOX=$(scripts/make-sandbox.sh)
./target/debug/spike-codex "$SANDBOX" scripts/codex-steer.jsonl "$HOME/.mirante-spike/codex.jsonl" 60
```
Expected: `mid-run delivery (codex): Steered | Queued | Never | Inconclusive`. If you get `Never`, inspect `codex.jsonl` for JSON-RPC `error` responses to ids 2–4, fix the script per the schema and re-run. Record the final working script in FINDINGS.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(spikes): codex app-server scripted steering driver"
```

---

### Task 11: Findings and go/no-go

**Files:**
- Create: `docs/spikes/FINDINGS.md`

- [ ] **Step 1: Write the findings with the real numbers from Tasks 8–10**

`docs/spikes/FINDINGS.md` structure:
```markdown
# Week-1 spike findings

Environment: macOS <version>, Claude Code <version>, Codex <version>, date <YYYY-MM-DD>.

| Spike | Path | Result | Evidence (command output) | Decision |
|---|---|---|---|---|
| S1 Claude | PTY + hooks | Steered/Queued/Never | ... | ... |
| S1 Claude | stream-json | Steered/Queued/Never | ... | ... |
| S1 Codex | app-server turn/steer | Steered/Queued/Never | ... | ... |
| S2 Quota | interactive vs -p | tokens A vs B; meter delta | ... | chosen mode |
| S3 Status | hooks | wrong statuses: n / 50 | ... | mapping changes (if any) |
| S4 Resume | --resume <id> | PASS/FAIL | ... | ... |

## Driver decision per agent
- Claude Code: <PTY+hooks | stream-json> because ...
- Codex: <app-server> because ...

## Changes to the spec / Plan 2 inputs
- ...
```
Fill every cell from actual output. A cell without evidence is marked "not run" with the reason.

- [ ] **Step 2: Commit**

```bash
git add docs/spikes/FINDINGS.md && git commit -m "docs(spikes): week-1 findings and driver decisions"
```

- [ ] **Step 3: Hand back for Plan 2.** Plan 2 covers the daemon, drivers, Tauri UI and persistence. It is written from FINDINGS. Push to `origin main` only after the founder confirms.
