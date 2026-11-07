# Mirante Plan 2a — mirante-core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `crates/mirante-core`, the pure library under the cockpit daemon: the durable domain model, the SQLite store, the hooks + transcript status fusion engine, steer/queue classification, an incremental transcript tailer, the git worktree manager and the Claude launch config. Every rule that Week 1 measured (`docs/spikes/FINDINGS.md`) becomes a unit test here before any PTY, daemon or UI exists.

**Architecture:** One library crate with one responsibility per module and no I/O beyond SQLite, the transcript file and the `git` CLI. The daemon (Plan 2b) will own a `Store`, one `StatusEngine` + `TranscriptTail` per live session, and will turn `LaunchSpec` into a PTY process; this crate only decides and persists. Proven spike logic is moved and adapted (status mapping, transcript classifier, env scrub, settings generator); `spike-kit` stays untouched and keeps its own copies.

**Tech Stack:** Rust 1.99 (edition 2021), rusqlite 0.41 (`bundled`), serde/serde_json, thiserror 2, uuid 1 (`v7`), `git` CLI ≥ 2.31 (uses `rev-parse --path-format=absolute`) via `std::process::Command`. No new dev-dependencies (a 20-line `TempDir` helper replaces `tempfile`).

**Inputs:** `docs/spec/2026-11-07-cockpit-prototype-design.md` (product spec), `docs/spikes/FINDINGS.md` (authoritative measured rules, including Task 10's Codex results), `crates/spike-kit/src/*.rs` (tested logic being moved).

**Ground rules for every task:**
- Shell setup for every command: `export PATH=/opt/homebrew/opt/rustup/bin:$PATH && cd ~/Documents/Develop/mirante`.
- The repo already has the local commit e-mail set (`git config --local user.email` → `pedrohenriquecandidoferreira@hotmail.com`); check it once before the first commit and do not change it.
- Work on the current branch (solo dev, no per-plan branches).
- `spike-kit` must keep compiling and passing all of its tests (61 at commit `b5f6032`). Never edit it in this plan.
- Code blocks are formatted as `cargo fmt -- --config max_width=120,use_small_heuristics=Max` would format them; do not run a plain `cargo fmt` on the workspace (it would reformat `spike-kit`).

**Out of scope (next plans):**
- **Plan 2b — daemon:** PTY session management (portable-pty + vt100), `mirante-hook` → daemon transport, the `cockpitd`/`miranted` Unix-socket (0600, peer-uid check) typed RPC, snapshot + `events_since` reconciliation for clients, wiring `StatusEngine` + `TranscriptTail` + `Store` together, trust-dialog surfacing (FINDINGS rule 5), answering permission dialogs in the PTY (rule 6).
- **Plan 2c — CLI** over the RPC.
- **Plan 2d — Codex driver:** `codex app-server` JSON-RPC (`turn/steer`, `turn/interrupt`, `thread/resume`, approval requests), its own status rules (FINDINGS rule 8: open `commandExecution` items after `turn/completed`) and env scrub (`CMUX_*`).
- **Plan 3 — Tauri UI.** Also later: hidden checkpoint refs per turn, cost/token ceilings, protected-path suggestions, repo `setup`/`teardown` scripts.

---

## File structure

```
Cargo.toml                                   # workspace: add crates/mirante-core
crates/mirante-core/
  Cargo.toml                                 # rusqlite(bundled), serde, serde_json, thiserror, uuid(v7)
  src/lib.rs                                 # module list + re-exports
  src/error.rs                               # CoreError (thiserror) + Result alias
  src/test_support.rs                        # #[cfg(test)] TempDir (auto-removed temp directory)
  src/model.rs                               # ids, time, Status/Lifecycle/AgentKind/PermissionStatus + transition rules, entity structs
  src/hook.rs                                # HookEvent (Claude hook payload incl. agent_id/agent_type), side-agent test, permission detail
  src/migrations/0001_init.sql               # schema v1: tasks, conversations, sessions (+ per-task lock index), turns, permission_requests, events (append-only)
  src/store.rs                               # Store: open (WAL, FKs), versioned migrations, CRUD, validated transitions, audit events
  src/status.rs                              # StatusEngine: hooks + transcript fusion -> Status + transitions
  src/delivery.rs                            # Delivery + classify_transcript / classify_message (steered vs queued)
  src/transcript_tail.rs                     # TranscriptTail: byte-offset JSONL tailer with partial-line buffering
  src/worktree.rs                            # WorktreeManager: create/remove/list git worktrees, branch validation
  src/claude_launch.rs                       # resolve real claude binary, env scrub, settings JSON, LaunchSpec
```

Dependency order: `error` → `model` → `hook` → `store` → `status` → `delivery` (uses `status::is_sidechain`) → `transcript_tail` → `worktree` → `claude_launch`.

---

### Task 1: Scaffold the `mirante-core` crate

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/mirante-core/Cargo.toml`, `crates/mirante-core/src/lib.rs`, `crates/mirante-core/src/error.rs`, `crates/mirante-core/src/test_support.rs`

- [ ] **Step 1: Add the crate to the workspace and write its manifest, error type and module list**

`Cargo.toml`:
```toml
[workspace]
members = ["crates/spike-kit", "crates/mirante-core"]
resolver = "2"
```

`crates/mirante-core/Cargo.toml`:
```toml
[package]
name = "mirante-core"
version = "0.1.0"
edition = "2021"
publish = false
license = "Apache-2.0"

[dependencies]
rusqlite = { version = "0.41", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
uuid = { version = "1", features = ["v7"] }
```

`crates/mirante-core/src/error.rs`:
```rust
use thiserror::Error;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("{kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },
    #[error("invalid {kind} transition: {from} -> {to}")]
    InvalidTransition { kind: &'static str, from: String, to: String },
    #[error("task {0} already has a live session")]
    TaskBusy(String),
    #[error("stale status for task {task_id}: expected {expected}, found {actual}")]
    StaleStatus { task_id: String, expected: String, actual: String },
    #[error("unknown {kind} value in database: {value}")]
    BadEnum { kind: &'static str, value: String },
    #[error("invalid branch name {name:?}: {reason}")]
    InvalidBranch { name: String, reason: String },
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
    #[error("claude binary not found: {0}")]
    ClaudeNotFound(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
```

`crates/mirante-core/src/lib.rs`:
```rust
//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod error;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
```

- [ ] **Step 2: Write the failing test for the temp-dir helper**

`crates/mirante-core/src/test_support.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_is_created_and_removed() {
        let kept;
        {
            let d = TempDir::new("smoke");
            kept = d.path().to_path_buf();
            assert!(kept.is_dir());
        }
        assert!(!kept.exists());
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p mirante-core test_support`
Expected: compile error `` cannot find type `TempDir` in this scope `` (or `` failed to resolve: use of undeclared type `TempDir` ``).

- [ ] **Step 4: Implement the helper above the test module**

Insert at the top of `crates/mirante-core/src/test_support.rs`:
```rust
use std::path::{Path, PathBuf};

/// A unique directory under the system temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mirante-core-{label}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
```

- [ ] **Step 5: Run the tests and check spike-kit is untouched**

Run: `cargo test -p mirante-core test_support && cargo test -p spike-kit 2>&1 | grep "test result" | head -1`
Expected: `test result: ok. 1 passed` for mirante-core, then `test result: ok. 61 passed` for spike-kit.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/mirante-core
git commit -F - <<'EOF'
feat(core): scaffold mirante-core crate with error type and test temp dir

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 2: Domain model and transition rules

**Files:**
- Create: `crates/mirante-core/src/model.rs`
- Modify: `crates/mirante-core/src/lib.rs`

Two state machines, both validated (agent-plataform had none): `Status` is what the agent is doing (`Ended` may only go back to `Idle`, i.e. a resumed session); `Lifecycle` is the task's own life (`Created → Active → Archived`, `Created → Archived`; `Archived` is terminal). The entity structs mirror the spec's durable model: Task → Conversation (durable, `native_session_id`) → Session (process activation) → Turn, plus PermissionRequest and Event (`seq` + `boot_id`). `Conversation::native_session_id` holds Claude's `session_id` now and will hold Codex's thread id in Plan 2d.

- [ ] **Step 1: Register the module and write the failing tests**

`crates/mirante-core/src/lib.rs`:
```rust
//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod error;
pub mod model;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
```

`crates/mirante-core/src/model.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_transitions() {
        assert!(Status::Idle.can_transition_to(Status::Working));
        assert!(Status::Working.can_transition_to(Status::NeedsYou));
        assert!(Status::NeedsYou.can_transition_to(Status::Idle));
        assert!(Status::Idle.can_transition_to(Status::Ended));
        assert!(Status::Ended.can_transition_to(Status::Idle));
        assert!(!Status::Ended.can_transition_to(Status::Working));
        assert!(!Status::Ended.can_transition_to(Status::NeedsYou));
        assert!(!Status::Working.can_transition_to(Status::Working));
    }

    #[test]
    fn lifecycle_transitions() {
        assert!(Lifecycle::Created.can_transition_to(Lifecycle::Active));
        assert!(Lifecycle::Created.can_transition_to(Lifecycle::Archived));
        assert!(Lifecycle::Active.can_transition_to(Lifecycle::Archived));
        assert!(!Lifecycle::Active.can_transition_to(Lifecycle::Created));
        assert!(!Lifecycle::Archived.can_transition_to(Lifecycle::Active));
        assert!(!Lifecycle::Active.can_transition_to(Lifecycle::Active));
    }

    #[test]
    fn text_enums_roundtrip_and_reject_unknown() {
        for s in [Status::Idle, Status::Working, Status::NeedsYou, Status::Ended] {
            assert_eq!(Status::parse(s.as_str()).unwrap(), s);
        }
        assert_eq!(PermissionStatus::parse("allowed_once").unwrap(), PermissionStatus::AllowedOnce);
        assert_eq!(AgentKind::parse("codex").unwrap(), AgentKind::Codex);
        assert_eq!(Lifecycle::parse("archived").unwrap(), Lifecycle::Archived);
        assert!(matches!(Status::parse("busy"), Err(CoreError::BadEnum { kind: "status", .. })));
    }

    #[test]
    fn ids_are_unique_and_time_ordered() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert!(a < b, "{a} < {b}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core model::`
Expected: compile errors such as `` cannot find type `Status` in this scope `` and `` cannot find function `new_id` ``.

- [ ] **Step 3: Implement the model above the test module**

Insert at the top of `crates/mirante-core/src/model.rs`:
```rust
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{CoreError, Result};

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Time-ordered unique id (UUID v7, hyphenated).
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Generates `as_str`/`parse` for a fieldless enum stored as TEXT in SQLite.
macro_rules! text_enum {
    ($name:ident, $kind:literal, { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            pub fn parse(s: &str) -> Result<Self> {
                match s {
                    $($text => Ok($name::$variant),)+
                    other => Err(CoreError::BadEnum { kind: $kind, value: other.to_string() }),
                }
            }
        }
    };
}

/// What the agent of a task is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Working,
    NeedsYou,
    Ended,
}
text_enum!(Status, "status", { Idle => "idle", Working => "working", NeedsYou => "needs_you", Ended => "ended" });

impl Status {
    /// Any live status may move to any other live status or to `Ended`. `Ended` may only
    /// move to `Idle` (a resumed session). Staying in the same status is not a transition.
    pub fn can_transition_to(self, to: Status) -> bool {
        match (self, to) {
            (a, b) if a == b => false,
            (Status::Ended, Status::Idle) => true,
            (Status::Ended, _) => false,
            _ => true,
        }
    }
}

/// Lifecycle of a task, independent of what its agent is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// Created, no session started yet.
    Created,
    /// At least one session was started.
    Active,
    /// Worktree removed, branch kept. Terminal.
    Archived,
}
text_enum!(Lifecycle, "lifecycle", { Created => "created", Active => "active", Archived => "archived" });

impl Lifecycle {
    pub fn can_transition_to(self, to: Lifecycle) -> bool {
        matches!(
            (self, to),
            (Lifecycle::Created, Lifecycle::Active)
                | (Lifecycle::Created, Lifecycle::Archived)
                | (Lifecycle::Active, Lifecycle::Archived)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
}
text_enum!(AgentKind, "agent", { Claude => "claude", Codex => "codex" });

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStatus {
    Pending,
    AllowedOnce,
    Denied,
}
text_enum!(PermissionStatus, "permission_status", { Pending => "pending", AllowedOnce => "allowed_once", Denied => "denied" });

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub repo_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub agent: AgentKind,
    pub goal: String,
    pub notes: String,
    pub lifecycle: Lifecycle,
    pub status: Status,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Input for `Store::create_task`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    pub title: String,
    pub repo_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub agent: AgentKind,
    pub goal: String,
}

/// The durable agent conversation of a task. `native_session_id` is the agent's own id
/// (Claude's `session_id` from `SessionStart`), used to relaunch with `--resume`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub task_id: String,
    pub native_session_id: Option<String>,
    pub created_at: i64,
}

/// One process activation of a conversation. Live while `ended_at` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub task_id: String,
    pub conversation_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub transcript_path: Option<String>,
    pub transcript_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub id: String,
    pub session_id: String,
    /// 1-based position of the turn within its session.
    pub idx: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

/// Append-only audit record. `seq` is monotonic across boots; `boot_id` names the
/// store opening (daemon boot) that wrote it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub boot_id: String,
    pub task_id: Option<String>,
    pub kind: String,
    pub payload: Value,
    pub ts_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub task_id: String,
    pub session_id: String,
    pub tool: String,
    pub detail: String,
    pub status: PermissionStatus,
    pub created_at: i64,
    pub answered_at: Option<i64>,
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core model::`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): domain model with validated status and lifecycle transitions

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 3: Hook payload with agent identity

> **Superseded in part:** commit "fix(core): status engine handles resume/clear, dialog clearing, late Stop, slash commands" supersedes this task's code for the hook fields (`cwd`, `prompt`, `permission_mode`, `tool_use_id`) and for `is_side_agent` (`agent_id` or `SubagentStart`/`SubagentStop`; `agent_type` alone is the `--agent` main thread). `src/hook.rs` is the source of truth.

**Files:**
- Create: `crates/mirante-core/src/hook.rs`
- Modify: `crates/mirante-core/src/lib.rs`

Moved from `spike_kit::hook::HookEvent`, plus the fields FINDINGS rule 1 asks for: `agent_id` / `agent_type` (side-agent filtering) and `tool_input` (permission card detail). Parse errors are returned, never turned into an empty event (a "do not repeat" item from spec §8).

- [ ] **Step 1: Register the module and write the failing tests**

`crates/mirante-core/src/lib.rs`:
```rust
//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod error;
pub mod hook;
pub mod model;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
```

`crates/mirante-core/src/hook.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_payload_with_agent_identity_and_ignores_unknown_fields() {
        let raw = r#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Bash",
            "tool_input":{"command":"ls"},"agent_id":"a-7","agent_type":"general-purpose","extra":1}"#;
        let e = HookEvent::parse(raw).unwrap();
        assert_eq!(e.hook_event_name, "PreToolUse");
        assert_eq!(e.agent_id.as_deref(), Some("a-7"));
        assert_eq!(e.agent_type.as_deref(), Some("general-purpose"));
        assert!(e.is_side_agent());
        assert_eq!(e.permission_detail(), "ls");
    }

    #[test]
    fn main_agent_payload_has_no_agent_id() {
        let e = HookEvent::parse(r#"{"hook_event_name":"Stop","session_id":"s1"}"#).unwrap();
        assert!(!e.is_side_agent());
        assert_eq!(e, HookEvent { session_id: Some("s1".into()), ..HookEvent::named("Stop") });
    }

    #[test]
    fn invalid_payload_is_an_error_not_an_empty_event() {
        assert!(HookEvent::parse("not json").is_err());
        assert!(HookEvent::parse(r#"{"session_id":"s1"}"#).is_err());
    }

    #[test]
    fn permission_detail_falls_back_to_json() {
        let e = HookEvent {
            tool_input: Some(serde_json::json!({"file_path": "src/a.rs"})),
            ..HookEvent::named("PermissionRequest")
        };
        assert_eq!(e.permission_detail(), "src/a.rs");
        let e = HookEvent { tool_input: Some(serde_json::json!({"q": 1})), ..HookEvent::named("PermissionRequest") };
        assert_eq!(e.permission_detail(), r#"{"q":1}"#);
        assert_eq!(HookEvent::named("PermissionRequest").permission_detail(), "");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core hook::`
Expected: compile error `` cannot find type `HookEvent` in this scope ``.

- [ ] **Step 3: Implement above the test module**

Insert at the top of `crates/mirante-core/src/hook.rs`:
```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// Subset of the JSON Claude Code writes to a hook's stdin. Unknown fields are ignored.
/// `agent_id` / `agent_type` are present when a subagent or side agent emitted the event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
}

impl HookEvent {
    /// Parses one hook payload. Errors are returned, never swallowed into an empty event.
    pub fn parse(raw: &str) -> Result<HookEvent> {
        Ok(serde_json::from_str(raw)?)
    }

    pub fn named(name: &str) -> HookEvent {
        HookEvent { hook_event_name: name.to_string(), ..HookEvent::default() }
    }

    /// True when a subagent / side agent emitted the event (the payload names an agent id).
    pub fn is_side_agent(&self) -> bool {
        self.agent_id.is_some()
    }

    /// Human-readable detail for a permission card: the Bash command, the file path,
    /// or the compact JSON of the tool input.
    pub fn permission_detail(&self) -> String {
        let Some(input) = &self.tool_input else {
            return String::new();
        };
        for key in ["command", "file_path", "path", "url"] {
            if let Some(s) = input[key].as_str() {
                return s.to_string();
            }
        }
        input.to_string()
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core hook::`
Expected: `test result: ok. 4 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): hook payload model with agent identity and permission detail

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 4: Store — open, WAL and versioned migrations

> **Superseded in part:** the commit titled `fix(core): harden store constraints, worktree argv, launch quoting, tailer memory` supersedes the code below for store hardening (WAL check, migration 0002 trigger, UNIQUE mapping, Immediate transactions, conversation/session ownership checks). Prefer the code in `crates/mirante-core` over the snippets here.


**Files:**
- Create: `crates/mirante-core/src/migrations/0001_init.sql`, `crates/mirante-core/src/store.rs`
- Modify: `crates/mirante-core/src/lib.rs`

Schema v1 is embedded with `include_str!` and applied in an immediate transaction; `PRAGMA user_version` records the applied version, so re-opening never re-runs it. The schema already carries the two structural guarantees later tasks test: the **per-task lock** (partial unique index: one session per task with `ended_at IS NULL`) and the **append-only events** table (`AUTOINCREMENT` seq + triggers that abort UPDATE/DELETE). Each `Store::open` mints a fresh `boot_id` (one per daemon boot).

- [ ] **Step 1: Write the schema**

`crates/mirante-core/src/migrations/0001_init.sql`:
```sql
CREATE TABLE tasks (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    repo_path     TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    branch        TEXT NOT NULL,
    agent         TEXT NOT NULL CHECK (agent IN ('claude', 'codex')),
    goal          TEXT NOT NULL DEFAULT '',
    notes         TEXT NOT NULL DEFAULT '',
    lifecycle     TEXT NOT NULL CHECK (lifecycle IN ('created', 'active', 'archived')),
    status        TEXT NOT NULL CHECK (status IN ('idle', 'working', 'needs_you', 'ended')),
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE TABLE conversations (
    id                TEXT PRIMARY KEY,
    task_id           TEXT NOT NULL REFERENCES tasks(id),
    native_session_id TEXT,
    created_at        INTEGER NOT NULL
);
CREATE INDEX conversations_task ON conversations(task_id, created_at);

CREATE TABLE sessions (
    id                TEXT PRIMARY KEY,
    task_id           TEXT NOT NULL REFERENCES tasks(id),
    conversation_id   TEXT NOT NULL REFERENCES conversations(id),
    started_at        INTEGER NOT NULL,
    ended_at          INTEGER,
    transcript_path   TEXT,
    transcript_offset INTEGER NOT NULL DEFAULT 0
);
-- Per-task lock: at most one live (not ended) session per task.
CREATE UNIQUE INDEX sessions_one_live_per_task ON sessions(task_id) WHERE ended_at IS NULL;

CREATE TABLE turns (
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    idx        INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at   INTEGER,
    UNIQUE (session_id, idx)
);

CREATE TABLE permission_requests (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    session_id  TEXT NOT NULL REFERENCES sessions(id),
    tool        TEXT NOT NULL,
    detail      TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('pending', 'allowed_once', 'denied')),
    created_at  INTEGER NOT NULL,
    answered_at INTEGER
);
CREATE INDEX permission_requests_task ON permission_requests(task_id, status);

-- Append-only audit trail. AUTOINCREMENT: seq is never reused, even after deletes.
CREATE TABLE events (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    boot_id TEXT NOT NULL,
    task_id TEXT,
    kind    TEXT NOT NULL,
    payload TEXT NOT NULL,
    ts_ms   INTEGER NOT NULL
);
CREATE INDEX events_task ON events(task_id, seq);
CREATE TRIGGER events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
```

- [ ] **Step 2: Register the module and write the failing tests**

`crates/mirante-core/src/lib.rs`:
```rust
//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod error;
pub mod hook;
pub mod model;
pub mod store;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
```

`crates/mirante-core/src/store.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn migrates_to_latest_version_once() {
        let dir = TempDir::new("store-migrate");
        let path = dir.path().join("db/mirante.sqlite");
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 1);
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), 1);
    }

    #[test]
    fn file_database_uses_wal_and_foreign_keys() {
        let dir = TempDir::new("store-wal");
        let s = Store::open(&dir.path().join("m.sqlite")).unwrap();
        assert_eq!(s.journal_mode().unwrap(), "wal");
        let fk: i64 = s.conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn each_opening_gets_a_new_boot_id() {
        let a = Store::open_in_memory().unwrap();
        let b = Store::open_in_memory().unwrap();
        assert_ne!(a.boot_id(), b.boot_id());
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p mirante-core store::`
Expected: compile error `` cannot find type `Store` in this scope ``.

- [ ] **Step 4: Implement open + migrations above the test module**

Insert at the top of `crates/mirante-core/src/store.rs`:
```rust
use std::path::Path;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, TransactionBehavior};

use crate::error::Result;
use crate::model::{new_id, AgentKind, Lifecycle, PermissionStatus, Status};

/// Embedded, ordered schema migrations. `PRAGMA user_version` records the last applied one.
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("migrations/0001_init.sql"))];

macro_rules! sql_text_enum {
    ($($t:ty),+) => {$(
        impl ToSql for $t {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }
        impl FromSql for $t {
            fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
                <$t>::parse(v.as_str()?).map_err(|e| FromSqlError::Other(Box::new(e)))
            }
        }
    )+};
}
sql_text_enum!(Status, Lifecycle, AgentKind, PermissionStatus);

/// Single owner of Mirante's durable state. Every mutation runs in one transaction that
/// also appends its audit event, so state and trail never diverge.
pub struct Store {
    conn: Connection,
    boot_id: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        Store::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Store> {
        Store::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Store> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // Returns the resulting mode ("wal" on disk, "memory" for in-memory databases).
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn)?;
        Ok(Store { conn, boot_id: new_id() })
    }

    /// Identifies this opening of the store (one daemon boot). Stamped on every event.
    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    pub fn journal_mode(&self) -> Result<String> {
        Ok(self.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?)
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p mirante-core store::`
Expected: `test result: ok. 3 passed`.

- [ ] **Step 6: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): sqlite store with WAL and versioned embedded migrations

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 5: Store — tasks, validated transitions and the audit trail

> **Superseded in part:** see the note at Task 4; the `fix(core): harden store constraints...` commit changes store behavior here (answered-once UPDATE guard, session/conversation checks, Immediate transactions).


**Files:**
- Modify: `crates/mirante-core/src/store.rs`

Every mutation runs in one immediate transaction that also inserts its event, so state and trail never diverge ("persisted on every transition"). `set_status` is a compare-and-set: the caller states the status it believes is current, and a mismatch is `StaleStatus`, not a silent overwrite. `events_since(seq, limit)` is the reconciliation read clients will use.

- [ ] **Step 1: Write the failing tests**

Append to the end of `crates/mirante-core/src/store.rs`:
```rust
#[cfg(test)]
fn sample_task(title: &str) -> NewTask {
    NewTask {
        title: title.to_string(),
        repo_path: "/repos/app".into(),
        worktree_path: format!("/wt/app/{title}"),
        branch: title.to_string(),
        agent: AgentKind::Claude,
        goal: "ship it".into(),
    }
}

#[cfg(test)]
mod task_tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn create_get_and_list_tasks() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.create_task(sample_task("feat-a")).unwrap();
        let b = s.create_task(sample_task("feat-b")).unwrap();
        assert_eq!(a.lifecycle, Lifecycle::Created);
        assert_eq!(a.status, Status::Idle);
        assert_eq!(s.get_task(&a.id).unwrap(), a);
        let ids: Vec<String> = s.list_tasks(false).unwrap().into_iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![a.id.clone(), b.id.clone()]);
        assert!(matches!(s.get_task("nope"), Err(CoreError::NotFound { kind: "task", .. })));
    }

    #[test]
    fn archived_tasks_are_hidden_unless_asked() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s.create_task(sample_task("feat-a")).unwrap();
        s.transition_lifecycle(&a.id, Lifecycle::Archived).unwrap();
        assert!(s.list_tasks(false).unwrap().is_empty());
        assert_eq!(s.list_tasks(true).unwrap().len(), 1);
    }

    #[test]
    fn lifecycle_transitions_are_validated_and_audited() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        assert_eq!(s.transition_lifecycle(&t.id, Lifecycle::Active).unwrap().lifecycle, Lifecycle::Active);
        let err = s.transition_lifecycle(&t.id, Lifecycle::Created).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTransition { kind: "lifecycle", .. }), "{err}");
        let kinds: Vec<String> = s.task_events(&t.id).unwrap().into_iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec!["task_created", "lifecycle"]);
    }

    #[test]
    fn set_status_is_compare_and_set_and_validated() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let e = s.set_status(&t.id, Status::Idle, Status::Working, "hook:UserPromptSubmit").unwrap();
        assert_eq!(e.kind, "status");
        assert_eq!(e.payload, json!({"from": "idle", "to": "working", "cause": "hook:UserPromptSubmit"}));
        assert_eq!(s.get_task(&t.id).unwrap().status, Status::Working);
        let stale = s.set_status(&t.id, Status::Idle, Status::NeedsYou, "x").unwrap_err();
        assert!(matches!(stale, CoreError::StaleStatus { .. }), "{stale}");
        s.set_status(&t.id, Status::Working, Status::Ended, "hook:SessionEnd").unwrap();
        let bad = s.set_status(&t.id, Status::Ended, Status::Working, "x").unwrap_err();
        assert!(matches!(bad, CoreError::InvalidTransition { kind: "status", .. }), "{bad}");
        assert_eq!(s.get_task(&t.id).unwrap().status, Status::Ended);
    }

    #[test]
    fn goal_and_notes_persist() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let t = s.update_goal_notes(&t.id, "new goal", "remember X").unwrap();
        assert_eq!((t.goal.as_str(), t.notes.as_str()), ("new goal", "remember X"));
    }

    #[test]
    fn events_are_monotonic_across_reopen_and_carry_boot_id() {
        let dir = TempDir::new("store-events");
        let path = dir.path().join("m.sqlite");
        let mut s1 = Store::open(&path).unwrap();
        let boot1 = s1.boot_id().to_string();
        let t = s1.create_task(sample_task("feat-a")).unwrap();
        let first = s1.append_event(Some(&t.id), "step", &json!({"tool": "Bash"})).unwrap();
        drop(s1);
        let mut s2 = Store::open(&path).unwrap();
        let second = s2.append_event(Some(&t.id), "step", &json!({"tool": "Edit"})).unwrap();
        assert!(second.seq > first.seq);
        assert_ne!(second.boot_id, boot1);
        let since = s2.events_since(first.seq, 10).unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0], second);
        assert_eq!(s2.events_since(0, 1).unwrap().len(), 1);
        assert_eq!(s2.events_since(0, 1).unwrap()[0].boot_id, boot1);
    }

    #[test]
    fn events_table_is_append_only() {
        let mut s = Store::open_in_memory().unwrap();
        s.append_event(None, "boot", &json!({})).unwrap();
        assert!(s.conn.execute("UPDATE events SET kind = 'x'", []).is_err());
        assert!(s.conn.execute("DELETE FROM events", []).is_err());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core store::`
Expected: compile errors such as `` no method named `create_task` found for struct `Store` `` and `` cannot find type `NewTask` ``.

- [ ] **Step 3: Implement**

Replace the import block at the top of `crates/mirante-core/src/store.rs` (everything from `use std::path::Path;` down to the `use crate::model::...;` line) with:
```rust
use std::path::Path;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

use crate::error::{CoreError, Result};
use crate::model::{new_id, now_ms, AgentKind, Event, Lifecycle, NewTask, PermissionStatus, Status, Task};
```

Then insert, between the closing `}` of `mod tests` and the `#[cfg(test)] fn sample_task` you just appended:
```rust
fn insert_event(conn: &Connection, boot_id: &str, task_id: Option<&str>, kind: &str, payload: &Value) -> Result<Event> {
    let ts_ms = now_ms();
    conn.execute(
        "INSERT INTO events (boot_id, task_id, kind, payload, ts_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![boot_id, task_id, kind, payload.to_string(), ts_ms],
    )?;
    Ok(Event {
        seq: conn.last_insert_rowid() as u64,
        boot_id: boot_id.to_string(),
        task_id: task_id.map(str::to_string),
        kind: kind.to_string(),
        payload: payload.clone(),
        ts_ms,
    })
}

const TASK_COLUMNS: &str =
    "id, title, repo_path, worktree_path, branch, agent, goal, notes, lifecycle, status, created_at, updated_at";

fn task_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        title: r.get(1)?,
        repo_path: r.get(2)?,
        worktree_path: r.get(3)?,
        branch: r.get(4)?,
        agent: r.get(5)?,
        goal: r.get(6)?,
        notes: r.get(7)?,
        lifecycle: r.get(8)?,
        status: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

fn event_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let payload: String = r.get(4)?;
    Ok(Event {
        seq: r.get::<_, i64>(0)? as u64,
        boot_id: r.get(1)?,
        task_id: r.get(2)?,
        kind: r.get(3)?,
        payload: serde_json::from_str(&payload)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?,
        ts_ms: r.get(5)?,
    })
}

fn load_task(conn: &Connection, id: &str) -> Result<Task> {
    conn.query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"), [id], task_from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound { kind: "task", id: id.to_string() })
}

impl Store {
    pub fn create_task(&mut self, new: NewTask) -> Result<Task> {
        let now = now_ms();
        let task = Task {
            id: new_id(),
            title: new.title,
            repo_path: new.repo_path,
            worktree_path: new.worktree_path,
            branch: new.branch,
            agent: new.agent,
            goal: new.goal,
            notes: String::new(),
            lifecycle: Lifecycle::Created,
            status: Status::Idle,
            created_at: now,
            updated_at: now,
        };
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            &format!("INSERT INTO tasks ({TASK_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"),
            params![
                task.id,
                task.title,
                task.repo_path,
                task.worktree_path,
                task.branch,
                task.agent,
                task.goal,
                task.notes,
                task.lifecycle,
                task.status,
                task.created_at,
                task.updated_at
            ],
        )?;
        insert_event(&tx, &self.boot_id, Some(&task.id), "task_created", &serde_json::to_value(&task)?)?;
        tx.commit()?;
        Ok(task)
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        load_task(&self.conn, id)
    }

    /// Tasks ordered by creation time; archived ones only when asked.
    pub fn list_tasks(&self, include_archived: bool) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE ?1 OR lifecycle != 'archived' ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map([include_archived], task_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_goal_notes(&mut self, id: &str, goal: &str, notes: &str) -> Result<Task> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        load_task(&tx, id)?;
        tx.execute(
            "UPDATE tasks SET goal = ?2, notes = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, goal, notes, now_ms()],
        )?;
        insert_event(&tx, &self.boot_id, Some(id), "task_updated", &json!({ "goal": goal, "notes": notes }))?;
        let task = load_task(&tx, id)?;
        tx.commit()?;
        Ok(task)
    }

    /// Validated lifecycle move (see `Lifecycle::can_transition_to`), persisted with its event.
    pub fn transition_lifecycle(&mut self, id: &str, to: Lifecycle) -> Result<Task> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let from = load_task(&tx, id)?.lifecycle;
        if !from.can_transition_to(to) {
            return Err(CoreError::InvalidTransition {
                kind: "lifecycle",
                from: from.as_str().into(),
                to: to.as_str().into(),
            });
        }
        tx.execute("UPDATE tasks SET lifecycle = ?2, updated_at = ?3 WHERE id = ?1", params![id, to, now_ms()])?;
        insert_event(&tx, &self.boot_id, Some(id), "lifecycle", &json!({ "from": from, "to": to }))?;
        let task = load_task(&tx, id)?;
        tx.commit()?;
        Ok(task)
    }

    /// Compare-and-set of the task status. Fails with `StaleStatus` when the stored status
    /// is not `from`, and with `InvalidTransition` when `from -> to` is not allowed.
    pub fn set_status(&mut self, id: &str, from: Status, to: Status, cause: &str) -> Result<Event> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actual = load_task(&tx, id)?.status;
        if actual != from {
            return Err(CoreError::StaleStatus {
                task_id: id.to_string(),
                expected: from.as_str().into(),
                actual: actual.as_str().into(),
            });
        }
        if !from.can_transition_to(to) {
            return Err(CoreError::InvalidTransition {
                kind: "status",
                from: from.as_str().into(),
                to: to.as_str().into(),
            });
        }
        tx.execute("UPDATE tasks SET status = ?2, updated_at = ?3 WHERE id = ?1", params![id, to, now_ms()])?;
        let event =
            insert_event(&tx, &self.boot_id, Some(id), "status", &json!({ "from": from, "to": to, "cause": cause }))?;
        tx.commit()?;
        Ok(event)
    }

    /// Appends a free-form event (e.g. a normalized agent step) to the audit trail.
    pub fn append_event(&mut self, task_id: Option<&str>, kind: &str, payload: &Value) -> Result<Event> {
        insert_event(&self.conn, &self.boot_id, task_id, kind, payload)
    }

    /// Events with `seq > after_seq`, oldest first, at most `limit`. Clients reconcile from
    /// a snapshot plus this; the live stream is only a hint.
    pub fn events_since(&self, after_seq: u64, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, boot_id, task_id, kind, payload, ts_ms FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after_seq as i64, limit as i64], event_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn task_events(&self, task_id: &str) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, boot_id, task_id, kind, payload, ts_ms FROM events WHERE task_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([task_id], event_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core store::`
Expected: `test result: ok. 10 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): task CRUD, validated lifecycle/status transitions, append-only events

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 6: Store — conversations, sessions with the per-task lock, turns

> **Superseded in part:** see the note at Task 4; the `fix(core): harden store constraints...` commit changes store behavior here (answered-once UPDATE guard, session/conversation checks, Immediate transactions).


**Files:**
- Modify: `crates/mirante-core/src/store.rs`

`start_session` is the per-task lock: inside an immediate transaction it refuses (`TaskBusy`) while a live session exists, and the partial unique index from Task 4 backs it if anything bypasses the check. The first session moves a `Created` task to `Active`; an `Archived` task cannot start one. `end_session` releases the lock and closes any open turn, so nothing is left orphaned (spec §8 "locks left orphaned until TTL"). The transcript path + byte offset live on the session row so the tailer resumes after a restart.

- [ ] **Step 1: Write the failing tests**

Append to the end of `crates/mirante-core/src/store.rs`:
```rust
#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::test_support::TempDir;

    fn task_and_conversation(s: &mut Store) -> (Task, Conversation) {
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let c = s.create_conversation(&t.id).unwrap();
        (t, c)
    }

    #[test]
    fn native_session_id_survives_reopen() {
        let dir = TempDir::new("store-native");
        let path = dir.path().join("m.sqlite");
        let mut s = Store::open(&path).unwrap();
        let (t, c) = task_and_conversation(&mut s);
        s.set_native_session_id(&c.id, "claude-sess-1").unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        let latest = s.latest_conversation(&t.id).unwrap().unwrap();
        assert_eq!(latest.native_session_id.as_deref(), Some("claude-sess-1"));
    }

    #[test]
    fn per_task_lock_allows_one_live_session() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        let first = s.start_session(&t.id, &c.id).unwrap();
        assert!(matches!(s.start_session(&t.id, &c.id), Err(CoreError::TaskBusy(id)) if id == t.id));
        assert_eq!(s.live_session(&t.id).unwrap().unwrap().id, first.id);
        s.end_session(&first.id).unwrap();
        assert!(s.live_session(&t.id).unwrap().is_none());
        let second = s.start_session(&t.id, &c.id).unwrap();
        assert_ne!(second.id, first.id);
    }

    #[test]
    fn unique_index_backs_the_lock_even_without_the_check() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        s.start_session(&t.id, &c.id).unwrap();
        let raw = s.conn.execute(
            "INSERT INTO sessions (id, task_id, conversation_id, started_at) VALUES ('x', ?1, ?2, 0)",
            params![t.id, c.id],
        );
        assert!(raw.is_err());
    }

    #[test]
    fn first_session_activates_the_task_and_archived_tasks_cannot_start() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        let sess = s.start_session(&t.id, &c.id).unwrap();
        assert_eq!(s.get_task(&t.id).unwrap().lifecycle, Lifecycle::Active);
        s.end_session(&sess.id).unwrap();
        s.transition_lifecycle(&t.id, Lifecycle::Archived).unwrap();
        let err = s.start_session(&t.id, &c.id).unwrap_err();
        assert!(matches!(err, CoreError::InvalidTransition { kind: "session", .. }), "{err}");
    }

    #[test]
    fn turns_are_numbered_and_closed_by_end_session() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        let sess = s.start_session(&t.id, &c.id).unwrap();
        let t1 = s.start_turn(&sess.id).unwrap();
        assert!(s.end_turn(&t1.id).unwrap().ended_at.is_some());
        let t2 = s.start_turn(&sess.id).unwrap();
        assert_eq!((t1.idx, t2.idx), (1, 2));
        s.end_session(&sess.id).unwrap();
        assert!(s.turns(&sess.id).unwrap().iter().all(|t| t.ended_at.is_some()));
        let kinds: Vec<String> = s.task_events(&t.id).unwrap().into_iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                "task_created",
                "conversation_created",
                "lifecycle",
                "session_started",
                "turn_started",
                "turn_ended",
                "turn_started",
                "session_ended"
            ]
        );
    }

    #[test]
    fn transcript_offset_persists() {
        let mut s = Store::open_in_memory().unwrap();
        let (t, c) = task_and_conversation(&mut s);
        let sess = s.start_session(&t.id, &c.id).unwrap();
        s.set_transcript(&sess.id, "/home/u/.claude/projects/x/s.jsonl", 4096).unwrap();
        let got = s.get_session(&sess.id).unwrap();
        assert_eq!(got.transcript_path.as_deref(), Some("/home/u/.claude/projects/x/s.jsonl"));
        assert_eq!(got.transcript_offset, 4096);
        assert!(matches!(s.set_transcript("nope", "/x", 0), Err(CoreError::NotFound { .. })));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core store::`
Expected: compile errors such as `` no method named `create_conversation` found for struct `Store` ``.

- [ ] **Step 3: Implement**

Replace the `use crate::model::{...};` line at the top of `crates/mirante-core/src/store.rs` with:
```rust
use crate::model::{
    new_id, now_ms, AgentKind, Conversation, Event, Lifecycle, NewTask, PermissionStatus, Session, Status, Task, Turn,
};
```

Then insert, directly above the `#[cfg(test)] mod session_tests` you just appended:
```rust
const SESSION_COLUMNS: &str = "id, task_id, conversation_id, started_at, ended_at, transcript_path, transcript_offset";

fn session_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        task_id: r.get(1)?,
        conversation_id: r.get(2)?,
        started_at: r.get(3)?,
        ended_at: r.get(4)?,
        transcript_path: r.get(5)?,
        transcript_offset: r.get::<_, i64>(6)? as u64,
    })
}

fn conversation_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation { id: r.get(0)?, task_id: r.get(1)?, native_session_id: r.get(2)?, created_at: r.get(3)? })
}

fn turn_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Turn> {
    Ok(Turn { id: r.get(0)?, session_id: r.get(1)?, idx: r.get(2)?, started_at: r.get(3)?, ended_at: r.get(4)? })
}

fn load_session(conn: &Connection, id: &str) -> Result<Session> {
    conn.query_row(&format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1"), [id], session_from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound { kind: "session", id: id.to_string() })
}

impl Store {
    pub fn create_conversation(&mut self, task_id: &str) -> Result<Conversation> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        load_task(&tx, task_id)?;
        let c =
            Conversation { id: new_id(), task_id: task_id.to_string(), native_session_id: None, created_at: now_ms() };
        tx.execute(
            "INSERT INTO conversations (id, task_id, native_session_id, created_at) VALUES (?1, ?2, NULL, ?3)",
            params![c.id, c.task_id, c.created_at],
        )?;
        insert_event(&tx, &self.boot_id, Some(task_id), "conversation_created", &json!({ "conversation_id": c.id }))?;
        tx.commit()?;
        Ok(c)
    }

    /// Records the agent's own session id (from `SessionStart`) so the task can `--resume`.
    pub fn set_native_session_id(&mut self, conversation_id: &str, native_session_id: &str) -> Result<()> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task_id: String = tx
            .query_row("SELECT task_id FROM conversations WHERE id = ?1", [conversation_id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| CoreError::NotFound { kind: "conversation", id: conversation_id.to_string() })?;
        tx.execute(
            "UPDATE conversations SET native_session_id = ?2 WHERE id = ?1",
            params![conversation_id, native_session_id],
        )?;
        insert_event(
            &tx,
            &self.boot_id,
            Some(&task_id),
            "native_session_id",
            &json!({ "conversation_id": conversation_id, "native_session_id": native_session_id }),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn latest_conversation(&self, task_id: &str) -> Result<Option<Conversation>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, task_id, native_session_id, created_at FROM conversations
                 WHERE task_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1",
                [task_id],
                conversation_from_row,
            )
            .optional()?)
    }

    /// Starts a process activation. Per-task lock: fails with `TaskBusy` while another session
    /// of the task is live (also enforced by a partial unique index). A `Created` task becomes
    /// `Active`; an `Archived` task cannot start sessions.
    pub fn start_session(&mut self, task_id: &str, conversation_id: &str) -> Result<Session> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = load_task(&tx, task_id)?;
        if task.lifecycle == Lifecycle::Archived {
            return Err(CoreError::InvalidTransition { kind: "session", from: "archived".into(), to: "live".into() });
        }
        let busy: Option<String> = tx
            .query_row("SELECT id FROM sessions WHERE task_id = ?1 AND ended_at IS NULL", [task_id], |r| r.get(0))
            .optional()?;
        if busy.is_some() {
            return Err(CoreError::TaskBusy(task_id.to_string()));
        }
        let s = Session {
            id: new_id(),
            task_id: task_id.to_string(),
            conversation_id: conversation_id.to_string(),
            started_at: now_ms(),
            ended_at: None,
            transcript_path: None,
            transcript_offset: 0,
        };
        tx.execute(
            &format!("INSERT INTO sessions ({SESSION_COLUMNS}) VALUES (?1, ?2, ?3, ?4, NULL, NULL, 0)"),
            params![s.id, s.task_id, s.conversation_id, s.started_at],
        )?;
        if task.lifecycle == Lifecycle::Created {
            tx.execute(
                "UPDATE tasks SET lifecycle = 'active', updated_at = ?2 WHERE id = ?1",
                params![task_id, now_ms()],
            )?;
            insert_event(
                &tx,
                &self.boot_id,
                Some(task_id),
                "lifecycle",
                &json!({ "from": "created", "to": "active" }),
            )?;
        }
        insert_event(
            &tx,
            &self.boot_id,
            Some(task_id),
            "session_started",
            &json!({ "session_id": s.id, "conversation_id": conversation_id }),
        )?;
        tx.commit()?;
        Ok(s)
    }

    /// Ends a session (releases the per-task lock) and closes any turn still open in it.
    pub fn end_session(&mut self, session_id: &str) -> Result<Session> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let s = load_session(&tx, session_id)?;
        if s.ended_at.is_none() {
            let now = now_ms();
            tx.execute("UPDATE sessions SET ended_at = ?2 WHERE id = ?1", params![session_id, now])?;
            tx.execute(
                "UPDATE turns SET ended_at = ?2 WHERE session_id = ?1 AND ended_at IS NULL",
                params![session_id, now],
            )?;
            insert_event(&tx, &self.boot_id, Some(&s.task_id), "session_ended", &json!({ "session_id": session_id }))?;
        }
        let s = load_session(&tx, session_id)?;
        tx.commit()?;
        Ok(s)
    }

    pub fn live_session(&self, task_id: &str) -> Result<Option<Session>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE task_id = ?1 AND ended_at IS NULL"),
                [task_id],
                session_from_row,
            )
            .optional()?)
    }

    pub fn get_session(&self, id: &str) -> Result<Session> {
        load_session(&self.conn, id)
    }

    /// Persists where the transcript tailer is. Not audited: it moves on every poll.
    pub fn set_transcript(&mut self, session_id: &str, path: &str, offset: u64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE sessions SET transcript_path = ?2, transcript_offset = ?3 WHERE id = ?1",
            params![session_id, path, offset as i64],
        )?;
        if n == 0 {
            return Err(CoreError::NotFound { kind: "session", id: session_id.to_string() });
        }
        Ok(())
    }

    pub fn start_turn(&mut self, session_id: &str) -> Result<Turn> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let s = load_session(&tx, session_id)?;
        let idx: i64 =
            tx.query_row("SELECT COALESCE(MAX(idx), 0) + 1 FROM turns WHERE session_id = ?1", [session_id], |r| {
                r.get(0)
            })?;
        let t = Turn { id: new_id(), session_id: session_id.to_string(), idx, started_at: now_ms(), ended_at: None };
        tx.execute(
            "INSERT INTO turns (id, session_id, idx, started_at, ended_at) VALUES (?1, ?2, ?3, ?4, NULL)",
            params![t.id, t.session_id, t.idx, t.started_at],
        )?;
        insert_event(
            &tx,
            &self.boot_id,
            Some(&s.task_id),
            "turn_started",
            &json!({ "session_id": session_id, "turn_id": t.id, "idx": idx }),
        )?;
        tx.commit()?;
        Ok(t)
    }

    pub fn end_turn(&mut self, turn_id: &str) -> Result<Turn> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let t = tx
            .query_row(
                "SELECT id, session_id, idx, started_at, ended_at FROM turns WHERE id = ?1",
                [turn_id],
                turn_from_row,
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound { kind: "turn", id: turn_id.to_string() })?;
        if t.ended_at.is_none() {
            let task_id = load_session(&tx, &t.session_id)?.task_id;
            tx.execute("UPDATE turns SET ended_at = ?2 WHERE id = ?1", params![turn_id, now_ms()])?;
            insert_event(
                &tx,
                &self.boot_id,
                Some(&task_id),
                "turn_ended",
                &json!({ "turn_id": turn_id, "idx": t.idx }),
            )?;
        }
        let t = tx.query_row(
            "SELECT id, session_id, idx, started_at, ended_at FROM turns WHERE id = ?1",
            [turn_id],
            turn_from_row,
        )?;
        tx.commit()?;
        Ok(t)
    }

    pub fn turns(&self, session_id: &str) -> Result<Vec<Turn>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, idx, started_at, ended_at FROM turns WHERE session_id = ?1 ORDER BY idx",
        )?;
        let rows = stmt.query_map([session_id], turn_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core store::`
Expected: `test result: ok. 16 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): conversations, sessions with per-task lock, turns and transcript offset

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 7: Store — permission requests

> **Superseded in part:** see the note at Task 4; the `fix(core): harden store constraints...` commit changes store behavior here (answered-once UPDATE guard, session/conversation checks, Immediate transactions).


**Files:**
- Modify: `crates/mirante-core/src/store.rs`

Permission cards are rows, so a pending request survives an app restart (spec §2.5). An answer is `AllowedOnce` or `Denied`, exactly once; there is no "always allow" value at all (spec §5, FINDINGS rule 6).

- [ ] **Step 1: Write the failing tests**

Append to the end of `crates/mirante-core/src/store.rs`:
```rust
#[cfg(test)]
mod permission_tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn pending_request_survives_restart_and_is_answered_once() {
        let dir = TempDir::new("store-perm");
        let path = dir.path().join("m.sqlite");
        let mut s = Store::open(&path).unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let c = s.create_conversation(&t.id).unwrap();
        let sess = s.start_session(&t.id, &c.id).unwrap();
        let p = s.create_permission_request(&sess.id, "Bash", "touch a.txt").unwrap();
        drop(s);

        let mut s = Store::open(&path).unwrap();
        let pending = s.pending_permissions(&t.id).unwrap();
        assert_eq!(pending, vec![p.clone()]);
        let answered = s.answer_permission(&p.id, PermissionStatus::Denied).unwrap();
        assert_eq!(answered.status, PermissionStatus::Denied);
        assert!(answered.answered_at.is_some());
        assert!(s.pending_permissions(&t.id).unwrap().is_empty());
        let again = s.answer_permission(&p.id, PermissionStatus::AllowedOnce).unwrap_err();
        assert!(matches!(again, CoreError::InvalidTransition { kind: "permission", .. }), "{again}");
    }

    #[test]
    fn pending_is_not_an_answer() {
        let mut s = Store::open_in_memory().unwrap();
        let t = s.create_task(sample_task("feat-a")).unwrap();
        let c = s.create_conversation(&t.id).unwrap();
        let sess = s.start_session(&t.id, &c.id).unwrap();
        let p = s.create_permission_request(&sess.id, "Edit", "src/a.rs").unwrap();
        assert!(s.answer_permission(&p.id, PermissionStatus::Pending).is_err());
        assert_eq!(
            s.answer_permission(&p.id, PermissionStatus::AllowedOnce).unwrap().status,
            PermissionStatus::AllowedOnce
        );
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core store::`
Expected: compile errors such as `` no method named `create_permission_request` found for struct `Store` ``.

- [ ] **Step 3: Implement**

Replace the `use crate::model::{...};` block at the top of `crates/mirante-core/src/store.rs` with:
```rust
use crate::model::{
    new_id, now_ms, AgentKind, Conversation, Event, Lifecycle, NewTask, PermissionRequest, PermissionStatus, Session,
    Status, Task, Turn,
};
```

Then insert, directly above the `#[cfg(test)] mod permission_tests` you just appended:
```rust
const PERMISSION_COLUMNS: &str = "id, task_id, session_id, tool, detail, status, created_at, answered_at";

fn permission_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PermissionRequest> {
    Ok(PermissionRequest {
        id: r.get(0)?,
        task_id: r.get(1)?,
        session_id: r.get(2)?,
        tool: r.get(3)?,
        detail: r.get(4)?,
        status: r.get(5)?,
        created_at: r.get(6)?,
        answered_at: r.get(7)?,
    })
}

impl Store {
    pub fn create_permission_request(
        &mut self,
        session_id: &str,
        tool: &str,
        detail: &str,
    ) -> Result<PermissionRequest> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let s = load_session(&tx, session_id)?;
        let p = PermissionRequest {
            id: new_id(),
            task_id: s.task_id.clone(),
            session_id: session_id.to_string(),
            tool: tool.to_string(),
            detail: detail.to_string(),
            status: PermissionStatus::Pending,
            created_at: now_ms(),
            answered_at: None,
        };
        tx.execute(
            &format!(
                "INSERT INTO permission_requests ({PERMISSION_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)"
            ),
            params![p.id, p.task_id, p.session_id, p.tool, p.detail, p.status, p.created_at],
        )?;
        insert_event(
            &tx,
            &self.boot_id,
            Some(&p.task_id),
            "permission_requested",
            &json!({ "permission_id": p.id, "tool": tool, "detail": detail }),
        )?;
        tx.commit()?;
        Ok(p)
    }

    /// Answers a pending request exactly once. `answer` must be `AllowedOnce` or `Denied`;
    /// there is no "always allow".
    pub fn answer_permission(&mut self, id: &str, answer: PermissionStatus) -> Result<PermissionRequest> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = tx
            .query_row(
                &format!("SELECT {PERMISSION_COLUMNS} FROM permission_requests WHERE id = ?1"),
                [id],
                permission_from_row,
            )
            .optional()?
            .ok_or_else(|| CoreError::NotFound { kind: "permission_request", id: id.to_string() })?;
        if p.status != PermissionStatus::Pending || answer == PermissionStatus::Pending {
            return Err(CoreError::InvalidTransition {
                kind: "permission",
                from: p.status.as_str().into(),
                to: answer.as_str().into(),
            });
        }
        tx.execute(
            "UPDATE permission_requests SET status = ?2, answered_at = ?3 WHERE id = ?1",
            params![id, answer, now_ms()],
        )?;
        insert_event(
            &tx,
            &self.boot_id,
            Some(&p.task_id),
            "permission_answered",
            &json!({ "permission_id": id, "answer": answer }),
        )?;
        let p = tx.query_row(
            &format!("SELECT {PERMISSION_COLUMNS} FROM permission_requests WHERE id = ?1"),
            [id],
            permission_from_row,
        )?;
        tx.commit()?;
        Ok(p)
    }

    pub fn pending_permissions(&self, task_id: &str) -> Result<Vec<PermissionRequest>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PERMISSION_COLUMNS} FROM permission_requests WHERE task_id = ?1 AND status = 'pending' ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map([task_id], permission_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core store::`
Expected: `test result: ok. 18 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): persisted permission requests answered once

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 8: Status engine — hook rules

> **Superseded in part:** commit "fix(core): status engine handles resume/clear, dialog clearing, late Stop, slash commands" supersedes this task's code for `SessionStart` by `source`, permission-dialog clearing (`PreToolUse` never clears; `PostToolUse` by `tool_use_id`; `Input::PermissionAnswered`) and slash-command prompts. `src/status.rs` is the source of truth.

**Files:**
- Create: `crates/mirante-core/src/status.rs`
- Modify: `crates/mirante-core/src/lib.rs`

Adapted from `spike_kit::status::next_status`, which mapped each hook independently and produced the S3 mismatches. The engine is now stateful, one per session, with an explicit turn:
- only a main-agent `UserPromptSubmit` opens a turn; `PreToolUse`/`PostToolUse` never do (this is what neutralises the stray post-`Stop` `PreToolUse`, with or without `agent_id`);
- main-agent `Stop`, the idle `Notification` (FINDINGS rule 3) and `SessionEnd` close it;
- `SessionStart` means `Idle` only when no turn is open (it also fires after a mid-turn compaction);
- a pending permission dialog remembers which agent raised it, and only that agent's next tool event clears it (a parallel side agent's tool use must not hide the main agent's dialog). A side agent's `PermissionRequest` still means `NeedsYou`: its dialog blocks the session;
- `Ended` is terminal for the engine (a resumed session gets a new engine).

Transcript input is accepted but ignored until Task 9.

- [ ] **Step 1: Register the module and write the failing tests**

`crates/mirante-core/src/lib.rs`:
```rust
//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod error;
pub mod hook;
pub mod model;
pub mod status;
pub mod store;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
```

`crates/mirante-core/src/status.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn hook(name: &str) -> Input {
        Input::Hook(HookEvent::named(name))
    }
    fn side(name: &str) -> Input {
        Input::Hook(HookEvent {
            agent_id: Some("a-1".into()),
            agent_type: Some("general-purpose".into()),
            ..HookEvent::named(name)
        })
    }
    fn notification(msg: &str) -> Input {
        Input::Hook(HookEvent { message: Some(msg.into()), ..HookEvent::named("Notification") })
    }
    fn statuses(e: &mut StatusEngine, inputs: &[Input]) -> Vec<Status> {
        e.apply_all(inputs).into_iter().map(|t| t.to).collect()
    }

    #[test]
    fn plain_turn_with_tools() {
        let mut e = StatusEngine::new();
        let got = statuses(
            &mut e,
            &[
                hook("SessionStart"),
                hook("UserPromptSubmit"),
                hook("PreToolUse"),
                hook("PostToolUse"),
                hook("Stop"),
                hook("SessionEnd"),
            ],
        );
        assert_eq!(got, vec![Status::Working, Status::Idle, Status::Ended]);
    }

    #[test]
    fn allowed_permission_goes_needs_you_then_working() {
        let mut e = StatusEngine::new();
        let t = e.apply_all(&[
            hook("UserPromptSubmit"),
            hook("PreToolUse"),
            hook("PermissionRequest"),
            hook("PostToolUse"),
            hook("Stop"),
        ]);
        let got: Vec<(Status, &str)> = t.iter().map(|t| (t.to, t.cause.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (Status::Working, "hook:UserPromptSubmit"),
                (Status::NeedsYou, "hook:PermissionRequest"),
                (Status::Working, "hook:PostToolUse"),
                (Status::Idle, "hook:Stop"),
            ]
        );
    }

    #[test]
    fn idle_notification_is_idle_other_notifications_need_you() {
        let mut e = StatusEngine::new();
        e.apply(&hook("UserPromptSubmit"));
        assert_eq!(e.apply(&notification("Claude needs your permission to use Bash")).unwrap().to, Status::NeedsYou);
        assert_eq!(e.apply(&notification("Claude is waiting for your input")).unwrap().to, Status::Idle);
        assert!(!e.turn_open());
    }

    #[test]
    fn session_start_mid_turn_does_not_reset_to_idle() {
        let mut e = StatusEngine::new();
        e.apply(&hook("UserPromptSubmit"));
        assert_eq!(e.apply(&hook("SessionStart")), None);
        assert_eq!(e.status(), Status::Working);
    }

    #[test]
    fn ended_is_terminal_for_the_engine() {
        let mut e = StatusEngine::new();
        e.apply(&hook("SessionEnd"));
        assert_eq!(e.apply(&hook("UserPromptSubmit")), None);
        assert_eq!(e.status(), Status::Ended);
    }

    #[test]
    fn unknown_and_neutral_events_change_nothing() {
        let mut e = StatusEngine::new();
        e.apply(&hook("UserPromptSubmit"));
        assert!(e.apply_all(&[hook("SubagentStop"), hook("PreCompact"), hook("SomethingNew")]).is_empty());
        assert_eq!(e.status(), Status::Working);
    }

    #[test]
    fn side_agent_stop_and_prompt_do_not_move_turn_boundaries() {
        let mut e = StatusEngine::new();
        e.apply(&hook("UserPromptSubmit"));
        assert_eq!(e.apply(&side("Stop")), None);
        assert!(e.turn_open());
        e.apply(&hook("Stop"));
        assert_eq!(e.apply(&side("UserPromptSubmit")), None);
        assert_eq!(e.status(), Status::Idle);
    }

    #[test]
    fn side_agent_tool_use_does_not_clear_main_agent_dialog() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), hook("PreToolUse"), hook("PermissionRequest")]);
        assert_eq!(e.apply(&side("PostToolUse")), None);
        assert_eq!(e.status(), Status::NeedsYou);
        assert_eq!(e.apply(&hook("PostToolUse")).unwrap().to, Status::Working);
    }

    #[test]
    fn side_agent_permission_after_turn_needs_you_until_it_proceeds() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), hook("Stop")]);
        assert_eq!(e.apply(&side("PermissionRequest")).unwrap().to, Status::NeedsYou);
        assert_eq!(e.apply(&hook("PreToolUse")), None);
        assert_eq!(e.apply(&side("PostToolUse")).unwrap().to, Status::Idle);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core status::`
Expected: compile errors such as `` cannot find type `StatusEngine` in this scope ``.

- [ ] **Step 3: Implement above the test module**

Insert at the top of `crates/mirante-core/src/status.rs`:
```rust
//! Status fusion: one engine per session, fed hook events and transcript lines in arrival
//! order (transcript lines in file order). Rules come from docs/spikes/FINDINGS.md:
//!
//! - A turn opens only on a main-agent `UserPromptSubmit` hook or a main-chain transcript
//!   prompt line. Tool events never open a turn, so a stray `PreToolUse` after `Stop`
//!   (side agent, S3 mismatch 2) cannot flip the task back to `Working`.
//! - A turn closes on a main-agent `Stop`, the idle `Notification`, or the transcript's
//!   `system` `turn_duration` / `stop_hook_summary` line. The transcript end covers the
//!   deny path, where no hook fires (S3 mismatch 1).
//! - Transcript turns are counted on their own; a transcript turn end closes the fused turn
//!   only when both name the same turn, so a late transcript line from the previous turn
//!   cannot end the turn the hooks already opened.
//! - Side-agent events (`agent_id` present; transcript `isSidechain` / `agentId` /
//!   `parentToolUseID`) never open or close a turn. A side agent's `PermissionRequest`
//!   still means `NeedsYou`: its dialog blocks the session.
use serde_json::Value;

use crate::hook::HookEvent;
use crate::model::Status;

/// Text of the Notification Claude Code sends after ~60s without input (2.1.285:
/// "Claude is waiting for your input"). The agent is idle; it does not need you.
pub const IDLE_NOTIFICATION: &str = "waiting for your input";

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Hook(HookEvent),
    Transcript(Value),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: Status,
    pub to: Status,
    /// `hook:<event name>` or `transcript:<line kind>`; stored in the status event.
    pub cause: String,
}

/// Who asked for permission: `None` is the main agent, `Some(id)` a side agent.
type Asker = Option<String>;

#[derive(Debug, Clone)]
pub struct StatusEngine {
    status: Status,
    turn_open: bool,
    /// Number of turns opened (the fused turn counter).
    turn_seq: u64,
    /// Pending permission dialog and who raised it.
    pending: Option<Asker>,
}

impl Default for StatusEngine {
    fn default() -> Self {
        StatusEngine::new()
    }
}

impl StatusEngine {
    pub fn new() -> Self {
        StatusEngine {
            status: Status::Idle,
            turn_open: false,
            turn_seq: 0,
            pending: None,
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn turn_open(&self) -> bool {
        self.turn_open
    }

    /// Applies one input; returns the transition if the status changed.
    pub fn apply(&mut self, input: &Input) -> Option<Transition> {
        if self.status == Status::Ended {
            return None;
        }
        match input {
            Input::Hook(ev) => self.apply_hook(ev),
            // Transcript fusion is added in the next task.
            Input::Transcript(_) => None,
        }
    }

    /// Applies inputs in order and returns only the transitions.
    pub fn apply_all<'a>(&mut self, inputs: impl IntoIterator<Item = &'a Input>) -> Vec<Transition> {
        inputs.into_iter().filter_map(|i| self.apply(i)).collect()
    }

    fn set(&mut self, to: Status, cause: String) -> Option<Transition> {
        if !self.status.can_transition_to(to) {
            return None;
        }
        let from = self.status;
        self.status = to;
        Some(Transition { from, to, cause })
    }

    fn open_turn(&mut self) {
        self.turn_open = true;
        self.turn_seq += 1;
        self.pending = None;
    }

    fn close_turn(&mut self) {
        self.turn_open = false;
        self.pending = None;
    }

    fn apply_hook(&mut self, ev: &HookEvent) -> Option<Transition> {
        let cause = format!("hook:{}", ev.hook_event_name);
        let side = ev.is_side_agent();
        match ev.hook_event_name.as_str() {
            // `SessionStart` also fires after a mid-turn compaction; it only means idle
            // when no turn is open.
            "SessionStart" if !self.turn_open => self.set(Status::Idle, cause),
            "UserPromptSubmit" if !side && !self.turn_open => {
                self.open_turn();
                self.set(Status::Working, cause)
            }
            "PreToolUse" | "PostToolUse" => {
                let asker: Asker = ev.agent_id.clone();
                match &self.pending {
                    // A dialog is open: only the agent that asked can clear it.
                    Some(p) if *p != asker => None,
                    Some(_) => {
                        self.pending = None;
                        let to = if self.turn_open { Status::Working } else { Status::Idle };
                        self.set(to, cause)
                    }
                    None if self.turn_open => self.set(Status::Working, cause),
                    None => None,
                }
            }
            "PermissionRequest" => {
                self.pending = Some(ev.agent_id.clone());
                self.set(Status::NeedsYou, cause)
            }
            "Notification" if ev.message.as_deref().is_some_and(|m| m.contains(IDLE_NOTIFICATION)) => {
                self.close_turn();
                self.set(Status::Idle, cause)
            }
            "Notification" => self.set(Status::NeedsYou, cause),
            "Stop" if !side => {
                self.close_turn();
                self.set(Status::Idle, cause)
            }
            "SessionEnd" => {
                self.close_turn();
                self.set(Status::Ended, cause)
            }
            _ => None,
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core status::`
Expected: `test result: ok. 9 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): stateful hook status engine with turn gating and agent identity

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 9: Status engine — transcript fusion (both S3 failure classes)

> **Superseded in part:** commit "fix(core): status engine handles resume/clear, dialog clearing, late Stop, slash commands" supersedes this task's code for late hook `Stop`s (paired with `stop_hook_summary` lines) and adds fixtures from the real Task 8 steer run and queued probe. `src/status.rs` is the source of truth.

**Files:**
- Modify: `crates/mirante-core/src/status.rs`

FINDINGS rule 1: the transcript's `system` lines with subtype `turn_duration` / `stop_hook_summary` are authoritative turn ends; sidechain lines are ignored; inputs are applied in arrival order (transcript lines in file order). The transcript keeps its **own** turn counter (`tx_turn`); its end closes the fused turn only when both refer to the same turn, so a transcript line from the previous turn that arrives after the hooks opened the next turn is ignored. The counters re-align whenever both sources are quiescent, so a hook-only turn (no transcript prompt line) cannot desync later turns. A transcript prompt that arrives before its `UserPromptSubmit` opens the turn itself; `promptSource: "queued"` prompts open a turn too (a queued message delivered after `Stop`).

- [ ] **Step 1: Write the failing tests**

Append to the end of `crates/mirante-core/src/status.rs`:
```rust
#[cfg(test)]
mod fusion_tests {
    use super::*;
    use serde_json::json;

    fn hook(name: &str) -> Input {
        Input::Hook(HookEvent::named(name))
    }
    fn side_hook(name: &str) -> Input {
        Input::Hook(HookEvent { agent_id: Some("a-1".into()), ..HookEvent::named(name) })
    }
    fn tx(v: Value) -> Input {
        Input::Transcript(v)
    }
    fn prompt() -> Input {
        tx(
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "typed", "message": {"content": "touch s3-denied.txt"}}),
        )
    }
    fn system(subtype: &str) -> Input {
        tx(json!({"type": "system", "subtype": subtype}))
    }

    /// S3 mismatch 1 (3/3 runs): after "No" on a permission dialog no hook fires; the
    /// transcript ends the turn with `turn_duration` only (no `stop_hook_summary`).
    #[test]
    fn deny_path_is_closed_by_transcript_turn_duration() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("SessionStart"), hook("UserPromptSubmit"), hook("PreToolUse"), hook("PermissionRequest")]);
        assert_eq!(e.status(), Status::NeedsYou);
        let t = e.apply_all(&[
            prompt(),
            tx(json!({"type": "assistant", "message": {"stop_reason": "tool_use", "content": [{"type": "tool_use", "name": "Bash"}]}})),
            tx(json!({"type": "user", "message": {"content": [{"type": "text", "text": "[Request interrupted by user for tool use]"}]}})),
            system("turn_duration"),
        ]);
        assert_eq!(
            t,
            vec![Transition { from: Status::NeedsYou, to: Status::Idle, cause: "transcript:turn_duration".into() }]
        );
        assert!(!e.turn_open());
    }

    /// S3 mismatch 2 (run 2): a PreToolUse with the same session id arrives ~1.7s after
    /// Stop, from a side agent. With or without `agent_id`, it must not flip to Working.
    #[test]
    fn stray_pre_tool_use_after_stop_stays_idle() {
        for stray in [hook("PreToolUse"), side_hook("PreToolUse")] {
            let mut e = StatusEngine::new();
            e.apply_all(&[
                hook("UserPromptSubmit"),
                hook("PreToolUse"),
                hook("PermissionRequest"),
                hook("PostToolUse"),
                hook("Stop"),
            ]);
            assert_eq!(e.apply(&stray), None);
            assert_eq!(e.status(), Status::Idle);
        }
    }

    #[test]
    fn transcript_prompt_before_hook_opens_the_turn_once() {
        let mut e = StatusEngine::new();
        assert_eq!(e.apply(&prompt()).unwrap().cause, "transcript:prompt");
        assert_eq!(e.apply(&hook("UserPromptSubmit")), None);
        assert_eq!(e.apply(&hook("Stop")).unwrap().to, Status::Idle);
        // The transcript's own end for the same turn is now a no-op.
        assert_eq!(e.apply(&system("stop_hook_summary")), None);
        assert_eq!(e.apply(&system("turn_duration")), None);
    }

    #[test]
    fn late_transcript_end_of_previous_turn_does_not_end_the_new_turn() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), prompt(), hook("Stop"), hook("UserPromptSubmit")]);
        assert_eq!(e.status(), Status::Working);
        assert_eq!(e.apply(&system("stop_hook_summary")), None);
        assert_eq!(e.apply(&system("turn_duration")), None);
        assert_eq!(e.status(), Status::Working);
        // The new turn's own prompt + end still close it.
        e.apply(&prompt());
        assert_eq!(e.apply(&system("turn_duration")).unwrap().to, Status::Idle);
    }

    #[test]
    fn hook_only_turn_does_not_desync_later_transcript_ends() {
        let mut e = StatusEngine::new();
        // Turn 1 has no transcript prompt line (e.g. a hook-only prompt).
        e.apply_all(&[hook("UserPromptSubmit"), hook("Stop")]);
        // Turn 2 is the deny path: only the transcript can close it.
        e.apply_all(&[hook("UserPromptSubmit"), hook("PreToolUse"), hook("PermissionRequest"), prompt()]);
        assert_eq!(e.apply(&system("turn_duration")).unwrap().to, Status::Idle);
    }

    #[test]
    fn queued_prompt_line_opens_a_turn() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), prompt(), hook("Stop"), system("turn_duration")]);
        let queued = tx(json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued", "message": {}}));
        assert_eq!(e.apply(&queued).unwrap().to, Status::Working);
        assert_eq!(e.apply(&hook("PreToolUse")), None);
        assert_eq!(e.apply(&system("turn_duration")).unwrap().to, Status::Idle);
    }

    #[test]
    fn sidechain_transcript_lines_are_ignored() {
        let mut e = StatusEngine::new();
        e.apply_all(&[hook("UserPromptSubmit"), prompt()]);
        let side_lines = [
            tx(json!({"type": "system", "subtype": "turn_duration", "isSidechain": true})),
            tx(json!({"type": "system", "subtype": "turn_duration", "agentId": "a-1"})),
            tx(json!({"type": "system", "subtype": "stop_hook_summary", "parentToolUseID": "toolu_1"})),
            tx(json!({"type": "user", "isSidechain": true, "origin": {"kind": "human"}})),
        ];
        assert!(e.apply_all(&side_lines).is_empty());
        assert_eq!(e.status(), Status::Working);
    }

    #[test]
    fn tool_result_lines_are_not_prompts() {
        let mut e = StatusEngine::new();
        let tool_result =
            tx(json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1"}]}}));
        assert_eq!(e.apply(&tool_result), None);
        assert_eq!(e.status(), Status::Idle);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core status::`
Expected: `test result: FAILED. 12 passed; 5 failed`. The failures are `deny_path_is_closed_by_transcript_turn_duration`, `transcript_prompt_before_hook_opens_the_turn_once`, `late_transcript_end_of_previous_turn_does_not_end_the_new_turn`, `hook_only_turn_does_not_desync_later_transcript_ends` and `queued_prompt_line_opens_a_turn`. `stray_pre_tool_use_after_stop_stays_idle`, `sidechain_transcript_lines_are_ignored` and `tool_result_lines_are_not_prompts` already pass: they pin Task 8's turn gating so the fusion cannot regress it.

- [ ] **Step 3: Implement the fusion**

Replace everything in `crates/mirante-core/src/status.rs` above `#[cfg(test)]` / `mod tests` with:
```rust
//! Status fusion: one engine per session, fed hook events and transcript lines in arrival
//! order (transcript lines in file order). Rules come from docs/spikes/FINDINGS.md:
//!
//! - A turn opens only on a main-agent `UserPromptSubmit` hook or a main-chain transcript
//!   prompt line. Tool events never open a turn, so a stray `PreToolUse` after `Stop`
//!   (side agent, S3 mismatch 2) cannot flip the task back to `Working`.
//! - A turn closes on a main-agent `Stop`, the idle `Notification`, or the transcript's
//!   `system` `turn_duration` / `stop_hook_summary` line. The transcript end covers the
//!   deny path, where no hook fires (S3 mismatch 1).
//! - Transcript turns are counted on their own; a transcript turn end closes the fused turn
//!   only when both name the same turn, so a late transcript line from the previous turn
//!   cannot end the turn the hooks already opened.
//! - Side-agent events (`agent_id` present; transcript `isSidechain` / `agentId` /
//!   `parentToolUseID`) never open or close a turn. A side agent's `PermissionRequest`
//!   still means `NeedsYou`: its dialog blocks the session.
use serde_json::Value;

use crate::hook::HookEvent;
use crate::model::Status;

/// Text of the Notification Claude Code sends after ~60s without input (2.1.285:
/// "Claude is waiting for your input"). The agent is idle; it does not need you.
pub const IDLE_NOTIFICATION: &str = "waiting for your input";

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Hook(HookEvent),
    Transcript(Value),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: Status,
    pub to: Status,
    /// `hook:<event name>` or `transcript:<line kind>`; stored in the status event.
    pub cause: String,
}

/// Who asked for permission: `None` is the main agent, `Some(id)` a side agent.
type Asker = Option<String>;

#[derive(Debug, Clone)]
pub struct StatusEngine {
    status: Status,
    turn_open: bool,
    /// Number of turns opened by either source (the fused turn counter).
    turn_seq: u64,
    /// Number of turns the transcript itself has opened, and whether one is open there.
    tx_turn: u64,
    tx_in_turn: bool,
    /// Pending permission dialog and who raised it.
    pending: Option<Asker>,
}

impl Default for StatusEngine {
    fn default() -> Self {
        StatusEngine::new()
    }
}

impl StatusEngine {
    pub fn new() -> Self {
        StatusEngine {
            status: Status::Idle,
            turn_open: false,
            turn_seq: 0,
            tx_turn: 0,
            tx_in_turn: false,
            pending: None,
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn turn_open(&self) -> bool {
        self.turn_open
    }

    /// Applies one input; returns the transition if the status changed.
    pub fn apply(&mut self, input: &Input) -> Option<Transition> {
        if self.status == Status::Ended {
            return None;
        }
        match input {
            Input::Hook(ev) => self.apply_hook(ev),
            Input::Transcript(line) => self.apply_transcript(line),
        }
    }

    /// Applies inputs in order and returns only the transitions.
    pub fn apply_all<'a>(&mut self, inputs: impl IntoIterator<Item = &'a Input>) -> Vec<Transition> {
        inputs.into_iter().filter_map(|i| self.apply(i)).collect()
    }

    fn set(&mut self, to: Status, cause: String) -> Option<Transition> {
        if !self.status.can_transition_to(to) {
            return None;
        }
        let from = self.status;
        self.status = to;
        Some(Transition { from, to, cause })
    }

    fn open_turn(&mut self) {
        self.turn_open = true;
        self.turn_seq += 1;
        self.pending = None;
    }

    fn close_turn(&mut self) {
        self.turn_open = false;
        self.pending = None;
        // Both sources quiescent: re-align the transcript's turn counter with the fused one,
        // so a hook-only turn (no transcript prompt line) cannot desync later turns.
        if !self.tx_in_turn {
            self.tx_turn = self.turn_seq;
        }
    }

    fn apply_hook(&mut self, ev: &HookEvent) -> Option<Transition> {
        let cause = format!("hook:{}", ev.hook_event_name);
        let side = ev.is_side_agent();
        match ev.hook_event_name.as_str() {
            // `SessionStart` also fires after a mid-turn compaction; it only means idle
            // when no turn is open.
            "SessionStart" if !self.turn_open => self.set(Status::Idle, cause),
            "UserPromptSubmit" if !side && !self.turn_open => {
                self.open_turn();
                self.set(Status::Working, cause)
            }
            "PreToolUse" | "PostToolUse" => {
                let asker: Asker = ev.agent_id.clone();
                match &self.pending {
                    // A dialog is open: only the agent that asked can clear it.
                    Some(p) if *p != asker => None,
                    Some(_) => {
                        self.pending = None;
                        let to = if self.turn_open { Status::Working } else { Status::Idle };
                        self.set(to, cause)
                    }
                    None if self.turn_open => self.set(Status::Working, cause),
                    None => None,
                }
            }
            "PermissionRequest" => {
                self.pending = Some(ev.agent_id.clone());
                self.set(Status::NeedsYou, cause)
            }
            "Notification" if ev.message.as_deref().is_some_and(|m| m.contains(IDLE_NOTIFICATION)) => {
                self.close_turn();
                self.set(Status::Idle, cause)
            }
            "Notification" => self.set(Status::NeedsYou, cause),
            "Stop" if !side => {
                self.close_turn();
                self.set(Status::Idle, cause)
            }
            "SessionEnd" => {
                self.close_turn();
                self.set(Status::Ended, cause)
            }
            _ => None,
        }
    }

    fn apply_transcript(&mut self, v: &Value) -> Option<Transition> {
        if is_sidechain(v) {
            return None;
        }
        if is_prompt(v) {
            if self.tx_in_turn {
                return None;
            }
            self.tx_in_turn = true;
            self.tx_turn += 1;
            if self.tx_turn > self.turn_seq {
                // The transcript saw the prompt before the hook did.
                self.open_turn();
                return self.set(Status::Working, "transcript:prompt".into());
            }
            return None;
        }
        if let Some(kind) = turn_end_kind(v) {
            if !self.tx_in_turn {
                return None;
            }
            self.tx_in_turn = false;
            if self.turn_open && self.tx_turn == self.turn_seq {
                self.close_turn();
                return self.set(Status::Idle, format!("transcript:{kind}"));
            }
            if !self.turn_open {
                self.tx_turn = self.turn_seq;
            }
        }
        None
    }
}

/// Subagent / sidechain transcript lines (own turns, own stop reasons).
pub fn is_sidechain(v: &Value) -> bool {
    v["isSidechain"] == true || !v["agentId"].is_null() || !v["parentToolUseID"].is_null()
}

/// A line that starts a turn: a typed prompt (`origin.kind == "human"`), a headless prompt
/// (`promptSource == "sdk"`), or a queued message delivered after a turn
/// (`promptSource == "queued"`). Tool results have no `origin` and are not prompts.
pub fn is_prompt(v: &Value) -> bool {
    v["type"] == "user"
        && (v["origin"]["kind"] == "human" || v["promptSource"] == "sdk" || v["promptSource"] == "queued")
}

/// `Some(subtype)` for the transcript's authoritative turn-end lines.
pub fn turn_end_kind(v: &Value) -> Option<&str> {
    if v["type"] != "system" {
        return None;
    }
    v["subtype"].as_str().filter(|s| *s == "turn_duration" || *s == "stop_hook_summary")
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core status::`
Expected: `test result: ok. 17 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): fuse transcript turn ends into status (deny path, side agents)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 10: Steer vs queue classification

> **Superseded in part:** commit "fix(core): status engine handles resume/clear, dialog clearing, late Stop, slash commands" supersedes this task's code for the prompt predicate (shared `status::is_prompt`) and `remove` pairing by content; it documents the global-FIFO orphan risk. `src/delivery.rs` is the source of truth.

**Files:**
- Create: `crates/mirante-core/src/delivery.rs`
- Modify: `crates/mirante-core/src/lib.rs`

Port of `spike_kit::delivery::classify_transcript` at `0cfefd1` (absorbed_mid_turn, sidechain filter, FIFO pairing, cancel handling, queued-line confirmation), restructured so the UI can ask per message:
- `analyze` pairs **every** enqueue with its remove/dequeue FIFO (the spike only paired the first one);
- `classify_message(lines, text)` answers "was my message steered or queued?" for the most recent enqueue whose `content` matches;
- new `Delivery::Pending` for "not decided yet" (no remove/dequeue yet, or a dequeue whose queued prompt line is not written yet). On a live, growing transcript the spike's `Never` for those cases was wrong. Two spike tests therefore change expectation: `transcript_enqueued_but_never_removed_is_never` → `enqueued_but_not_yet_taken_is_pending`, and `transcript_remove_before_enqueue_is_ignored` now expects `Pending`.
- Not ported: `classify_hooks` (FINDINGS rule 2 proved hooks cannot tell steered from queued), `classify_stream` (headless fallback only) and `classify_codex` (Codex steering is acknowledged by the protocol; Plan 2d).

- [ ] **Step 1: Register the module and write the failing tests**

`crates/mirante-core/src/lib.rs`:
```rust
//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod delivery;
pub mod error;
pub mod hook;
pub mod model;
pub mod status;
pub mod store;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
```

`crates/mirante-core/src/delivery.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Minimal, anonymized shapes of real Claude Code 2.1.285 transcript lines (Week 1, Task 8).
    fn prompt(text: &str) -> Value {
        json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "typed",
               "message": {"role": "user", "content": text}})
    }
    fn tool_use() -> Value {
        json!({"type": "assistant", "message": {"stop_reason": "tool_use",
               "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash"}]}})
    }
    fn tool_result() -> Value {
        json!({"type": "user", "message": {"role": "user",
               "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "passo-1"}]}})
    }
    fn queue(op: &str) -> Value {
        json!({"type": "queue-operation", "operation": op, "content": "BANANA please"})
    }
    fn enqueue(text: &str) -> Value {
        json!({"type": "queue-operation", "operation": "enqueue", "content": text})
    }
    fn dequeue() -> Value {
        json!({"type": "queue-operation", "operation": "dequeue"})
    }
    fn removed(reason: &str) -> Value {
        json!({"type": "queue-operation", "operation": "remove", "content": "BANANA please",
               "reason": reason, "commandUuid": "c1"})
    }
    fn answer(text: &str) -> Value {
        json!({"type": "assistant", "message": {"stop_reason": "end_turn",
               "content": [{"type": "text", "text": text}]}})
    }
    fn queued_user() -> Value {
        json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued", "message": {}})
    }
    fn turn_end() -> Vec<Value> {
        vec![
            json!({"type": "system", "subtype": "stop_hook_summary"}),
            json!({"type": "system", "subtype": "turn_duration", "durationMs": 10763}),
        ]
    }

    #[test]
    fn remove_before_turn_end_is_steered() {
        let mut t = vec![prompt("run five steps"), tool_use(), tool_result(), queue("enqueue"), tool_use()];
        t.extend([removed("absorbed_mid_turn"), tool_result()]);
        t.push(json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": "BANANA please"}}));
        t.push(answer("BANANA"));
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn headless_sdk_prompt_starts_a_turn() {
        let mut t = vec![queue("enqueue"), dequeue()];
        t.push(json!({"type": "user", "promptSource": "sdk",
                      "message": {"role": "user", "content": [{"type": "text", "text": "run five steps"}]}}));
        t.extend([tool_use(), queue("enqueue"), tool_result()]);
        t.push(json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": "BANANA please"}}));
        t.extend([removed("absorbed_mid_turn"), answer("BANANA")]);
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn dequeue_after_end_turn_is_queued() {
        let t = vec![
            prompt("list 60 fruits"),
            queue("enqueue"),
            answer("1. apple ..."),
            dequeue(),
            json!({"type": "system", "subtype": "stop_hook_summary"}),
            json!({"type": "system", "subtype": "turn_duration"}),
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued",
                   "message": {"role": "user", "content": "BANANA please"}}),
            answer("BANANA"),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn enqueue_while_idle_is_not_mid_run() {
        let t = vec![
            queue("enqueue"),
            dequeue(),
            prompt("say hi"),
            answer("hi"),
            json!({"type": "system", "subtype": "stop_hook_summary"}),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Inconclusive);
    }

    #[test]
    fn remove_after_next_human_prompt_is_queued() {
        let t =
            vec![prompt("one"), queue("enqueue"), answer("done"), prompt("two"), removed("dequeued"), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn tool_result_user_line_does_not_end_the_turn() {
        let t = vec![prompt("one"), tool_use(), queue("enqueue"), tool_result(), removed("absorbed_mid_turn")];
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn enqueued_but_not_yet_taken_is_pending() {
        let mut t = vec![prompt("one"), queue("enqueue"), answer("done")];
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Pending);
    }

    #[test]
    fn dequeued_without_queued_line_yet_is_pending() {
        let t = vec![prompt("one"), queue("enqueue"), answer("done"), dequeue()];
        assert_eq!(classify_transcript(&t), Delivery::Pending);
    }

    #[test]
    fn without_enqueue_is_inconclusive() {
        let mut t = vec![prompt("one"), answer("done")];
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Inconclusive);
    }

    #[test]
    fn remove_before_enqueue_is_ignored() {
        let mut t = vec![removed("dequeued"), prompt("one"), queue("enqueue"), answer("done")];
        t.extend(turn_end());
        assert_eq!(classify_transcript(&t), Delivery::Pending);
    }

    #[test]
    fn sidechain_end_turn_does_not_end_parent_turn() {
        let t = vec![
            prompt("one"),
            tool_use(),
            queue("enqueue"),
            json!({"type": "assistant", "isSidechain": true, "agentId": "a1",
                   "message": {"stop_reason": "end_turn", "content": []}}),
            json!({"type": "system", "subtype": "turn_duration", "parentToolUseID": "toolu_1"}),
            removed("absorbed_mid_turn"),
            answer("BANANA"),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Steered);
    }

    #[test]
    fn sidechain_prompt_and_enqueue_are_ignored() {
        let t = vec![
            json!({"type": "user", "isSidechain": true, "origin": {"kind": "human"}, "message": {}}),
            json!({"type": "queue-operation", "operation": "enqueue", "agentId": "a1"}),
            answer("done"),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Inconclusive);
    }

    #[test]
    fn cancelled_remove_mid_turn_is_never() {
        let t = vec![prompt("one"), tool_use(), queue("enqueue"), removed("cancelled"), answer("done")];
        assert_eq!(classify_transcript(&t), Delivery::Never);
    }

    #[test]
    fn non_absorbed_remove_then_queued_user_is_queued() {
        let t = vec![prompt("one"), queue("enqueue"), dequeue(), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn cancelled_enqueue_does_not_borrow_a_later_queued_line() {
        let mut t = vec![prompt("one"), tool_use(), queue("enqueue"), removed("cancelled"), answer("done")];
        t.extend(turn_end());
        t.extend([prompt("two"), queue("enqueue"), dequeue(), queued_user()]);
        assert_eq!(classify_transcript(&t), Delivery::Never);
        let t = vec![prompt("one"), queue("enqueue"), removed("user_cancelled"), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Never);
    }

    #[test]
    fn queued_line_after_next_in_turn_enqueue_or_prompt_is_never() {
        let t = vec![prompt("one"), tool_use(), queue("enqueue"), dequeue(), queue("enqueue"), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Never);
        let t = vec![prompt("one"), queue("enqueue"), dequeue(), prompt("two"), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Never);
    }

    #[test]
    fn multiple_enqueues_pair_fifo() {
        let t = vec![
            prompt("one"),
            tool_use(),
            queue("enqueue"),
            queue("enqueue"),
            removed("absorbed_mid_turn"),
            answer("x"),
        ];
        assert_eq!(classify_transcript(&t), Delivery::Steered);
        let t = vec![prompt("one"), queue("enqueue"), queue("enqueue"), answer("x"), dequeue(), queued_user()];
        assert_eq!(classify_transcript(&t), Delivery::Queued);
    }

    #[test]
    fn classify_message_finds_each_message_by_text() {
        // "A" is absorbed mid-turn; "B" arrives too late for a tool boundary and is queued.
        let t = vec![
            prompt("run five steps"),
            tool_use(),
            enqueue("A"),
            json!({"type": "queue-operation", "operation": "remove", "content": "A", "reason": "absorbed_mid_turn"}),
            tool_result(),
            enqueue("B"),
            answer("done"),
            dequeue(),
            json!({"type": "system", "subtype": "turn_duration"}),
            json!({"type": "user", "origin": {"kind": "human"}, "promptSource": "queued", "message": {"content": "B"}}),
        ];
        assert_eq!(classify_message(&t, "A"), Delivery::Steered);
        assert_eq!(classify_message(&t, " B "), Delivery::Queued);
        assert_eq!(classify_message(&t, "C"), Delivery::Inconclusive);
    }

    #[test]
    fn classify_message_is_pending_while_waiting_and_inconclusive_when_idle() {
        let live = vec![prompt("one"), tool_use(), enqueue("A")];
        assert_eq!(classify_message(&live, "A"), Delivery::Pending);
        let idle = vec![enqueue("A"), dequeue(), prompt("A")];
        assert_eq!(classify_message(&idle, "A"), Delivery::Inconclusive);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core delivery::`
Expected: compile errors such as `` cannot find function `classify_transcript` in this scope `` and `` cannot find type `Delivery` ``.

- [ ] **Step 3: Implement above the test module**

Insert at the top of `crates/mirante-core/src/delivery.rs`:
```rust
//! Steer vs queue, from the Claude Code transcript (ported from `spike_kit::delivery::
//! classify_transcript`, the version validated in Week 1).
//!
//! Rule, from real 2.1.285 transcripts, by FILE ORDER (timestamps are not monotonic):
//! - A message typed while a turn runs is logged as `queue-operation` `enqueue` with its
//!   `content`.
//! - Steered: a `remove` with `reason: "absorbed_mid_turn"` before that turn ends.
//! - Queued: a `dequeue` (or a non-cancel `remove`), then a `user` line with
//!   `promptSource: "queued"` before the next in-turn enqueue or the next non-queued prompt.
//! - Enqueues pair with remove/dequeue operations first-in, first-out.
//! - A turn starts at a typed prompt (`origin.kind == "human"`) or a headless prompt
//!   (`promptSource == "sdk"`), and ends at the final assistant line
//!   (`stop_reason == "end_turn"`) or a `system` `stop_hook_summary` / `turn_duration` line.
//! - Sidechain lines (`isSidechain`, `agentId`, `parentToolUseID`) are ignored.
//!
//! Change from the spike: when the outcome is not decided yet (no remove/dequeue yet, or a
//! dequeue whose queued prompt line has not been written yet) the result is `Pending`, not
//! `Never`. The UI calls this on a live, growing transcript.
use std::collections::{HashMap, VecDeque};

use serde::Serialize;
use serde_json::Value;

use crate::status::is_sidechain;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// Absorbed by the running turn at a tool boundary (true mid-run steering).
    Steered,
    /// Delivered as a new prompt after the running turn ended.
    Queued,
    /// Enqueued, outcome not written to the transcript yet.
    Pending,
    /// Cancelled, or overtaken by another prompt: never delivered.
    Never,
    /// No matching mid-turn message in the transcript.
    Inconclusive,
}

fn is_prompt(v: &Value) -> bool {
    v["type"] == "user" && (v["origin"]["kind"] == "human" || v["promptSource"] == "sdk")
}

fn is_turn_end(v: &Value) -> bool {
    (v["type"] == "assistant" && v["message"]["stop_reason"] == "end_turn")
        || (v["type"] == "system" && (v["subtype"] == "stop_hook_summary" || v["subtype"] == "turn_duration"))
}

fn is_op(v: &Value, ops: &[&str]) -> bool {
    v["type"] == "queue-operation" && v["operation"].as_str().is_some_and(|o| ops.contains(&o))
}

/// Main-chain lines plus, for each enqueue, whether it happened inside a running turn and
/// which remove/dequeue took it (FIFO).
struct Analysis<'a> {
    lines: Vec<&'a Value>,
    enqueues: Vec<(usize, bool)>,
    taken_by: HashMap<usize, usize>,
}

fn analyze(all: &[Value]) -> Analysis<'_> {
    let lines: Vec<&Value> = all.iter().filter(|v| !is_sidechain(v)).collect();
    let mut enqueues = Vec::new();
    let mut taken_by = HashMap::new();
    let mut fifo = VecDeque::new();
    let mut in_turn = false;
    for (i, v) in lines.iter().enumerate() {
        if is_prompt(v) {
            in_turn = true;
        } else if is_turn_end(v) {
            in_turn = false;
        } else if is_op(v, &["enqueue"]) {
            enqueues.push((i, in_turn));
            fifo.push_back(i);
        } else if is_op(v, &["remove", "dequeue"]) {
            if let Some(e) = fifo.pop_front() {
                taken_by.insert(e, i);
            }
        }
    }
    Analysis { lines, enqueues, taken_by }
}

fn classify_enqueue(a: &Analysis<'_>, enqueue: usize) -> Delivery {
    let lines = &a.lines;
    let Some(&taken) = a.taken_by.get(&enqueue) else {
        return Delivery::Pending;
    };
    let end = lines[enqueue + 1..].iter().position(|v| is_turn_end(v) || is_prompt(v)).map(|p| p + enqueue + 1);
    let before_end = end.is_none_or(|e| taken < e);
    let op = lines[taken];
    if before_end && op["reason"] == "absorbed_mid_turn" {
        return Delivery::Steered;
    }
    if op["operation"] == "remove" && op["reason"].as_str().is_some_and(|r| r.contains("cancel")) {
        return Delivery::Never;
    }
    // A queued user line must follow before the next in-turn enqueue or the next non-queued prompt.
    let mut in_turn = before_end;
    for v in &lines[taken + 1..] {
        if v["type"] == "user" && v["promptSource"] == "queued" {
            return Delivery::Queued;
        }
        if is_prompt(v) {
            return Delivery::Never;
        }
        if is_turn_end(v) {
            in_turn = false;
        } else if in_turn && is_op(v, &["enqueue"]) {
            return Delivery::Never;
        }
    }
    Delivery::Pending
}

/// Outcome of the first message enqueued while a turn was running.
pub fn classify_transcript(lines: &[Value]) -> Delivery {
    let a = analyze(lines);
    match a.enqueues.iter().find(|(_, in_turn)| *in_turn) {
        Some(&(i, _)) => classify_enqueue(&a, i),
        None => Delivery::Inconclusive,
    }
}

/// "Was my message steered or queued?" — outcome of the most recent message whose enqueued
/// text equals `text` (trimmed). `Inconclusive` when no such message was enqueued mid-turn.
pub fn classify_message(lines: &[Value], text: &str) -> Delivery {
    let a = analyze(lines);
    let want = text.trim();
    let found =
        a.enqueues.iter().rev().find(|(i, _)| a.lines[*i]["content"].as_str().is_some_and(|c| c.trim() == want));
    match found {
        Some(&(i, true)) => classify_enqueue(&a, i),
        _ => Delivery::Inconclusive,
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core delivery::`
Expected: `test result: ok. 19 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): per-message steer/queue classification from the transcript

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 11: Incremental transcript tailer

> **Superseded in part:** the commit titled `fix(core): harden store constraints, worktree argv, launch quoting, tailer memory` supersedes the code below for the tailer (1 MiB chunked reads, 64 MiB cap on unterminated lines, byte-level JSON parsing). Prefer the code in `crates/mirante-core` over the snippets here.


**Files:**
- Create: `crates/mirante-core/src/transcript_tail.rs`
- Modify: `crates/mirante-core/src/lib.rs`

Reads only bytes appended since the last poll. A partial last line stays buffered as bytes (so a multi-byte UTF-8 character split across writes is safe) until its newline arrives. `offset()` always points just after the last complete line; the daemon persists it with `Store::set_transcript` and passes it back to `TranscriptTail::new` after a restart. Invalid complete lines are counted in `skipped`, never silently dropped. A file that shrinks below the offset is re-read from 0 and flagged `reset`.

- [ ] **Step 1: Register the module and write the failing tests**

`crates/mirante-core/src/lib.rs`:
```rust
//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod delivery;
pub mod error;
pub mod hook;
pub mod model;
pub mod status;
pub mod store;
pub mod transcript_tail;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
```

`crates/mirante-core/src/transcript_tail.rs`:
```rust
#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::*;
    use crate::test_support::TempDir;

    fn append(path: &Path, text: &str) {
        let mut f = OpenOptions::new().create(true).append(true).open(path).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn missing_file_is_an_empty_batch() {
        let dir = TempDir::new("tail-missing");
        let mut t = TranscriptTail::new(&dir.path().join("none.jsonl"), 0);
        assert_eq!(t.poll().unwrap(), TailBatch::default());
    }

    #[test]
    fn returns_only_new_lines_on_each_poll() {
        let dir = TempDir::new("tail-new");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\n{\"n\":2}\n");
        let mut t = TranscriptTail::new(&path, 0);
        let b = t.poll().unwrap();
        assert_eq!(b.lines.iter().map(|v| v["n"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(t.poll().unwrap().lines.len(), 0);
        append(&path, "{\"n\":3}\n");
        assert_eq!(t.poll().unwrap().lines[0]["n"], 3);
        assert_eq!(t.offset(), std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn partial_last_line_waits_for_its_newline() {
        let dir = TempDir::new("tail-partial");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\n{\"n\":");
        let mut t = TranscriptTail::new(&path, 0);
        let b = t.poll().unwrap();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.skipped, 0);
        assert_eq!(t.offset(), 8);
        append(&path, "2,\"s\":\"ã\"}");
        assert!(t.poll().unwrap().lines.is_empty());
        append(&path, "\n");
        let b = t.poll().unwrap();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.lines[0]["s"], "ã");
    }

    #[test]
    fn resumes_from_a_persisted_offset() {
        let dir = TempDir::new("tail-resume");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\n{\"n\":2}\n{\"n\":");
        let mut first = TranscriptTail::new(&path, 0);
        first.poll().unwrap();
        let saved = first.offset();
        append(&path, "3}\n");
        let mut second = TranscriptTail::new(&path, saved);
        let b = second.poll().unwrap();
        assert_eq!(b.lines.len(), 1);
        assert_eq!(b.lines[0]["n"], 3);
    }

    #[test]
    fn invalid_lines_are_counted_not_hidden() {
        let dir = TempDir::new("tail-invalid");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\nnot json\n\n{\"n\":2}\n");
        let b = TranscriptTail::new(&path, 0).poll().unwrap();
        assert_eq!(b.lines.len(), 2);
        assert_eq!(b.skipped, 1);
    }

    #[test]
    fn truncated_file_is_reread_from_start() {
        let dir = TempDir::new("tail-trunc");
        let path = dir.path().join("t.jsonl");
        append(&path, "{\"n\":1}\n{\"n\":2}\n");
        let mut t = TranscriptTail::new(&path, 0);
        t.poll().unwrap();
        std::fs::write(&path, "{\"n\":9}\n").unwrap();
        let b = t.poll().unwrap();
        assert!(b.reset);
        assert_eq!(b.lines[0]["n"], 9);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core transcript_tail::`
Expected: compile errors such as `` cannot find type `TranscriptTail` in this scope ``.

- [ ] **Step 3: Implement above the test module**

Insert at the top of `crates/mirante-core/src/transcript_tail.rs`:
```rust
//! Incremental reader for an append-only JSONL transcript. Each `poll` returns the complete
//! lines appended since the last one; a partial last line stays buffered until its newline
//! arrives. `offset()` is the byte position after the last complete line: persist it and
//! pass it back to `TranscriptTail::new` to resume after a restart.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::Result;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TailBatch {
    pub lines: Vec<Value>,
    /// Complete, non-blank lines that were not valid JSON (counted, never silently dropped).
    pub skipped: usize,
    /// True when the file shrank below the saved offset and was re-read from the start.
    pub reset: bool,
}

#[derive(Debug, Clone)]
pub struct TranscriptTail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl TranscriptTail {
    pub fn new(path: &Path, offset: u64) -> Self {
        TranscriptTail { path: path.to_path_buf(), offset, partial: Vec::new() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads what was appended since the last poll. A missing file yields an empty batch
    /// (Claude creates the transcript on the first prompt).
    pub fn poll(&mut self) -> Result<TailBatch> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TailBatch::default()),
            Err(e) => return Err(e.into()),
        };
        let mut batch = TailBatch::default();
        let len = file.metadata()?.len();
        let read_from = self.offset + self.partial.len() as u64;
        if len < read_from {
            self.offset = 0;
            self.partial.clear();
            batch.reset = true;
        }
        file.seek(SeekFrom::Start(self.offset + self.partial.len() as u64))?;
        let mut fresh = Vec::new();
        file.read_to_end(&mut fresh)?;
        self.partial.extend_from_slice(&fresh);

        let Some(last_newline) = self.partial.iter().rposition(|b| *b == b'\n') else {
            return Ok(batch);
        };
        let complete: Vec<u8> = self.partial.drain(..=last_newline).collect();
        self.offset += complete.len() as u64;
        for line in complete.split(|b| *b == b'\n') {
            let text = String::from_utf8_lossy(line);
            if text.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => batch.lines.push(v),
                Err(_) => batch.skipped += 1,
            }
        }
        Ok(batch)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core transcript_tail::`
Expected: `test result: ok. 6 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): byte-offset transcript tailer with partial-line buffering

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 12: Worktree manager

> **Superseded in part:** the commit titled `fix(core): harden store constraints, worktree argv, launch quoting, tailer memory` supersedes the code below for the worktree manager (`--end-of-options`, base validation, root-confined remove, detached-HEAD and HOME errors). Prefer the code in `crates/mirante-core` over the snippets here.


**Files:**
- Create: `crates/mirante-core/src/worktree.rs`
- Modify: `crates/mirante-core/src/lib.rs`

Spec §2.1 and §4: the user picks the branch name (no prefix is added), the base is the repo's **current** branch unless one is given (never an assumed `main`; the test repo's only branch is `trunk`), the path is `<root>/<repo-name>/<branch>` with `root` defaulting to `~/.mirante/worktrees` and injectable for tests. An existing branch is checked out as is. Removing a worktree keeps the branch and refuses a dirty worktree (no `--force`), so uncommitted agent work is never discarded silently. Names are validated by `git check-ref-format --branch` plus explicit rejection of `-x`, `HEAD` and `@{...}`.

- [ ] **Step 1: Register the module and write the failing tests**

`crates/mirante-core/src/lib.rs`:
```rust
//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod delivery;
pub mod error;
pub mod hook;
pub mod model;
pub mod status;
pub mod store;
pub mod transcript_tail;
pub mod worktree;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
```

`crates/mirante-core/src/worktree.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// A repo whose only branch is `trunk` (proves nothing assumes `main`).
    fn repo(dir: &TempDir) -> PathBuf {
        let repo = dir.path().join("my-app");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "trunk"]);
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        repo
    }

    #[test]
    fn validates_branch_names() {
        for ok in ["feat-login", "pedro/fix-123", "v2.1"] {
            validate_branch_name(ok).unwrap();
        }
        for bad in ["", " ", "-x", "HEAD", "@{-1}", "a..b", "has space", "ends.lock", "x~1", "a:b"] {
            assert!(matches!(validate_branch_name(bad), Err(CoreError::InvalidBranch { .. })), "{bad:?}");
        }
    }

    #[test]
    fn creates_from_current_branch_without_assuming_main() {
        let dir = TempDir::new("wt-create");
        let repo = repo(&dir);
        assert_eq!(current_branch(&repo).unwrap(), "trunk");
        let mgr = WorktreeManager::new(&dir.path().join("worktrees"));
        let wt = mgr.create(&repo, "pedro/feature-x", None).unwrap();
        assert_eq!(wt.path, dir.path().join("worktrees/my-app/pedro/feature-x"));
        assert!(wt.path.join("README.md").is_file());
        assert_eq!(current_branch(&wt.path).unwrap(), "pedro/feature-x");
        assert_eq!(wt.head, git(&repo, &["rev-parse", "trunk"]).unwrap());
        assert_eq!(repo_name(&wt.path).unwrap(), "my-app");
    }

    #[test]
    fn explicit_base_and_existing_branch() {
        let dir = TempDir::new("wt-base");
        let repo = repo(&dir);
        git(&repo, &["branch", "release"]).unwrap();
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        mgr.create(&repo, "from-release", Some("release")).unwrap();
        let existing = mgr.create(&repo, "release", None).unwrap();
        assert_eq!(existing.branch.as_deref(), Some("release"));
        assert!(mgr.create(&repo, "nope", Some("no-such-base")).is_err());
    }

    #[test]
    fn rejects_invalid_names_and_taken_paths() {
        let dir = TempDir::new("wt-reject");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        assert!(matches!(mgr.create(&repo, "bad name", None), Err(CoreError::InvalidBranch { .. })));
        mgr.create(&repo, "one", None).unwrap();
        assert!(mgr.create(&repo, "one", None).is_err());
    }

    #[test]
    fn remove_keeps_the_branch_and_list_shows_only_managed_worktrees() {
        let dir = TempDir::new("wt-remove");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let a = mgr.create(&repo, "a", None).unwrap();
        mgr.create(&repo, "b", None).unwrap();
        let listed: Vec<Option<String>> = mgr.list(&repo).unwrap().into_iter().map(|w| w.branch).collect();
        assert_eq!(listed, vec![Some("a".to_string()), Some("b".to_string())]);
        mgr.remove(&repo, &a.path).unwrap();
        assert!(!a.path.exists());
        assert!(branch_exists(&repo, "a").unwrap());
        assert_eq!(mgr.list(&repo).unwrap().len(), 1);
    }

    #[test]
    fn remove_refuses_a_dirty_worktree() {
        let dir = TempDir::new("wt-dirty");
        let repo = repo(&dir);
        let mgr = WorktreeManager::new(&dir.path().join("wt"));
        let a = mgr.create(&repo, "a", None).unwrap();
        std::fs::write(a.path.join("new.txt"), "work\n").unwrap();
        assert!(matches!(mgr.remove(&repo, &a.path), Err(CoreError::Git { .. })));
        assert!(a.path.exists());
    }

    #[test]
    fn parses_porcelain_including_detached() {
        let text = "worktree /r\nHEAD aaa\nbranch refs/heads/trunk\n\nworktree /w/x\nHEAD bbb\ndetached\n";
        let w = parse_porcelain(text);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].branch.as_deref(), Some("trunk"));
        assert_eq!(w[1], Worktree { path: "/w/x".into(), branch: None, head: "bbb".into() });
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core worktree::`
Expected: compile errors such as `` cannot find function `validate_branch_name` in this scope `` and `` cannot find type `WorktreeManager` ``.

- [ ] **Step 3: Implement above the test module**

Insert at the top of `crates/mirante-core/src/worktree.rs`:
```rust
//! Git worktrees per task, via the `git` CLI. Layout: `<root>/<repo-name>/<branch>`, with
//! `root` = `~/.mirante/worktrees` by default. The branch name is the user's choice (no
//! forced prefix), and the base is the repo's current branch unless one is given: never
//! an assumed `main`. Removing a worktree keeps its branch.
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CoreError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// `None` for a detached HEAD.
    pub branch: Option<String>,
    pub head: String,
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        return Err(CoreError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn invalid(name: &str, reason: &str) -> CoreError {
    CoreError::InvalidBranch { name: name.to_string(), reason: reason.to_string() }
}

/// Accepts any name git accepts as a branch (`git check-ref-format --branch`), except
/// names that are not literal branch names: leading `-`, `HEAD`, and `@{...}` shorthands.
pub fn validate_branch_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(invalid(name, "empty"));
    }
    if name.starts_with('-') {
        return Err(invalid(name, "starts with '-'"));
    }
    if name == "HEAD" || name.contains("@{") {
        return Err(invalid(name, "not a literal branch name"));
    }
    let ok = Command::new("git").args(["check-ref-format", "--branch", name]).output()?.status.success();
    if !ok {
        return Err(invalid(name, "rejected by git check-ref-format"));
    }
    Ok(())
}

/// The branch checked out in `repo` (fails on a detached HEAD).
pub fn current_branch(repo: &Path) -> Result<String> {
    git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
}

/// Directory name of the repository's main worktree (works from any of its worktrees).
pub fn repo_name(repo: &Path) -> Result<String> {
    let common = git(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
    let common = PathBuf::from(common);
    let top = if common.file_name().is_some_and(|n| n == ".git") {
        common.parent().unwrap_or(&common).to_path_buf()
    } else {
        common
    };
    top.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| CoreError::Git {
        args: "rev-parse --git-common-dir".into(),
        stderr: "no repository name".into(),
    })
}

fn branch_exists(repo: &Path, branch: &str) -> Result<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .output()?;
    Ok(out.status.success())
}

pub fn parse_porcelain(text: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    for block in text.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let mut path = None;
        let mut head = String::new();
        let mut branch = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                head = h.to_string();
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            }
        }
        if let Some(path) = path {
            out.push(Worktree { path, branch, head });
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct WorktreeManager {
    root: PathBuf,
}

impl WorktreeManager {
    pub fn new(root: &Path) -> Self {
        WorktreeManager { root: root.to_path_buf() }
    }

    /// `~/.mirante/worktrees`.
    pub fn default_root() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".mirante/worktrees")
    }

    pub fn path_for(&self, repo: &Path, branch: &str) -> Result<PathBuf> {
        Ok(self.root.join(repo_name(repo)?).join(branch))
    }

    /// Creates `<root>/<repo>/<branch>`. An existing branch is checked out as is; a new one
    /// starts from `base`, or from the repo's current branch when `base` is `None`.
    pub fn create(&self, repo: &Path, branch: &str, base: Option<&str>) -> Result<Worktree> {
        validate_branch_name(branch)?;
        let path = self.path_for(repo, branch)?;
        if path.exists() {
            return Err(CoreError::Git {
                args: "worktree add".into(),
                stderr: format!("{} already exists", path.display()),
            });
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path_str = path.to_string_lossy().into_owned();
        if branch_exists(repo, branch)? {
            git(repo, &["worktree", "add", &path_str, branch])?;
        } else {
            let base = match base {
                Some(b) => b.to_string(),
                None => current_branch(repo)?,
            };
            git(repo, &["worktree", "add", "-b", branch, &path_str, &base])?;
        }
        let head = git(&path, &["rev-parse", "HEAD"])?;
        Ok(Worktree { path, branch: Some(branch.to_string()), head })
    }

    /// Removes the worktree directory and its git metadata. The branch is kept. Refuses a
    /// dirty worktree (git's own check) so uncommitted work is never discarded silently.
    pub fn remove(&self, repo: &Path, path: &Path) -> Result<()> {
        git(repo, &["worktree", "remove", &path.to_string_lossy()])?;
        Ok(())
    }

    /// Worktrees of `repo` that live under this manager's root.
    pub fn list(&self, repo: &Path) -> Result<Vec<Worktree>> {
        let root = std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        let all = parse_porcelain(&git(repo, &["worktree", "list", "--porcelain"])?);
        Ok(all
            .into_iter()
            .filter(|w| {
                let p = std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone());
                p.starts_with(&root)
            })
            .collect())
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core worktree::`
Expected: `test result: ok. 7 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): git worktree manager with user-chosen branches and detected base

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 13: Claude launch config, then full verification

> **Superseded in part:** the commit titled `fix(core): harden store constraints, worktree argv, launch quoting, tailer memory` supersedes the code below for launch config (shell-quoted hook command, canonicalized shim check, documented env scrubbing). Prefer the code in `crates/mirante-core` over the snippets here.


**Files:**
- Create: `crates/mirante-core/src/claude_launch.rs`
- Modify: `crates/mirante-core/src/lib.rs`

Pure data (FINDINGS rule 4), nothing is spawned:
- `resolve_claude_bin`: `MIRANTE_CLAUDE_BIN` wins (must be an executable file); otherwise the first executable `claude` on `PATH` outside known shim dirs (`.cmuxterm/cmux-cli-shims`). Takes its inputs as arguments so tests never touch the real environment; `resolve_claude_bin_from_env` is the thin wrapper.
- `env_to_scrub`: the 13 nested-session names from `spike_kit::util::NESTED_SESSION_ENV`, plus present variables with the `CLAUDE_CODE_MESSAGING_`, `CMUX_CLAUDE_` or `CMUX_AGENT_` prefixes. Auth variables are never named.
- `settings_json`: from `spike_kit::hook::settings_json` (every event → the hook command; tool events with matcher `"*"`; the allowlist passed in). No `defaultMode`: permission mode is the `--permission-mode manual` argument.
- `launch_spec`: `--settings <file> --permission-mode manual [--resume <id>]`, cwd = worktree, `MIRANTE_TASK_ID=<task>` set (scopes the hook to Mirante sessions), scrub list attached. Never `-p`, `--bare` or any skip-permissions flag.

- [ ] **Step 1: Register the module and write the failing tests**

`crates/mirante-core/src/lib.rs` (final):
```rust
//! Mirante core: domain model, SQLite store, status fusion, steer/queue classification,
//! transcript tailing, git worktrees and the Claude launch config. Pure library: no PTY,
//! no daemon, no UI.
pub mod claude_launch;
pub mod delivery;
pub mod error;
pub mod hook;
pub mod model;
pub mod status;
pub mod store;
pub mod transcript_tail;
pub mod worktree;

pub use error::{CoreError, Result};

#[cfg(test)]
pub(crate) mod test_support;
```

`crates/mirante-core/src/claude_launch.rs`:
```rust
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-core claude_launch::`
Expected: compile errors such as `` cannot find function `resolve_claude_bin` in this scope ``.

- [ ] **Step 3: Implement above the test module**

Insert at the top of `crates/mirante-core/src/claude_launch.rs`:
```rust
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
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-core claude_launch::`
Expected: `test result: ok. 5 passed`.

- [ ] **Step 5: Full workspace verification**

Run: `cargo test --workspace 2>&1 | grep "test result" | head -2`
Expected: `test result: ok. 81 passed` (mirante-core) and `test result: ok. 61 passed` (spike-kit).

Run: `cargo clippy -p mirante-core --all-targets -- -D warnings`
Expected: `Finished` with no warnings.

Run: `git status --short crates/spike-kit`
Expected: no output (spike-kit untouched).

- [ ] **Step 6: Commit**

```bash
git add crates/mirante-core
git commit -F - <<'EOF'
feat(core): claude launch config (real binary, env scrub, scoped settings)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

## Self-review

### Scope and FINDINGS coverage

| Requirement (source) | Where | Test(s) |
|---|---|---|
| New lib crate in the workspace; allowed deps only; spike-kit untouched (brief §1) | Task 1, Task 13 Step 5 | `temp_dir_is_created_and_removed`; workspace run shows 61 spike-kit tests |
| Task / Conversation / Session / Turn / Event(seq, boot_id) / PermissionRequest / Status (brief §2, spec §4 durable model) | Task 2 (structs), Tasks 4–7 (tables) | `model::tests::*`, store tests |
| Explicit transition validation (spec §8 "add the transition validation the old code lacked") | Task 2 (`can_transition_to`), Task 5 (`transition_lifecycle`, compare-and-set `set_status`), Task 7 (permission answered once) | `status_transitions`, `lifecycle_transitions`, `lifecycle_transitions_are_validated_and_audited`, `set_status_is_compare_and_set_and_validated`, `pending_request_survives_restart_and_is_answered_once` |
| Versioned embedded migrations, WAL (brief §3) | Task 4 | `migrates_to_latest_version_once`, `file_database_uses_wal_and_foreign_keys` |
| Append-only events with monotonic seq + boot_id (spec §4) | Task 4 (schema, triggers), Task 5 | `events_are_monotonic_across_reopen_and_carry_boot_id`, `events_table_is_append_only`, `each_opening_gets_a_new_boot_id` |
| Persist on every transition (spec §4) | Tasks 5–7: state change + event in one transaction | `turns_are_numbered_and_closed_by_end_session` (event kinds), `lifecycle_transitions_are_validated_and_audited` |
| Per-task lock, no orphaned lock (spec §8) | Task 6 | `per_task_lock_allows_one_live_session`, `unique_index_backs_the_lock_even_without_the_check`, `turns_are_numbered_and_closed_by_end_session` |
| Native session id stored for resume (FINDINGS "Resume") | Task 6 | `native_session_id_survives_reopen` |
| Permission requests persisted, never auto-approved, answered once (spec §2.5, §5) | Task 7 | `pending_request_survives_restart_and_is_answered_once`, `pending_is_not_an_answer` |
| Idle Notification → Idle; PermissionRequest / other Notification → NeedsYou (FINDINGS rule 3) | Task 8 | `idle_notification_is_idle_other_notifications_need_you`, `allowed_permission_goes_needs_you_then_working` |
| Side-agent filtering by `agent_id`/`agent_type` (FINDINGS rule 1) | Task 3 (fields), Task 8 | `parses_payload_with_agent_identity_and_ignores_unknown_fields`, `side_agent_stop_and_prompt_do_not_move_turn_boundaries`, `side_agent_tool_use_does_not_clear_main_agent_dialog`, `side_agent_permission_after_turn_needs_you_until_it_proceeds` |
| S3 failure class 1: deny path, no hook fires (FINDINGS S3, rule 1) | Task 9 | `deny_path_is_closed_by_transcript_turn_duration` |
| S3 failure class 2: stray PreToolUse after Stop (FINDINGS S3, rule 1) | Task 8 (gating) + Task 9 (pinned) | `stray_pre_tool_use_after_stop_stays_idle` (with and without `agent_id`) |
| Transcript `turn_duration`/`stop_hook_summary` authoritative; isSidechain ignored; arrival / file order (FINDINGS rule 1) | Task 9 | `sidechain_transcript_lines_are_ignored`, `late_transcript_end_of_previous_turn_does_not_end_the_new_turn`, `transcript_prompt_before_hook_opens_the_turn_once`, `hook_only_turn_does_not_desync_later_transcript_ends`, `queued_prompt_line_opens_a_turn` |
| Steering confirmed from the transcript, file order, absorbed_mid_turn / dequeue + queued, FIFO, cancel (FINDINGS rule 2) | Task 10 | all `delivery::tests::*` (17 ported + 2 per-message) |
| Per-message "steered or queued?" for the UI (brief §5, spec §2.4) | Task 10 | `classify_message_finds_each_message_by_text`, `classify_message_is_pending_while_waiting_and_inconclusive_when_idle` |
| Incremental tail by byte offset, partial last line buffered, offset persisted (brief §6) | Task 11 (+ Task 6 `set_transcript`) | `partial_last_line_waits_for_its_newline`, `resumes_from_a_persisted_offset`, `transcript_offset_persists` |
| No silent parse failures (spec §8) | Task 3, Task 11 | `invalid_payload_is_an_error_not_an_empty_event`, `invalid_lines_are_counted_not_hidden` |
| Worktree per task, user branch name, no prefix, never assume `main`, `~/.mirante/worktrees/<repo>/<branch>`, configurable root, remove keeps branch, list (spec §2.1, §4; brief §7) | Task 12 | `creates_from_current_branch_without_assuming_main`, `explicit_base_and_existing_branch`, `validates_branch_names`, `remove_keeps_the_branch_and_list_shows_only_managed_worktrees`, `remove_refuses_a_dirty_worktree` |
| Real claude binary, skip shims, `MIRANTE_CLAUDE_BIN` (FINDINGS rule 4) | Task 13 | `skips_shim_dirs_and_non_executables`, `override_wins_but_must_be_executable` |
| Env scrub, never auth vars (FINDINGS rule 4) | Task 13 | `scrubs_nested_and_wrapper_vars_but_never_auth` |
| `--settings` (additive), `--permission-mode manual`, `--resume <id>`, hooks → mirante-hook, allowlist passed in, scoped by `MIRANTE_TASK_ID` (FINDINGS rule 4, spec §4 hooks install, §5) | Task 13 | `settings_register_every_event_with_the_hook_command`, `launch_spec_for_new_and_resumed_sessions` |

Deliberately not covered here (owned by later plans, see "Out of scope"): trust dialog (rule 5) and answering dialogs in the PTY (rule 6) need the PTY; quota (rule 7) is open; Codex rules 8–9 belong to the Codex driver.

### Placeholder scan

No "TBD", "TODO", "add error handling" or "similar to Task N". Every code step shows complete code; every run step has an exact command and expected output.

### Type and name consistency

`Status`, `Lifecycle`, `AgentKind`, `PermissionStatus`, `Task`, `NewTask`, `Conversation`, `Session`, `Turn`, `Event`, `PermissionRequest` (Task 2) are the only domain types used by `store` (Tasks 4–7). `HookEvent` (Task 3) is the input of `status::Input::Hook` (Task 8). `status::is_sidechain` (Task 9) is reused by `delivery` (Task 10). `Store::set_transcript(session_id, path, offset)` (Task 6) persists `TranscriptTail::offset()` (Task 11). `Conversation::native_session_id` (Task 2/6) feeds `LaunchInput::resume` (Task 13). `CoreError` variants used by later tasks (`TaskBusy`, `StaleStatus`, `InvalidBranch`, `Git`, `ClaudeNotFound`) are all defined in Task 1.

### Known risks to verify in Plan 2b (with real sessions)

- Whether the stray post-`Stop` `PreToolUse` carries `agent_id` is unknown (the spike did not log it). The turn gating makes the result correct either way; Plan 2b should log raw payloads once to confirm.
- A prompt that fires `UserPromptSubmit` but never `Stop` (e.g. some slash commands) would keep the status `Working`. Check `/effort`, `/clear` and `/compact` in Plan 2b's S3 re-run.
- The deny-path fixture follows the shape described in `raw/s3-status-check.md` (interrupted user line, `turn_duration`, no `stop_hook_summary`); re-run the 51-checkpoint S3 script against the fused engine in Plan 2b (target: 0 wrong statuses in 50).
- On resume, Claude appends to the same transcript file. Plan 2b must start the new session's `TranscriptTail` at the file's current length (or the persisted offset) so old turns are not replayed into a fresh `StatusEngine`.
