# Plan 3 — Mirante cockpit (Tauri desktop app) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A macOS desktop cockpit that dispatches Claude Code / Codex agents, shows every task live and lets the user answer, steer and review them — one screen in three modes: V3 group-chat home, V1 classic detail, V2 agent grid.

**Architecture:** The daemon stays the only owner of state. A new Rust crate, `mirante-bridge`, is the cockpit's client of the daemon socket (allowlisted RPC, a resumable `events.subscribe` stream, `attach` by id), tested against a scripted fake daemon; the Tauri 2 shell in `apps/cockpit/src-tauri` only forwards six commands to it. The React frontend keeps one event-sourced store: a full snapshot, then events applied by `seq`, with every touched task re-read via `task.get` (the stream is a hint, the daemon the authority), reconnecting with `since_seq` + `boot_id`.

**Tech Stack:** Rust 2021 (std threads, `mirante-ipc`), Tauri 2.12 (`tauri-plugin-notification`, `tauri-plugin-opener`), React 19, Vite 8, TypeScript 7, xterm.js 6 (`@xterm/addon-fit`), Vitest 5 + Testing Library + jsdom 29, pnpm 10, Node ≥ 22.12.

---

## Before you start

Read: `docs/spec/2026-11-07-cockpit-prototype-design.md` (scope: dispatch, see every task, open a task, talk mid-run, permissions, context that survives; §5 security), `docs/spikes/FINDINGS.md` (steer vs queue semantics, idle Notification ≠ needs you, trust dialog never auto-accepted), and the daemon protocol at HEAD: `crates/mirante-ipc/src/types.rs` and `crates/mirante-daemon/src/server.rs`.

Written against `main` at `84b90fa` (Plan 2b complete: Claude PTY and Codex app-server drivers, memory guard wired). Every code block below was built and tested in a scratch clone of that commit, task by task, in order (see "Verification record" at the end).

All commands run from the repository root unless a step says otherwise. macOS is the target (Linux compiles the bridge but the app is not packaged for it here).

### Daemon changes this plan makes (Tasks 1–3, first)

The cockpit needs three things the daemon does not offer at HEAD. Nothing else on the daemon changes.

1. **`task.events`** (Task 1): a task's audit trail, paged backwards — to rebuild each card's step feed after an app or daemon restart (the live stream only replays ~recent events).
2. **Claude `step` events** (Task 2): Codex emits steps from its driver; Claude emits none, so the feed and Steps tab would be empty for Claude. The hub turns each `PostToolUse` / `PostToolUseFailure` hook into `{type: "tool", tool, summary, ok, agent}`.
3. **`task.diff`** (Task 3): the branch's changes against its base, with GitHub's compare URL and a local "pushed" flag — for the done card, the Diff tab and **Open PR**.

Plus one small CLI refactor (Task 4): `daemon_ctl::start_daemon_with(paths, bin)`, so the app can start the daemon from a binary it located itself.

### Already at HEAD and used as-is (from Plan 2b)

- Memory guard: `TaskView.memory` (`{bytes, peak, budget, state}`), `TaskView.hibernated`, `daemon.status.memory` (`SystemView`: `level`, `total`, `free`, `swap_used`, `swap_total`, `paused`), `session.hibernate` / `session.wake`, events `memory.warn|paused|resumed|offer_hibernate|leak|level|refused|hibernated|auto_hibernated`. All names the UI reads live in `apps/cockpit/src/lib/memguard.ts`; against an older daemon the panel says "not available" and the buttons report METHOD_NOT_FOUND.
- Codex "auto-edits" mode (`approvalPolicy: on-request`, `sandbox: workspace-write`): the UI labels every Codex task **auto-edits**.
- `grantRoot` approvals: the Codex driver names them `file_change (session-wide grant)`; the UI shows a **session-wide grant** tag on that card.
- `TaskView.usage` (Codex quota line) is shown as a muted tag.

### Wire contract the cockpit uses

| Need | Daemon call / event |
|---|---|
| Initial state, live updates, reconnect | `events.subscribe {since_seq?, boot_id?}` → `Snapshot {boot_id, seq, tasks, resumed}` then `event` notifications |
| Authoritative row after an event | `task.get {task}`; after a resumed snapshot `task.list` |
| Card history after restart | `task.events {task, limit: 200}` (Task 1) |
| Dispatch | `task.create {repo, agent, branch, goal}` then `session.start {task, prompt: goal}` |
| Trust dialog | `session.trust {task, accept}` |
| Permission cards | `permission.list`, `permission.answer {id, allow}` (once; CONFLICT = already answered) |
| Talk mid-run | `message.send {task, text, mode: steer|queue}`; UNSUPPORTED → `session.interrupt` then queue |
| Terminal | `attach {task, rows, cols}` (raw bytes after the response), `session.resize` |
| Session actions | `session.interrupt`, `session.stop`, `session.resume`, `session.hibernate`, `session.wake` |
| Done card / Diff tab | `task.diff {task}` (Task 3) |
| Banner, memory panel | `daemon.status` (polled every 5 s while connected) |

The bridge refuses everything else (`hook.event`, `daemon.shutdown`, `daemon.kill_orphan`, unknown methods).

### The screen (founder's design)

- **V3 home (default):** left rail = projects/repos (needs-you badge, add by path) + agents; center = the selected project's feed: one live card per task (dispatch line, labels, last steps, permission card with **Allow once** / **Deny**, trust card, done card with diff summary + **Open PR**), dispatch box `@claude <repo>/<branch> goal` / `@codex …`; right rail = board: **Needs you / Working / Ready for review**.
- **V1 detail:** click a card → tasks grouped by state on the left; the task with tabs **Steps / Terminal / Diff / Memory** (Memory is a placeholder until the memory layer) and a composer with **Steer now** / **Queue**.
- **V2 grid:** ⌘G toggles 2–6 live tiles (last lines, status, inline allow/deny, mini input). ⌘K command bar everywhere.
- Notifications: native, when a task starts needing the user, suppressed when that task is on screen in the focused window.

## File structure

```
crates/mirante-ipc/src/types.rs          (modify) TASK_EVENTS / TASK_DIFF, TaskEventsParams, TaskDiff, DiffFile
crates/mirante-daemon/src/diff.rs        (new)    read-only git diff vs fork point, compare URL
crates/mirante-daemon/src/hub/mod.rs     (modify) Hub::task_events, Hub::task_diff
crates/mirante-daemon/src/hub/session.rs (modify) tool_step: Claude hooks -> step events
crates/mirante-daemon/src/server.rs      (modify) dispatch arms
crates/mirante-cli/src/daemon_ctl.rs     (modify) start_daemon_with
crates/mirante-bridge/                   (new crate, workspace member)
  src/lib.rs        module map
  src/rpc.rs        allowlist, BridgeError {disconnected|rpc|protocol|forbidden}, call()
  src/stream.rs     subscribe thread, backoff, resume, StreamMsg {connected|event|disconnected}
  src/attach.rs     Attachments: open/write/close by id
  src/daemon.rs     find + start mirante-daemon
  src/fake.rs       FakeDaemon (tests + feature "fake")
  src/bin/mirante-fake-daemon.rs  demo tasks for UI work and the smoke test
apps/cockpit/                            (new; own pnpm package)
  package.json tsconfig.json vite.config.ts index.html .gitignore README.md app-icon.svg
  scripts/smoke.sh                      built app vs fake daemon
  src-tauri/                            (own Cargo workspace: Tauri never enters `cargo test --workspace`)
    Cargo.toml build.rs tauri.conf.json capabilities/default.json icons/ src/main.rs src/lib.rs
  src/
    main.tsx App.tsx env.tsx styles.css vite-env.d.ts test-setup.ts security.test.ts App.test.tsx
    lib/      protocol.ts daemon.ts memguard.ts dispatch.ts repos.ts labels.ts notify.ts shortcuts.ts theme.ts (+ tests)
    store/    state.ts (reducer) select.ts (derived views) cockpit.ts (wiring) (+ tests)
    components/ AgentCard Badges PermissionCard TrustCard DoneCard useDiff DispatchComposer LeftRail Board Home
                MessageComposer DiffTab TerminalTab Detail Grid CommandBar MemoryPanel (+ tests)
    test/     fixtures.ts env.tsx (harness: real store, recording rpc)
docs/dogfood/cockpit.md                  manual checklist
```

Responsibilities: `lib/daemon.ts` is the only frontend module importing Tauri's IPC; components get `rpc`, the store, storage, `openUrl`, `startDaemon` and `notify` from `Env` (React context), so tests inject fakes. `store/state.ts` is pure; `store/cockpit.ts` owns timers and calls.

## Phase 3-0 — Daemon prerequisites (the only daemon changes)

### Task 1: Daemon: `task.events` (a task's history)

The cockpit rebuilds each task's step feed after a restart. The live stream only replays recent events, so the daemon gets a read-only `task.events` call: the last N audit events of one task, paged backwards.

**Files:**
- Modify: `crates/mirante-ipc/src/types.rs` (method `TASK_EVENTS`, `TaskEventsParams`, `TASK_EVENTS_MAX`)
- Modify: `crates/mirante-daemon/src/hub/mod.rs` (`Hub::task_events` + test)
- Modify: `crates/mirante-daemon/src/server.rs` (dispatch + test)

Write the failing tests first.

- [ ] **Step 1: Add the hub test at the top of `mod tests` — `crates/mirante-daemon/src/hub/mod.rs`**

Replace:

```rust
mod tests {
    use super::testkit::Kit;
    use super::*;
    use mirante_ipc::types::CreateTask;
```

with:

```rust
mod tests {
    use super::testkit::Kit;
    use super::*;
    use mirante_ipc::types::CreateTask;

    #[test]
    fn task_events_pages_backwards_oldest_first() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "ev").task;
        k.hub.lock().store.append_event(Some(&t.id), "step", &json!({"n": 1})).unwrap();
        k.hub.lock().store.append_event(Some(&t.id), "step", &json!({"n": 2})).unwrap();
        let all = k.hub.task_events(TaskEventsParams { task: t.id.clone(), ..Default::default() }).unwrap();
        assert_eq!(all.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["task_created", "step", "step"]);
        assert!(all.windows(2).all(|w| w[0].seq < w[1].seq));
        let last = k.hub.task_events(TaskEventsParams { task: "ev".into(), limit: Some(1), ..Default::default() }).unwrap();
        assert_eq!(last, all[2..].to_vec());
        let before =
            k.hub.task_events(TaskEventsParams { task: t.id.clone(), before_seq: Some(all[2].seq), limit: Some(10) });
        assert_eq!(before.unwrap(), all[..2].to_vec());
        let none = k.hub.task_events(TaskEventsParams { task: "nope-nope".into(), ..Default::default() });
        assert!(matches!(none, Err(HubError::NotFound(_))), "{none:?}");
    }
```

- [ ] **Step 2: Add the RPC test before `subscribe_sends_a_snapshot_then_events` — `crates/mirante-daemon/src/server.rs`**

Replace:

```rust
    #[test]
    fn subscribe_sends_a_snapshot_then_events() {
```

with:

```rust
    #[test]
    fn task_events_is_served_over_rpc() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "te").task;
        let v = dispatch(&k.hub, method::TASK_EVENTS, json!({"task": t.id, "limit": 5})).unwrap();
        assert_eq!(v[0]["kind"], "task_created");
        let e = dispatch(&k.hub, method::TASK_EVENTS, json!({})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
    }

    #[test]
    fn subscribe_sends_a_snapshot_then_events() {
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test -p mirante-daemon task_events`

Expected: FAIL — error[E0425]: cannot find value `TASK_EVENTS` in module `method` (and `TaskEventsParams` unresolved)

Implement: the wire types, the hub method, the dispatch arm.

- [ ] **Step 4: In `pub mod method` — `crates/mirante-ipc/src/types.rs`**

Replace:

```rust
    pub const TASK_ARCHIVE: &str = "task.archive";
```

with:

```rust
    pub const TASK_ARCHIVE: &str = "task.archive";
    /// A task's audit trail, oldest first ([`super::TaskEventsParams`]).
    pub const TASK_EVENTS: &str = "task.events";
```

- [ ] **Step 5: Before `DaemonStatus` — `crates/mirante-ipc/src/types.rs`**

Replace:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
```

with:

```rust
/// Most events one `task.events` call returns.
pub const TASK_EVENTS_MAX: usize = 500;

/// `task.events`: the last `limit` events of a task (default 200, at most
/// [`TASK_EVENTS_MAX`]) with `seq < before_seq` when given, oldest first. Page backwards by
/// passing the first returned `seq` as the next `before_seq`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEventsParams {
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
```

- [ ] **Step 6: Imports — `crates/mirante-daemon/src/hub/mod.rs`**

Replace:

```rust
use mirante_ipc::types::{BootState, CreateTask, DaemonStatus, Snapshot, TaskView};
```

with:

```rust
use mirante_ipc::types::{BootState, CreateTask, DaemonStatus, Snapshot, TaskEventsParams, TaskView, TASK_EVENTS_MAX};
```

- [ ] **Step 7: After `get_task` — `crates/mirante-daemon/src/hub/mod.rs`**

Replace:

```rust
    pub fn get_task(&self, reference: &str) -> Result<TaskView> {
        let st = self.lock();
        let t = st.resolve(reference)?;
        st.view(t)
    }
```

with:

```rust
    pub fn get_task(&self, reference: &str) -> Result<TaskView> {
        let st = self.lock();
        let t = st.resolve(reference)?;
        st.view(t)
    }

    /// A page of the task's audit trail, oldest first (see [`TaskEventsParams`]). Clients
    /// rebuild a task's step feed from it after a restart.
    pub fn task_events(&self, p: TaskEventsParams) -> Result<Vec<Event>> {
        let st = self.lock();
        let t = st.resolve(&p.task)?;
        let limit = p.limit.unwrap_or(200).clamp(1, TASK_EVENTS_MAX);
        let mut events: Vec<Event> =
            st.store.task_events(&t.id)?.into_iter().filter(|e| p.before_seq.is_none_or(|b| e.seq < b)).collect();
        let excess = events.len().saturating_sub(limit);
        events.drain(..excess);
        Ok(events)
    }
```

- [ ] **Step 8: In `dispatch` — `crates/mirante-daemon/src/server.rs`**

Replace:

```rust
        method::TASK_ARCHIVE => out(hub.archive(&params::<TaskRef>(p)?.task)),
```

with:

```rust
        method::TASK_ARCHIVE => out(hub.archive(&params::<TaskRef>(p)?.task)),
        method::TASK_EVENTS => out(hub.task_events(params(p)?)),
```

- [ ] **Step 9: Run and confirm**

Run: `cargo test -p mirante-daemon task_events`

Expected: exit 0; output contains `test result: ok. 2 passed`

- [ ] **Step 10: Run and confirm**

Run: `cargo test -p mirante-ipc -p mirante-daemon`

Expected: exit 0; output contains `test result: ok`

- [ ] **Step 11: Commit**

```bash
git add crates/mirante-ipc/src/types.rs crates/mirante-daemon/src/hub/mod.rs crates/mirante-daemon/src/server.rs
git commit -F- <<'EOF'
feat(daemon): task.events, a task's audit trail paged backwards

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 2: Daemon: Claude tool calls become feed steps

Codex steps come from its driver (`step` events: `command`, `agent_message`). Claude has none: the hub sees every `PostToolUse` / `PostToolUseFailure` hook but only uses it for status. This task records each as a `step` event `{type: "tool", tool, summary, ok, agent}` so the cards and the Steps tab show what Claude did. Side agents are tagged, not dropped (the user may want to see them).

**Files:**
- Modify: `crates/mirante-daemon/src/hub/session.rs` (`tool_step`, call in `apply_hook_envelope`, test)

- [ ] **Step 1: Add the failing test after the `status` helper in `mod tests` — `crates/mirante-daemon/src/hub/session.rs`**

Replace:

```rust
    fn status(k: &Kit, task: &str) -> Status {
        k.hub.get_task(task).unwrap().task.status
    }
```

with:

```rust
    fn status(k: &Kit, task: &str) -> Status {
        k.hub.get_task(task).unwrap().task.status
    }

    #[test]
    fn a_claude_tool_that_ran_becomes_a_feed_step() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "steps").task;
        let bash = json!({"command": "cargo test"});
        k.hook(&t.id, json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": bash}));
        let edit = json!({"file_path": "/w/src/lib.rs"});
        let failed = json!({"hook_event_name": "PostToolUseFailure", "tool_name": "Edit", "tool_input": edit, "agent_type": "Explore"});
        k.hook(&t.id, failed);
        k.hook(&t.id, json!({"hook_event_name": "PreToolUse", "tool_name": "Read", "tool_input": {"file_path": "/w/a"}}));
        let events = k.hub.lock().store.task_events(&t.id).unwrap();
        let steps: Vec<Value> = events.into_iter().filter(|e| e.kind == "step").map(|e| e.payload).collect();
        assert_eq!(
            steps,
            vec![
                json!({"type": "tool", "tool": "Bash", "summary": "cargo test", "ok": true, "agent": null}),
                json!({"type": "tool", "tool": "Edit", "summary": "/w/src/lib.rs", "ok": false, "agent": "Explore"}),
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p mirante-daemon a_claude_tool_that_ran`

Expected: FAIL — FAILED: `left: []` (no step events yet)

- [ ] **Step 3: Add `tool_step` before the first `impl State` — `crates/mirante-daemon/src/hub/session.rs`**

Replace:

```rust
impl State {
    /// Delivers the next queued message
```

with:

```rust
/// A Claude tool call that ran (`PostToolUse` / `PostToolUseFailure`) as a feed step:
/// `{type: "tool", tool, summary, ok, agent}`. `summary` is what the tool worked on (command,
/// path, pattern, URL, ...; at most 200 chars); `agent` is a side agent's type, else null.
/// Claude has no other step source: Codex steps come from its driver.
pub(crate) fn tool_step(ev: &HookEvent) -> Option<Value> {
    let ok = match ev.hook_event_name.as_str() {
        "PostToolUse" => true,
        "PostToolUseFailure" => false,
        _ => return None,
    };
    let tool = ev.tool_name.as_deref()?;
    let summary = ["command", "file_path", "path", "pattern", "url", "description", "prompt"]
        .iter()
        .find_map(|k| ev.tool_input.as_ref().and_then(|i| i[*k].as_str()).filter(|s| !s.is_empty()))
        .unwrap_or("");
    let summary: String = summary.chars().take(200).collect();
    Some(json!({ "type": "tool", "tool": tool, "summary": summary, "ok": ok, "agent": ev.agent_type }))
}

impl State {
    /// Delivers the next queued message
```

- [ ] **Step 4: In `Hub::apply_hook_envelope`, after `settle_tool_ran` — `crates/mirante-daemon/src/hub/session.rs`**

Replace:

```rust
        if tool_ran {
            st.settle_tool_ran(&p.task, &session_id, &ev)?;
        }
        st.publish();
```

with:

```rust
        if tool_ran {
            st.settle_tool_ran(&p.task, &session_id, &ev)?;
        }
        if let Some(step) = tool_step(&ev) {
            st.event(Some(&p.task), "step", step)?;
        }
        st.publish();
```

- [ ] **Step 5: Run and confirm**

Run: `cargo test -p mirante-daemon`

Expected: exit 0; output contains `test result: ok`

- [ ] **Step 6: Commit**

```bash
git add crates/mirante-daemon/src/hub/session.rs
git commit -F- <<'EOF'
feat(daemon): claude tool calls recorded as feed steps

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 3: Daemon: `task.diff` (the branch against its base)

The done card shows "N files, +a −r" and the Diff tab shows the patch; "Open PR" needs GitHub's compare URL and whether the branch was pushed. Per-turn checkpoint refs (spec §4) stay out of scope: the diff is the worktree (committed + uncommitted) against the fork point from the repo checkout's current branch, which is the base `task.create` uses by default. Read-only git, no network (`pushed` checks the local `refs/remotes/origin/<branch>`).

**Files:**
- Create: `crates/mirante-daemon/src/diff.rs`
- Modify: `crates/mirante-daemon/src/lib.rs`, `crates/mirante-ipc/src/types.rs`, `crates/mirante-daemon/src/hub/mod.rs`, `crates/mirante-daemon/src/server.rs`

- [ ] **Step 1: Add the failing hub test — `crates/mirante-daemon/src/hub/mod.rs`**

Replace:

```rust
    #[test]
    fn task_events_pages_backwards_oldest_first() {
```

with:

```rust
    #[test]
    fn task_diff_reports_committed_uncommitted_and_untracked_changes() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "feat/d").task;
        let wt = Path::new(&t.worktree_path);
        let git = |dir: &Path, args: &[&str]| {
            let ok = std::process::Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success();
            assert!(ok, "git {args:?}");
        };
        std::fs::write(wt.join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        git(wt, &["add", "lib.rs"]);
        git(wt, &["commit", "-q", "-m", "add lib"]);
        std::fs::write(wt.join("README"), "hello\n").unwrap();
        std::fs::write(wt.join("notes.txt"), "scratch\n").unwrap();
        let d = k.hub.task_diff("feat/d").unwrap();
        assert_eq!(d.base, "trunk");
        let mut files: Vec<(String, Option<u64>, Option<u64>)> =
            d.files.iter().map(|f| (f.path.clone(), f.added, f.removed)).collect();
        files.sort();
        assert_eq!(files, vec![("README".into(), Some(1), Some(1)), ("lib.rs".into(), Some(2), Some(0))]);
        assert_eq!(d.untracked, vec!["notes.txt".to_string()]);
        assert!(d.patch.contains("+fn b() {}") && d.patch.contains("+hello"), "{}", d.patch);
        assert!(!d.truncated && !d.pushed);
        assert_eq!(d.compare_url, None);
        git(&k.repo, &["remote", "add", "origin", "git@github.com:acme/app.git"]);
        let url = k.hub.task_diff(&t.id).unwrap().compare_url;
        assert_eq!(url.as_deref(), Some("https://github.com/acme/app/compare/trunk...feat/d?expand=1"));
    }

    #[test]
    fn task_events_pages_backwards_oldest_first() {
```

- [ ] **Step 2: Extend the RPC test — `crates/mirante-daemon/src/server.rs`**

Replace:

```rust
        let e = dispatch(&k.hub, method::TASK_EVENTS, json!({})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
    }
```

with:

```rust
        let e = dispatch(&k.hub, method::TASK_EVENTS, json!({})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
        let d = dispatch(&k.hub, method::TASK_DIFF, json!({"task": "te"})).unwrap();
        assert_eq!((d["base"].as_str(), d["files"].as_array().map(Vec::len)), (Some("trunk"), Some(0)));
    }
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test -p mirante-daemon task_diff`

Expected: FAIL — error[E0425]: cannot find value `TASK_DIFF` in module `method`; error[E0599]: no method named `task_diff` found for struct `Arc<Hub>`

- [ ] **Step 4: Create `crates/mirante-daemon/src/diff.rs`**

```rust
//! `task.diff`: what a task branch changed against its base, read with git. Read-only: it
//! never writes to the repo or the worktree. Uncommitted changes count (the agent may not
//! have committed yet); untracked files are listed by name.
use std::path::Path;
use std::process::Command;

use mirante_ipc::types::{DiffFile, TaskDiff};

/// Largest patch returned; the rest is cut (`truncated: true`).
pub const PATCH_CAP: usize = 512 * 1024;

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `git diff --numstat` lines; a binary file (`-\t-\tpath`) has no counts.
pub fn parse_numstat(text: &str) -> Vec<DiffFile> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\t');
            let (a, r, path) = (parts.next()?, parts.next()?, parts.next()?);
            Some(DiffFile { path: path.to_string(), added: a.parse().ok(), removed: r.parse().ok() })
        })
        .collect()
}

/// GitHub's "open a pull request" page for `branch` against `base`, when `remote` is a
/// GitHub URL (`git@github.com:o/r.git`, `https://github.com/o/r(.git)`,
/// `ssh://git@github.com/o/r.git`). Anything else is `None`.
pub fn github_compare_url(remote: &str, base: &str, branch: &str) -> Option<String> {
    let path = remote
        .strip_prefix("git@github.com:")
        .or_else(|| remote.strip_prefix("https://github.com/"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let safe = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if parts.next().is_some() || !safe(owner) || !safe(repo) {
        return None;
    }
    let enc = |s: &str| -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    };
    Some(format!("https://github.com/{owner}/{repo}/compare/{}...{}?expand=1", enc(base), enc(branch)))
}

/// The diff of `worktree` (on `branch`) against its fork point from the repo checkout's
/// current branch, which is the base `task.create` uses by default (a detached checkout
/// falls back to its commit).
pub fn task_diff(repo: &Path, worktree: &Path, branch: &str) -> Result<TaskDiff, String> {
    let base = match git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(b) => b.trim().to_string(),
        Err(_) => git(repo, &["rev-parse", "HEAD"])?.trim().to_string(),
    };
    let merge_base = git(worktree, &["merge-base", "HEAD", &base])?.trim().to_string();
    let files = parse_numstat(&git(worktree, &["diff", "--numstat", &merge_base, "--"])?);
    let untracked = git(worktree, &["ls-files", "--others", "--exclude-standard"])?.lines().map(str::to_string).collect();
    let mut patch = git(worktree, &["diff", &merge_base, "--"])?;
    let truncated = patch.len() > PATCH_CAP;
    if truncated {
        let mut cut = PATCH_CAP;
        while !patch.is_char_boundary(cut) {
            cut -= 1;
        }
        patch.truncate(cut);
    }
    let compare_url =
        git(repo, &["remote", "get-url", "origin"]).ok().and_then(|u| github_compare_url(u.trim(), &base, branch));
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let pushed = git(worktree, &["rev-parse", "--verify", "--quiet", &remote_ref]).is_ok();
    Ok(TaskDiff { base, merge_base, files, untracked, patch, truncated, compare_url, pushed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_lines_parse_with_binary_files_uncounted() {
        let files = parse_numstat("3\t1\tsrc/a.rs\n-\t-\tlogo.png\n");
        assert_eq!(
            files,
            vec![
                DiffFile { path: "src/a.rs".into(), added: Some(3), removed: Some(1) },
                DiffFile { path: "logo.png".into(), added: None, removed: None },
            ]
        );
    }

    #[test]
    fn compare_urls_only_for_github_remotes() {
        let want = Some("https://github.com/acme/app/compare/main...feat/x%23y?expand=1".to_string());
        assert_eq!(github_compare_url("git@github.com:acme/app.git", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("https://github.com/acme/app", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("ssh://git@github.com/acme/app.git/", "main", "feat/x#y"), want);
        assert_eq!(github_compare_url("git@gitlab.com:acme/app.git", "main", "x"), None);
        assert_eq!(github_compare_url("https://github.com/acme/app/extra", "main", "x"), None);
        assert_eq!(github_compare_url("https://github.com/ac me/app", "main", "x"), None);
    }
}
```

- [ ] **Step 5: Module list — `crates/mirante-daemon/src/lib.rs`**

Replace:

```rust
pub mod driver;
```

with:

```rust
pub mod diff;
pub mod driver;
```

- [ ] **Step 6: In `pub mod method` — `crates/mirante-ipc/src/types.rs`**

Replace:

```rust
    pub const TASK_EVENTS: &str = "task.events";
```

with:

```rust
    pub const TASK_EVENTS: &str = "task.events";
    /// The task branch's changes against its base ([`super::TaskDiff`]).
    pub const TASK_DIFF: &str = "task.diff";
```

- [ ] **Step 7: Before `TASK_EVENTS_MAX` — `crates/mirante-ipc/src/types.rs`**

Replace:

```rust
/// Most events one `task.events` call returns.
```

with:

```rust
/// One changed file in a [`TaskDiff`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    pub path: String,
    /// Lines added / removed; `None` for a binary file.
    pub added: Option<u64>,
    pub removed: Option<u64>,
}

/// `task.diff` result: the task branch (committed and uncommitted) against the fork point
/// from its base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDiff {
    /// The base branch (the repo checkout's current branch), or a commit when detached.
    pub base: String,
    pub merge_base: String,
    pub files: Vec<DiffFile>,
    /// Untracked files (not in `patch`).
    pub untracked: Vec<String>,
    /// Unified diff, cut at 512 KiB.
    pub patch: String,
    pub truncated: bool,
    /// GitHub's "open a pull request" page, when `origin` is on GitHub.
    pub compare_url: Option<String>,
    /// `origin/<branch>` exists locally (the branch was pushed at least once).
    pub pushed: bool,
}

/// Most events one `task.events` call returns.
```

- [ ] **Step 8: Imports — `crates/mirante-daemon/src/hub/mod.rs`**

Replace:

```rust
use mirante_ipc::types::{BootState, CreateTask, DaemonStatus, Snapshot, TaskEventsParams, TaskView, TASK_EVENTS_MAX};
```

with:

```rust
use mirante_ipc::types::{
    BootState, CreateTask, DaemonStatus, Snapshot, TaskDiff, TaskEventsParams, TaskView, TASK_EVENTS_MAX,
};
```

- [ ] **Step 9: Before `task_events` — `crates/mirante-daemon/src/hub/mod.rs`**

Replace:

```rust
    /// A page of the task's audit trail
```

with:

```rust
    /// The task branch's changes against its base. git runs without the hub lock held.
    pub fn task_diff(&self, reference: &str) -> Result<TaskDiff> {
        let t = self.lock().resolve(reference)?;
        if t.lifecycle == Lifecycle::Archived {
            return Err(HubError::Conflict(format!("task {} is archived; its worktree is gone", t.branch)));
        }
        crate::diff::task_diff(Path::new(&t.repo_path), Path::new(&t.worktree_path), &t.branch).map_err(HubError::Conflict)
    }

    /// A page of the task's audit trail
```

- [ ] **Step 10: In `dispatch` — `crates/mirante-daemon/src/server.rs`**

Replace:

```rust
        method::TASK_ARCHIVE => out(hub.archive(&params::<TaskRef>(p)?.task)),
        method::TASK_EVENTS => out(hub.task_events(params(p)?)),
```

with:

```rust
        method::TASK_ARCHIVE => out(hub.archive(&params::<TaskRef>(p)?.task)),
        method::TASK_EVENTS => out(hub.task_events(params(p)?)),
        method::TASK_DIFF => out(hub.task_diff(&params::<TaskRef>(p)?.task)),
```

- [ ] **Step 11: Run and confirm**

Run: `cargo test -p mirante-daemon -- diff::tests task_diff`

Expected: exit 0; output contains `test result: ok. 3 passed`

- [ ] **Step 12: Run and confirm**

Run: `cargo test -p mirante-daemon && cargo clippy -p mirante-daemon --all-targets -- -D warnings`

Expected: exit 0; output contains `test result: ok`

- [ ] **Step 13: Commit**

```bash
git add crates/mirante-daemon/src/diff.rs crates/mirante-daemon/src/lib.rs crates/mirante-ipc/src/types.rs crates/mirante-daemon/src/hub/mod.rs crates/mirante-daemon/src/server.rs
git commit -F- <<'EOF'
feat(daemon): task.diff against the fork point, with GitHub compare URL and pushed flag

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


## Phase 3-i — Bridge and client store

### Task 4: `mirante-bridge`: fake daemon and allowlisted RPC

A new library crate holds everything the Tauri shell does, with no Tauri types, so it is tested against a scripted fake daemon on a real Unix socket. RPC calls open one short-lived connection each (a 45 s `session.start` must not block a permission answer) and only allowlisted methods pass. Also: `mirante-cli`'s `start_daemon` gets a variant taking the binary path, used by the app in Task 6.

**Files:**
- Create: `crates/mirante-bridge/Cargo.toml`, `src/lib.rs`, `src/fake.rs`, `src/rpc.rs`
- Modify: `Cargo.toml` (workspace member)
- Modify: `crates/mirante-cli/src/daemon_ctl.rs` (`start_daemon_with`)

- [ ] **Step 1: Workspace members — `Cargo.toml`**

Replace:

```toml
    "crates/mirante-cli",
]
```

with:

```toml
    "crates/mirante-cli",
    "crates/mirante-bridge",
]
```

- [ ] **Step 2: Create `crates/mirante-bridge/Cargo.toml`**

```toml
[package]
name = "mirante-bridge"
version = "0.1.0"
edition = "2021"
publish = false
license = "Apache-2.0"
description = "Daemon client for the Mirante desktop cockpit: allowlisted RPC, resumable event stream, attach."

[features]
# The scripted fake daemon (`mirante-fake-daemon`), for UI development and the app smoke test.
fake = []

[dependencies]
mirante-core = { path = "../mirante-core" }
mirante-ipc = { path = "../mirante-ipc" }
mirante-cli = { path = "../mirante-cli" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[[bin]]
name = "mirante-fake-daemon"
path = "src/bin/mirante-fake-daemon.rs"
required-features = ["fake"]
```

`lib.rs` already names the modules of Tasks 5 and 6; create them empty now so the crate builds (`touch`), they are filled in their tasks.

- [ ] **Step 3: Create `crates/mirante-bridge/src/lib.rs`**

```rust
//! The desktop cockpit's side of the daemon socket. Plain std threads and blocking I/O
//! (like the rest of Mirante), no Tauri types: the Tauri shell in `apps/cockpit/src-tauri`
//! only forwards these calls and messages, so everything here is tested against a fake
//! daemon without a window.
//!
//! - [`rpc`]: one request per connection, restricted to an allowlist of methods.
//! - [`stream`]: `events.subscribe` in a thread that reconnects and resumes with
//!   `since_seq` + `boot_id`.
//! - [`attach`]: a task's live terminal as raw bytes both ways.
//! - [`daemon`]: finding and starting `mirante-daemon`.
pub mod attach;
pub mod daemon;
pub mod rpc;
pub mod stream;

#[cfg(any(test, feature = "fake"))]
pub mod fake;

pub use rpc::{call, BridgeError};
```

- [ ] **Step 4: Run**

```bash
mkdir -p crates/mirante-bridge/src/bin && touch crates/mirante-bridge/src/stream.rs crates/mirante-bridge/src/attach.rs crates/mirante-bridge/src/daemon.rs && printf 'fn main() {}\n' > crates/mirante-bridge/src/bin/mirante-fake-daemon.rs
```

- [ ] **Step 5: Create `crates/mirante-bridge/src/fake.rs`**

```rust
//! A scripted stand-in for `mirante-daemon` on a real Unix socket: the same framing and
//! method names, canned answers, events pushed by the test. Every request is recorded.
use std::io::{BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mirante_ipc::rpc::{code, read_line, write_line, Notification, Request, Response, RpcError};
use mirante_ipc::types::{method, SubscribeParams};
use serde_json::{json, Value};

#[derive(Default)]
struct Shared {
    calls: Vec<(String, Value)>,
    boot_id: String,
    events: Vec<Value>,
    tasks: Vec<Value>,
    subscribers: Vec<UnixStream>,
    open: Vec<UnixStream>,
}

#[derive(Clone)]
pub struct FakeDaemon {
    pub socket: PathBuf,
    shared: Arc<Mutex<Shared>>,
}

impl FakeDaemon {
    /// Binds `socket` and serves until the process ends.
    pub fn start(socket: &Path) -> FakeDaemon {
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket).expect("bind fake daemon socket");
        let shared = Arc::new(Mutex::new(Shared { boot_id: "boot-1".into(), ..Default::default() }));
        let s = shared.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let s = s.clone();
                std::thread::spawn(move || serve(s, conn));
            }
        });
        FakeDaemon { socket: socket.to_path_buf(), shared }
    }

    pub fn calls(&self) -> Vec<(String, Value)> {
        self.shared.lock().unwrap().calls.clone()
    }

    pub fn methods(&self) -> Vec<String> {
        self.calls().into_iter().map(|(m, _)| m).collect()
    }

    /// Waits until `method` was called `n` times.
    pub fn wait_calls(&self, method: &str, n: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.methods().iter().filter(|m| *m == method).count() >= n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// The `TaskView` JSON values served by snapshots and `task.list`.
    pub fn set_tasks(&self, tasks: Vec<Value>) {
        self.shared.lock().unwrap().tasks = tasks;
    }

    /// Appends an event (next `seq`) and sends it to every subscriber. Returns its seq.
    pub fn push_event(&self, task: Option<&str>, kind: &str, payload: Value) -> u64 {
        let mut s = self.shared.lock().unwrap();
        let seq = s.events.len() as u64 + 1;
        let ev = json!({"seq": seq, "boot_id": s.boot_id, "task_id": task, "kind": kind, "payload": payload, "ts_ms": 1_700_000_000_000u64 + seq});
        s.events.push(ev.clone());
        let line = Notification::new(method::EVENT, ev);
        s.subscribers.retain_mut(|w| write_line(w, &line).is_ok());
        seq
    }

    /// Closes every open connection (as a daemon restart would), keeping the boot id.
    pub fn drop_connections(&self) {
        let mut s = self.shared.lock().unwrap();
        for c in s.open.drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
        s.subscribers.clear();
    }

    /// A new boot: later subscriptions get a full snapshot whatever they ask for.
    pub fn set_boot(&self, boot: &str) {
        self.shared.lock().unwrap().boot_id = boot.into();
    }
}

fn record(shared: &Arc<Mutex<Shared>>, req: &Request) {
    shared.lock().unwrap().calls.push((req.method.clone(), req.params.clone()));
}

fn serve(shared: Arc<Mutex<Shared>>, conn: UnixStream) {
    let Ok(mut writer) = conn.try_clone() else { return };
    if let Ok(c) = conn.try_clone() {
        shared.lock().unwrap().open.push(c);
    }
    let mut reader = BufReader::new(conn);
    while let Ok(Some(line)) = read_line(&mut reader) {
        let Ok(req) = serde_json::from_str::<Request>(&line) else {
            let _ = write_line(&mut writer, &Response::err(None, RpcError::new(code::PARSE, "bad json")));
            continue;
        };
        record(&shared, &req);
        match req.method.as_str() {
            method::EVENTS_SUBSCRIBE => return subscribe(&shared, req, writer, reader),
            method::ATTACH => return attach(req, writer, reader),
            _ => {
                let resp = match answer(&shared, &req.method, &req.params) {
                    Ok(v) => Response::ok(req.id, v),
                    Err(e) => Response::err(Some(req.id), e),
                };
                if write_line(&mut writer, &resp).is_err() {
                    return;
                }
            }
        }
    }
}

fn answer(shared: &Arc<Mutex<Shared>>, m: &str, p: &Value) -> Result<Value, RpcError> {
    let s = shared.lock().unwrap();
    match m {
        method::DAEMON_STATUS => Ok(json!({"pid": 1, "boot_id": s.boot_id, "version": "fake", "live_sessions": 0})),
        method::TASK_LIST => Ok(Value::Array(s.tasks.clone())),
        method::TASK_GET => s
            .tasks
            .iter()
            .find(|t| t["id"] == p["task"])
            .cloned()
            .ok_or_else(|| RpcError::new(code::NOT_FOUND, format!("no task {}", p["task"]))),
        method::PERMISSION_LIST | method::TASK_EVENTS => Ok(json!([])),
        _ => Ok(json!({})),
    }
}

fn subscribe(shared: &Arc<Mutex<Shared>>, req: Request, mut writer: UnixStream, mut reader: BufReader<UnixStream>) {
    let p: SubscribeParams = serde_json::from_value(req.params).unwrap_or_default();
    {
        let mut s = shared.lock().unwrap();
        let last = s.events.len() as u64;
        let resume = p.since_seq.filter(|q| p.boot_id.as_deref() == Some(s.boot_id.as_str()) && *q <= last);
        let snap = match resume {
            Some(q) => json!({"boot_id": s.boot_id, "seq": q, "tasks": [], "resumed": true}),
            None => json!({"boot_id": s.boot_id, "seq": last, "tasks": s.tasks, "resumed": false}),
        };
        if write_line(&mut writer, &Response::ok(req.id, snap)).is_err() {
            return;
        }
        let from = resume.unwrap_or(last) as usize;
        for ev in &s.events[from..] {
            let _ = write_line(&mut writer, &Notification::new(method::EVENT, ev.clone()));
        }
        if let Ok(w) = writer.try_clone() {
            s.subscribers.push(w);
        }
    }
    // Hold the connection until the client goes away.
    let mut sink = [0u8; 64];
    while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
}

/// Answers `{input: true}`, writes a banner, then echoes every input byte back.
fn attach(req: Request, mut writer: UnixStream, mut reader: BufReader<UnixStream>) {
    if write_line(&mut writer, &Response::ok(req.id, json!({"input": true}))).is_err() {
        return;
    }
    let _ = writer.write_all(b"fake terminal\r\n");
    let mut buf = [0u8; 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if writer.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = writer.shutdown(Shutdown::Both);
}

/// A task view as the daemon serves it (`TaskView`: the task flattened plus live facts).
pub fn task_json(id: &str, repo: &str, branch: &str, agent: &str, status: &str) -> Value {
    json!({
        "id": id, "title": format!("goal of {branch}"), "repo_path": repo,
        "worktree_path": format!("/wt/{branch}"), "branch": branch, "agent": agent,
        "goal": format!("goal of {branch}"), "notes": "", "lifecycle": "active", "status": status,
        "created_at": 1, "updated_at": 2, "live": true, "boot": "ready", "needs_you": 0, "queued": 0,
        "detail": null
    })
}
```

Rust tests live in the same file as the code (repo convention), so the red step is the test module against a stub: write `rpc.rs` below, but first run its tests with `call`'s body replaced by `todo!()` — the four tests fail with `not yet implemented`. Then restore the body shown.

- [ ] **Step 6: Create `crates/mirante-bridge/src/rpc.rs`**

```rust
//! Request/response calls, one short-lived connection each (a 45 s `session.start` must not
//! hold up an `answer` clicked meanwhile). Only methods a person may trigger from the
//! cockpit pass: never `hook.event` (that is the hook's), `daemon.shutdown` or
//! `daemon.kill_orphan` (the CLI's `mirante doctor` covers those), and the two
//! connection-taking methods go through [`crate::stream`] / [`crate::attach`].
use std::path::Path;
use std::time::Duration;

use mirante_ipc::types::method;
use mirante_ipc::{Client, ClientError};
use serde::Serialize;
use serde_json::Value;

pub const ALLOWED: &[&str] = &[
    method::TASK_CREATE,
    method::TASK_LIST,
    method::TASK_GET,
    method::TASK_ARCHIVE,
    method::TASK_EVENTS,
    method::TASK_DIFF,
    method::SESSION_START,
    method::SESSION_RESUME,
    method::SESSION_TRUST,
    method::SESSION_STOP,
    method::SESSION_INTERRUPT,
    method::SESSION_RESIZE,
    method::MESSAGE_SEND,
    method::PERMISSION_LIST,
    method::PERMISSION_ANSWER,
    method::DAEMON_STATUS,
    method::SESSION_HIBERNATE,
    method::SESSION_WAKE,
];

/// `session.start` waits for the agent to boot (up to 45 s in the daemon).
pub const CALL_TIMEOUT: Duration = Duration::from_secs(90);

/// What the frontend gets on failure, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BridgeError {
    /// No daemon on the socket (or the connection broke).
    Disconnected { message: String },
    /// The daemon refused: `code` is one of `mirante_ipc::rpc::code`.
    Rpc { code: i64, message: String },
    /// The daemon sent something unexpected.
    Protocol { message: String },
    /// The cockpit may not call this method.
    Forbidden { method: String },
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::Disconnected { message } => write!(f, "daemon not reachable: {message}"),
            BridgeError::Rpc { code, message } => write!(f, "{message} (code {code})"),
            BridgeError::Protocol { message } => write!(f, "daemon protocol: {message}"),
            BridgeError::Forbidden { method } => write!(f, "{method} is not allowed from the cockpit"),
        }
    }
}

impl std::error::Error for BridgeError {}

impl From<ClientError> for BridgeError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Io(e) => BridgeError::Disconnected { message: e.to_string() },
            ClientError::Rpc(e) => BridgeError::Rpc { code: e.code, message: e.message },
            ClientError::Protocol(m) => BridgeError::Protocol { message: m },
        }
    }
}

/// One allowlisted call. `params` of `null` is sent as `{}`.
pub fn call(socket: &Path, method: &str, params: Value) -> Result<Value, BridgeError> {
    if !ALLOWED.contains(&method) {
        return Err(BridgeError::Forbidden { method: method.to_string() });
    }
    let params = if params.is_null() { Value::Object(Default::default()) } else { params };
    let mut c = Client::connect(socket).map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
    c.set_timeout(Some(CALL_TIMEOUT)).map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
    Ok(c.call(method, params)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{task_json, FakeDaemon};
    use mirante_ipc::rpc::code;
    use mirante_ipc::test_dir::TestDir;
    use serde_json::json;

    #[test]
    fn allowed_calls_reach_the_daemon_and_return_its_json() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        fake.set_tasks(vec![task_json("t1", "/r/app", "feat/a", "claude", "working")]);
        let v = call(&fake.socket, method::TASK_LIST, Value::Null).unwrap();
        assert_eq!(v[0]["branch"], "feat/a");
        assert_eq!(fake.calls(), vec![(method::TASK_LIST.to_string(), json!({}))]);
    }

    #[test]
    fn daemon_errors_keep_their_code() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let e = call(&fake.socket, method::TASK_GET, json!({"task": "missing"})).unwrap_err();
        assert!(matches!(e, BridgeError::Rpc { code: c, .. } if c == code::NOT_FOUND), "{e:?}");
    }

    #[test]
    fn methods_outside_the_allowlist_never_reach_the_socket() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let refused = [
            method::HOOK_EVENT,
            method::DAEMON_SHUTDOWN,
            method::DAEMON_KILL_ORPHAN,
            method::EVENTS_SUBSCRIBE,
            method::ATTACH,
            "x.y",
        ];
        for m in refused {
            assert_eq!(call(&fake.socket, m, json!({})), Err(BridgeError::Forbidden { method: m.into() }));
        }
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn no_daemon_is_a_disconnected_error() {
        let d = TestDir::new();
        let e = call(&d.path().join("none.sock"), method::DAEMON_STATUS, json!({})).unwrap_err();
        assert!(matches!(e, BridgeError::Disconnected { .. }), "{e:?}");
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["kind"], "disconnected");
    }
}
```

- [ ] **Step 7: Split `start_daemon` — `crates/mirante-cli/src/daemon_ctl.rs`**

Replace:

```rust
pub fn start_daemon(paths: &Paths) -> io::Result<Client> {
    paths.ensure_home()?;
    let _ = logfile::rotate_if_over(&paths.log(), logfile::LOG_CAP);
    let log = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(paths.log())?;
    let mut cmd = daemon_command(&daemon_bin()?, paths, env_names());
```

with:

```rust
pub fn start_daemon(paths: &Paths) -> io::Result<Client> {
    start_daemon_with(paths, &daemon_bin()?)
}

/// [`start_daemon`] with an explicit daemon binary (the desktop app is not installed next to
/// `mirante-daemon`, so it finds the binary itself).
pub fn start_daemon_with(paths: &Paths, bin: &Path) -> io::Result<Client> {
    paths.ensure_home()?;
    let _ = logfile::rotate_if_over(&paths.log(), logfile::LOG_CAP);
    let log = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(paths.log())?;
    let mut cmd = daemon_command(bin, paths, env_names());
```

- [ ] **Step 8: Run and confirm**

Run: `cargo test -p mirante-bridge rpc && cargo test -p mirante-cli --lib`

Expected: exit 0; output contains `test result: ok. 4 passed`

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml Cargo.lock crates/mirante-bridge crates/mirante-cli/src/daemon_ctl.rs
git commit -F- <<'EOF'
feat(bridge): mirante-bridge crate with a fake daemon and allowlisted RPC

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 5: Bridge: resumable event stream

`events.subscribe` in its own thread. On a drop it reconnects with backoff (250 ms doubling to 5 s) and resumes with the last `seq` + `boot_id`; the daemon replays the gap or, on a new boot, sends a full snapshot. Messages are serialized `{type: connected|event|disconnected}` for the frontend.

**Files:**
- Modify (fill): `crates/mirante-bridge/src/stream.rs`

- [ ] **Step 1: Create `crates/mirante-bridge/src/stream.rs`**

```rust
//! The event stream: `events.subscribe` in its own thread, forwarded as [`StreamMsg`]s.
//! When the connection drops (daemon restart, or the daemon cut a subscriber that fell
//! behind) it reconnects with backoff and asks to resume from the last `seq` + `boot_id`;
//! the daemon then replays what was missed, or sends a full snapshot (new boot, gap too big).
use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use mirante_core::model::Event;
use mirante_ipc::rpc::read_line;
use mirante_ipc::types::{method, Snapshot, SubscribeParams};
use mirante_ipc::Client;
use serde::Serialize;

pub const BACKOFF_START: Duration = Duration::from_millis(250);
pub const BACKOFF_MAX: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamMsg {
    /// Subscribed. `snapshot.resumed` means "keep your state, missed events follow".
    Connected { snapshot: Snapshot },
    Event { event: Event },
    /// The stream is down; the next attempt is in `retry_ms`.
    Disconnected { error: String, retry_ms: u64 },
}

/// The running stream. Dropping it (or [`Stream::stop`]) ends the thread.
pub struct Stream {
    stop: Arc<AtomicBool>,
    conn: Arc<Mutex<Option<UnixStream>>>,
    thread: Option<JoinHandle<()>>,
}

impl Stream {
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(c) = self.conn.lock().unwrap().take() {
            let _ = c.shutdown(Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Starts the stream thread; `emit` is called from it for every message, in order.
pub fn spawn(socket: PathBuf, emit: impl Fn(StreamMsg) + Send + 'static) -> Stream {
    let stop = Arc::new(AtomicBool::new(false));
    let conn = Arc::new(Mutex::new(None));
    let (s, c) = (stop.clone(), conn.clone());
    let thread = std::thread::spawn(move || run(socket, emit, s, c));
    Stream { stop, conn, thread: Some(thread) }
}

fn run(socket: PathBuf, emit: impl Fn(StreamMsg), stop: Arc<AtomicBool>, conn: Arc<Mutex<Option<UnixStream>>>) {
    let mut resume = SubscribeParams::default();
    let mut backoff = BACKOFF_START;
    while !stop.load(Ordering::SeqCst) {
        let error = match once(&socket, &mut resume, &emit, &stop, &conn, &mut backoff) {
            Ok(()) => "the daemon closed the event stream".to_string(),
            Err(e) => e,
        };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        emit(StreamMsg::Disconnected { error, retry_ms: backoff.as_millis() as u64 });
        let mut waited = Duration::ZERO;
        while waited < backoff && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(25));
            waited += Duration::from_millis(25);
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

fn once(
    socket: &std::path::Path,
    resume: &mut SubscribeParams,
    emit: &impl Fn(StreamMsg),
    stop: &AtomicBool,
    conn: &Mutex<Option<UnixStream>>,
    backoff: &mut Duration,
) -> Result<(), String> {
    let mut client = Client::connect(socket).map_err(|e| e.to_string())?;
    client.set_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let snapshot: Snapshot = client.call(method::EVENTS_SUBSCRIBE, &*resume).map_err(|e| e.to_string())?;
    let (mut reader, writer): (BufReader<UnixStream>, UnixStream) = client.into_raw();
    *conn.lock().unwrap() = Some(writer);
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    *resume = SubscribeParams { since_seq: Some(snapshot.seq), boot_id: Some(snapshot.boot_id.clone()) };
    *backoff = BACKOFF_START;
    emit(StreamMsg::Connected { snapshot });
    loop {
        let Some(line) = read_line(&mut reader).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        let n: mirante_ipc::rpc::Notification = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if n.method != method::EVENT {
            continue;
        }
        let event: Event = serde_json::from_value(n.params).map_err(|e| e.to_string())?;
        *resume = SubscribeParams { since_seq: Some(event.seq), boot_id: Some(event.boot_id.clone()) };
        emit(StreamMsg::Event { event });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{task_json, FakeDaemon};
    use mirante_ipc::test_dir::TestDir;
    use serde_json::json;
    use std::sync::mpsc;

    fn next(rx: &mpsc::Receiver<StreamMsg>) -> StreamMsg {
        rx.recv_timeout(Duration::from_secs(5)).expect("a stream message")
    }

    fn connected(rx: &mpsc::Receiver<StreamMsg>) -> Snapshot {
        loop {
            if let StreamMsg::Connected { snapshot } = next(rx) {
                return snapshot;
            }
        }
    }

    #[test]
    fn snapshot_then_events_in_order() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        fake.set_tasks(vec![task_json("t1", "/r/app", "a", "claude", "idle")]);
        let (tx, rx) = mpsc::channel();
        let _s = spawn(fake.socket.clone(), move |m| tx.send(m).unwrap());
        let snap = connected(&rx);
        assert_eq!((snap.seq, snap.tasks.len(), snap.resumed), (0, 1, false));
        fake.push_event(Some("t1"), "status", json!({"from": "idle", "to": "working"}));
        fake.push_event(Some("t1"), "step", json!({"type": "tool"}));
        let kinds: Vec<(u64, String)> = (0..2)
            .map(|_| match next(&rx) {
                StreamMsg::Event { event } => (event.seq, event.kind),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(kinds, vec![(1, "status".into()), (2, "step".into())]);
    }

    #[test]
    fn a_dropped_stream_reconnects_and_resumes_from_the_last_seq() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let (tx, rx) = mpsc::channel();
        let _s = spawn(fake.socket.clone(), move |m| tx.send(m).unwrap());
        connected(&rx);
        fake.push_event(Some("t1"), "step", json!({}));
        assert!(matches!(next(&rx), StreamMsg::Event { .. }));
        fake.drop_connections();
        assert!(matches!(next(&rx), StreamMsg::Disconnected { retry_ms: 250, .. }));
        // Missed while down: replayed after the resumed snapshot.
        fake.push_event(Some("t1"), "status", json!({"to": "needs_you"}));
        let snap = connected(&rx);
        assert!(snap.resumed);
        assert_eq!(snap.seq, 1);
        match next(&rx) {
            StreamMsg::Event { event } => assert_eq!((event.seq, event.kind.as_str()), (2, "status")),
            other => panic!("{other:?}"),
        }
        let subs: Vec<serde_json::Value> =
            fake.calls().into_iter().filter(|(m, _)| m == method::EVENTS_SUBSCRIBE).map(|(_, p)| p).collect();
        assert_eq!(subs, vec![json!({}), json!({"since_seq": 1, "boot_id": "boot-1"})]);
    }

    #[test]
    fn a_new_boot_gets_a_full_snapshot() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let (tx, rx) = mpsc::channel();
        let _s = spawn(fake.socket.clone(), move |m| tx.send(m).unwrap());
        connected(&rx);
        fake.set_boot("boot-2");
        fake.set_tasks(vec![task_json("t9", "/r/app", "z", "codex", "idle")]);
        fake.drop_connections();
        let snap = connected(&rx);
        assert!(!snap.resumed);
        assert_eq!((snap.boot_id.as_str(), snap.tasks.len()), ("boot-2", 1));
    }

    #[test]
    fn no_daemon_retries_with_growing_backoff_and_stop_ends_the_thread() {
        let d = TestDir::new();
        let (tx, rx) = mpsc::channel();
        let s = spawn(d.path().join("none.sock"), move |m| {
            let _ = tx.send(m);
        });
        let waits: Vec<u64> = (0..2)
            .map(|_| match next(&rx) {
                StreamMsg::Disconnected { retry_ms, .. } => retry_ms,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(waits, vec![250, 500]);
        s.stop();
    }

    #[test]
    fn messages_serialize_tagged_for_the_frontend() {
        let m = StreamMsg::Disconnected { error: "x".into(), retry_ms: 250 };
        assert_eq!(serde_json::to_value(m).unwrap(), json!({"type": "disconnected", "error": "x", "retry_ms": 250}));
    }
}
```

- [ ] **Step 2: Run and confirm**

Run: `cargo test -p mirante-bridge stream`

Expected: exit 0; output contains `test result: ok. 5 passed`

- [ ] **Step 3: Commit**

```bash
git add crates/mirante-bridge/src/stream.rs
git commit -F- <<'EOF'
feat(bridge): event stream that reconnects and resumes from seq + boot id

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 6: Bridge: attach, daemon start, fake-daemon binary

Attachments are held by id (the frontend never sees sockets); output is read in a thread and handed to a callback, input is written raw, closing detaches without ending the session. `daemon::ensure_running` finds `mirante-daemon` without PATH (a GUI app does not inherit the shell's). The `mirante-fake-daemon` binary (feature `fake`) serves demo tasks for UI work and for the smoke test.

**Files:**
- Modify (fill): `crates/mirante-bridge/src/attach.rs`, `src/daemon.rs`, `src/bin/mirante-fake-daemon.rs`

- [ ] **Step 1: Create `crates/mirante-bridge/src/attach.rs`**

```rust
//! A task's live terminal: `attach` takes over a connection, then raw bytes flow both
//! ways. Output is read in a thread and handed to `on_output`; `on_end` runs once when the
//! stream ends (agent exited, daemon gone, or [`Attachments::close`]). Window size changes
//! go through `session.resize` (an RPC call), not through this connection.
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

use mirante_ipc::types::{method, AttachParams, AttachResult};
use mirante_ipc::Client;
use serde::Serialize;

use crate::rpc::BridgeError;

struct Attachment {
    writer: UnixStream,
    input: bool,
    reader: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Opened {
    pub id: u32,
    /// False for a read-only stream (Codex): typed bytes are dropped.
    pub input: bool,
}

/// Open attachments by id (the frontend holds ids, never sockets).
#[derive(Default)]
pub struct Attachments {
    next: AtomicU32,
    open: Mutex<HashMap<u32, Attachment>>,
}

impl Attachments {
    pub fn open(
        &self,
        socket: &Path,
        task: &str,
        rows: u16,
        cols: u16,
        mut on_output: impl FnMut(Vec<u8>) + Send + 'static,
        on_end: impl FnOnce() + Send + 'static,
    ) -> Result<Opened, BridgeError> {
        let mut c = Client::connect(socket).map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
        c.set_timeout(Some(std::time::Duration::from_secs(10)))
            .map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
        let r: AttachResult = c.call(method::ATTACH, AttachParams { task: task.into(), rows, cols })?;
        let (mut reader, writer) = c.into_raw();
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Ok(0) | Err(_) => break,
                    Ok(n) => on_output(buf[..n].to_vec()),
                }
            }
            on_end();
        });
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let a = Attachment { writer, input: r.input, reader: Some(thread) };
        self.open.lock().unwrap().insert(id, a);
        Ok(Opened { id, input: r.input })
    }

    /// Typed bytes. Ignored on a read-only stream; an unknown id is an error.
    pub fn write(&self, id: u32, bytes: &[u8]) -> Result<(), BridgeError> {
        let mut open = self.open.lock().unwrap();
        let a = open.get_mut(&id).ok_or_else(|| BridgeError::Protocol { message: format!("no attachment {id}") })?;
        if !a.input {
            return Ok(());
        }
        a.writer.write_all(bytes).map_err(|e| BridgeError::Disconnected { message: e.to_string() })
    }

    /// Detaches (the session keeps running). Unknown ids are ignored.
    pub fn close(&self, id: u32) {
        let a = self.open.lock().unwrap().remove(&id);
        if let Some(mut a) = a {
            let _ = a.writer.shutdown(Shutdown::Both);
            if let Some(t) = a.reader.take() {
                let _ = t.join();
            }
        }
    }

    pub fn close_all(&self) {
        let ids: Vec<u32> = self.open.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.close(id);
        }
    }

    pub fn len(&self) -> usize {
        self.open.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeDaemon;
    use mirante_ipc::test_dir::TestDir;
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::Duration;

    fn read_until(rx: &mpsc::Receiver<Vec<u8>>, want: &[u8]) -> Vec<u8> {
        let mut got = Vec::new();
        while !got.windows(want.len()).any(|w| w == want) {
            got.extend(rx.recv_timeout(Duration::from_secs(5)).expect("output"));
        }
        got
    }

    #[test]
    fn output_and_input_flow_until_close() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let all = Attachments::default();
        let (tx, rx) = mpsc::channel();
        let (end_tx, end_rx) = mpsc::channel();
        let o = all
            .open(&fake.socket, "t1", 30, 100, move |b| tx.send(b).unwrap(), move || end_tx.send(()).unwrap())
            .unwrap();
        assert_eq!(o, Opened { id: 1, input: true });
        assert_eq!(fake.calls()[0], ("attach".to_string(), json!({"task": "t1", "rows": 30, "cols": 100})));
        read_until(&rx, b"fake terminal\r\n");
        all.write(o.id, b"ls\r").unwrap();
        read_until(&rx, b"ls\r");
        all.close(o.id);
        end_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(all.is_empty());
        assert!(all.write(o.id, b"x").is_err());
    }

    #[test]
    fn the_stream_ending_on_the_daemon_side_calls_on_end() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let all = Attachments::default();
        let (end_tx, end_rx) = mpsc::channel();
        all.open(&fake.socket, "t1", 24, 80, |_| {}, move || end_tx.send(()).unwrap()).unwrap();
        fake.drop_connections();
        end_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        all.close_all();
    }
}
```

- [ ] **Step 2: Create `crates/mirante-bridge/src/daemon.rs`**

```rust
//! Finding and starting `mirante-daemon` from the desktop app. A GUI app does not inherit
//! the shell's PATH, so the binary is looked up in a fixed list.
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use mirante_ipc::Paths;

/// `MIRANTE_DAEMON_BIN`, else the first executable `mirante-daemon` in `~/.cargo/bin`,
/// `/opt/homebrew/bin`, `/usr/local/bin`.
pub fn find_daemon_bin(env_bin: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = env_bin.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".cargo/bin"));
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.into_iter().map(|d| d.join("mirante-daemon")).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Starts the daemon (detached, logging to `~/.mirante/daemon.log`) unless one answers.
pub fn ensure_running(paths: &Paths) -> Result<(), String> {
    if mirante_ipc::Client::connect(&paths.socket()).is_ok() {
        return Ok(());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let bin = find_daemon_bin(std::env::var_os("MIRANTE_DAEMON_BIN"), home.as_deref()).ok_or_else(|| {
        "mirante-daemon not found: install it (cargo install --path crates/mirante-daemon) or set MIRANTE_DAEMON_BIN"
            .to_string()
    })?;
    mirante_cli::daemon_ctl::start_daemon_with(paths, &bin).map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn env_wins_then_cargo_bin_when_executable() {
        let d = TestDir::new();
        assert_eq!(find_daemon_bin(Some("/x/daemon".into()), Some(d.path())), Some(PathBuf::from("/x/daemon")));
        let bin = d.path().join(".cargo/bin/mirante-daemon");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_ne!(find_daemon_bin(Some("".into()), Some(d.path())), Some(bin.clone()));
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_daemon_bin(None, Some(d.path())), Some(bin));
    }
}
```

- [ ] **Step 3: Create `crates/mirante-bridge/src/bin/mirante-fake-daemon.rs`**

```rust
//! `mirante-fake-daemon <socket>`: the scripted fake daemon with demo tasks, for working on
//! the cockpit UI without agents and for the app smoke test. Prints `call <method>` for
//! every request it receives. Not a daemon: it never starts agents or touches git.
use std::path::PathBuf;
use std::time::Duration;

use mirante_bridge::fake::{task_json, FakeDaemon};
use serde_json::json;

fn main() {
    let Some(socket) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: mirante-fake-daemon <socket path>");
        std::process::exit(2);
    };
    let fake = FakeDaemon::start(&socket);
    let mut waiting = task_json("t-codex-1", "/repos/shop", "fix/cart", "codex", "needs_you");
    waiting["needs_you"] = json!(1);
    let mut done = task_json("t-claude-2", "/repos/shop", "feat/search", "claude", "idle");
    done["live"] = json!(false);
    done["boot"] = json!(null);
    fake.set_tasks(vec![task_json("t-claude-1", "/repos/mirante", "feat/login", "claude", "working"), waiting, done]);
    fake.push_event(Some("t-claude-2"), "turn_started", json!({"idx": 1}));
    fake.push_event(Some("t-claude-2"), "turn_ended", json!({"idx": 1}));
    fake.push_event(
        Some("t-codex-1"),
        "permission_requested",
        json!({"permission_id": "p-1", "tool": "command", "detail": "npm install"}),
    );
    println!("fake daemon on {}", socket.display());
    let mut seen = 0;
    let mut n = 0u64;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let calls = fake.methods();
        for m in &calls[seen..] {
            println!("call {m}");
        }
        seen = calls.len();
        n += 1;
        if n.is_multiple_of(10) {
            let step = json!({"type": "tool", "tool": "Bash", "summary": format!("cargo test (run {})", n / 10), "ok": true, "agent": null});
            fake.push_event(Some("t-claude-1"), "step", step);
        }
    }
}
```

- [ ] **Step 4: Run and confirm**

Run: `cargo test -p mirante-bridge`

Expected: exit 0; output contains `test result: ok. 12 passed`

- [ ] **Step 5: Run and confirm**

Run: `cargo clippy --workspace --all-targets --features mirante-bridge/fake -- -D warnings`

Expected: exit 0; output contains `Finished`

- [ ] **Step 6: Commit**

```bash
git add crates/mirante-bridge
git commit -F- <<'EOF'
feat(bridge): attach by id, daemon lookup/start, scripted fake daemon binary

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 7: Cockpit scaffold and daemon transport

The frontend package (React 19, Vite, TypeScript, Vitest + Testing Library) and its only Tauri-facing module, `lib/daemon.ts`: `rpc`, `subscribe`, attach calls, typed `DaemonError`. `lib/protocol.ts` mirrors the wire types by hand. jsdom is pinned to 29 because jsdom 30+ needs Node 22.22+.

**Files:**
- Create: `apps/cockpit/package.json`
- Create: `apps/cockpit/tsconfig.json`
- Create: `apps/cockpit/vite.config.ts`
- Create: `apps/cockpit/index.html`
- Create: `apps/cockpit/.gitignore`
- Create: `apps/cockpit/src/vite-env.d.ts`
- Create: `apps/cockpit/src/test-setup.ts`
- Create: `apps/cockpit/src/main.tsx`
- Create: `apps/cockpit/src/lib/protocol.ts`
- Create: `apps/cockpit/src/lib/daemon.ts`
- Create: `apps/cockpit/src/lib/daemon.test.ts`

- [ ] **Step 1: Create `apps/cockpit/package.json`**

```json
{
  "name": "mirante-cockpit",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "license": "Apache-2.0",
  "engines": {
    "node": ">=22.12"
  },
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build",
    "test": "vitest run",
    "typecheck": "tsc --noEmit",
    "tauri": "tauri"
  },
  "dependencies": {
    "@tauri-apps/api": "^2.12.4",
    "@tauri-apps/plugin-notification": "^2.8.0",
    "@tauri-apps/plugin-opener": "^2.7.2",
    "@xterm/addon-fit": "^0.11.0",
    "@xterm/xterm": "^6.0.0",
    "react": "^19.3.1",
    "react-dom": "^19.3.1"
  },
  "devDependencies": {
    "@tauri-apps/cli": "^2.12.4",
    "@testing-library/jest-dom": "^7.0.1",
    "@testing-library/react": "^16.3.3",
    "@testing-library/user-event": "^14.6.8",
    "@types/react": "^19.3.0",
    "@types/react-dom": "^19.3.0",
    "@vitejs/plugin-react": "^6.2.1",
    "jsdom": "^29.1.1",
    "typescript": "^7.0.2",
    "vite": "^8.3.6",
    "vitest": "^5.1.0"
  }
}
```

- [ ] **Step 2: Create `apps/cockpit/tsconfig.json`**

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2023", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "moduleResolution": "bundler",
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "isolatedModules": true,
    "resolveJsonModule": true,
    "skipLibCheck": true,
    "types": ["vitest/globals", "@testing-library/jest-dom"]
  },
  "include": ["src", "vite.config.ts"]
}
```

- [ ] **Step 3: Create `apps/cockpit/vite.config.ts`**

```ts
/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves the built files itself; in dev it loads this server (fixed port, no LAN).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  // A desktop app loads its bundle from disk: one chunk is fine.
  build: { target: "safari16", outDir: "dist", sourcemap: false, chunkSizeWarningLimit: 2000 },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
    css: false,
  },
});
```

- [ ] **Step 4: Create `apps/cockpit/index.html`**

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Mirante</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
```

- [ ] **Step 5: Create `apps/cockpit/.gitignore`**

```
node_modules/
dist/
src-tauri/target/
src-tauri/gen/
```

- [ ] **Step 6: Create `apps/cockpit/src/vite-env.d.ts`**

```ts
/// <reference types="vite/client" />
```

- [ ] **Step 7: Create `apps/cockpit/src/test-setup.ts`**

```ts
import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

afterEach(() => cleanup());
```

- [ ] **Step 8: Create `apps/cockpit/src/main.tsx`**

```tsx
import { createRoot } from "react-dom/client";

// Replaced by the real entry in Task 18; enough for the first Tauri build.
createRoot(document.getElementById("root")!).render(<p>Mirante cockpit</p>);
```

- [ ] **Step 9: Run and confirm**

Run: `pnpm --dir apps/cockpit install`

Expected: exit 0; output contains `Done in`

- [ ] **Step 10: Create `apps/cockpit/src/lib/daemon.test.ts`**

```ts
import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage: (m: unknown) => void = () => {};
  },
}));

import { attachOpen, DaemonError, rpc } from "./daemon";
import { RpcCode } from "./protocol";

describe("daemon transport", () => {
  // Braces matter: a function returned from beforeEach is run as a cleanup hook.
  beforeEach(() => {
    invoke.mockClear();
  });

  it("rpc passes method and params to the bridge command", async () => {
    invoke.mockImplementation(async () => [1]);
    await expect(rpc("task.list")).resolves.toEqual([1]);
    expect(invoke).toHaveBeenCalledWith("rpc", { method: "task.list", params: {} });
  });

  it("bridge errors become DaemonError with the daemon code", async () => {
    invoke.mockImplementation(async () => {
      throw { kind: "rpc", code: RpcCode.UNSUPPORTED, message: "codex cannot resize" };
    });
    const e = (await rpc("session.resize").catch((x: unknown) => x)) as DaemonError;
    expect(e).toBeInstanceOf(DaemonError);
    expect([e.code, e.unsupported, e.message]).toEqual([3, true, "codex cannot resize"]);
  });

  it("attach decodes base64 chunks and reports the end", async () => {
    let channel: { onmessage: (m: unknown) => void } | undefined;
    invoke.mockImplementation(async (_cmd: string, args: { onMsg: typeof channel }) => {
      channel = args.onMsg;
      return { id: 7, input: true };
    });
    const chunks: string[] = [];
    let ended = false;
    const a = await attachOpen("t1", 24, 80, (b) => chunks.push(new TextDecoder().decode(b)), () => (ended = true));
    channel!.onmessage({ type: "data", b64: btoa("héllo".replace("é", "e")) });
    channel!.onmessage({ type: "end" });
    expect([a, chunks, ended]).toEqual([{ id: 7, input: true }, ["hello"], true]);
  });
});
```

- [ ] **Step 11: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/lib/daemon.test.ts`

Expected: FAIL — Failed to resolve import "./daemon"

- [ ] **Step 12: Create `apps/cockpit/src/lib/protocol.ts`**

```ts
// Wire types of the daemon protocol (crates/mirante-ipc/src/types.rs and
// crates/mirante-core/src/model.rs), as the bridge forwards them. Keep in sync by hand.

export type AgentKind = "claude" | "codex";
export type Status = "idle" | "working" | "needs_you" | "ended";
export type Lifecycle = "created" | "active" | "archiving" | "archived";
export type BootState = "booting" | "trust_prompt" | "ready" | "exited";
export type SendMode = "steer" | "queue";
export type PermissionStatus = "pending" | "allowed_once" | "denied";

export interface TaskView {
  id: string;
  title: string;
  repo_path: string;
  worktree_path: string;
  branch: string;
  agent: AgentKind;
  goal: string;
  notes: string;
  lifecycle: Lifecycle;
  status: Status;
  created_at: number;
  updated_at: number;
  live: boolean;
  boot: BootState | null;
  needs_you: number;
  queued: number;
  detail: string | null;
  /** Codex quota / token usage while live, e.g. "5h 42% · week 9% · 31k tokens". */
  usage?: string | null;
  /** Memory of the live session's process tree, once sampled (absent from older daemons). */
  memory?: MemView | null;
  /** Hibernated to free memory; `session.wake` resumes it. */
  hibernated?: boolean;
}

export interface MemView {
  bytes: number;
  peak: number;
  budget: number;
  /** "ok", "soft" or "hard". */
  state: string;
}

/** `mirante_ipc::types::SystemView`: the machine's memory as the daemon's guard sees it. */
export interface SystemView {
  /** "normal", "elevated" or "critical". */
  level: string;
  total: number;
  free: number;
  swap_used: number;
  swap_total: number;
  paused: boolean;
}

export interface DaemonEvent {
  seq: number;
  boot_id: string;
  task_id: string | null;
  kind: string;
  payload: Record<string, unknown>;
  ts_ms: number;
}

export interface Snapshot {
  boot_id: string;
  seq: number;
  tasks: TaskView[];
  resumed: boolean;
}

export interface PermissionView {
  id: string;
  task_id: string;
  session_id: string;
  tool: string;
  detail: string;
  status: PermissionStatus;
  created_at: number;
  answered_at: number | null;
  task_title: string;
  branch: string;
}

export interface DiffFile {
  path: string;
  added: number | null;
  removed: number | null;
}

export interface TaskDiff {
  base: string;
  merge_base: string;
  files: DiffFile[];
  untracked: string[];
  patch: string;
  truncated: boolean;
  compare_url: string | null;
  pushed: boolean;
}

export interface DaemonStatus {
  pid: number;
  boot_id: string;
  version: string;
  live_sessions: number;
  /** The memory guard's view (absent from older daemons and before the first sample). */
  memory?: SystemView | null;
}

export interface StartResult {
  task: string;
  boot: BootState;
  resumed: boolean;
  screen: string;
}

export interface SendResult {
  delivered: SendMode;
  queued: number;
}

export type StreamMsg =
  | { type: "connected"; snapshot: Snapshot }
  | { type: "event"; event: DaemonEvent }
  | { type: "disconnected"; error: string; retry_ms: number };

export type BridgeError =
  | { kind: "disconnected"; message: string }
  | { kind: "rpc"; code: number; message: string }
  | { kind: "protocol"; message: string }
  | { kind: "forbidden"; method: string };

/** `mirante_ipc::rpc::code`. */
export const RpcCode = {
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  NOT_FOUND: 1,
  CONFLICT: 2,
  UNSUPPORTED: 3,
  DRIVER: 4,
  NEEDS_YOU: 5,
} as const;

/** `mirante_ipc::types::method` (the ones the cockpit may call). */
export const M = {
  taskCreate: "task.create",
  taskList: "task.list",
  taskGet: "task.get",
  taskArchive: "task.archive",
  taskEvents: "task.events",
  taskDiff: "task.diff",
  sessionStart: "session.start",
  sessionResume: "session.resume",
  sessionTrust: "session.trust",
  sessionStop: "session.stop",
  sessionInterrupt: "session.interrupt",
  sessionResize: "session.resize",
  messageSend: "message.send",
  permissionList: "permission.list",
  permissionAnswer: "permission.answer",
  daemonStatus: "daemon.status",
  sessionHibernate: "session.hibernate",
  sessionWake: "session.wake",
} as const;
```

- [ ] **Step 13: Create `apps/cockpit/src/lib/daemon.ts`**

```ts
// The only module that talks to Tauri. Everything else gets these functions injected
// (store/cockpit.ts) or imports this module (and tests mock it).
import { Channel, invoke } from "@tauri-apps/api/core";
import type { BridgeError, StreamMsg } from "./protocol";
import { RpcCode } from "./protocol";

export class DaemonError extends Error {
  readonly info: BridgeError;
  constructor(info: BridgeError) {
    super(describe(info));
    this.info = info;
  }
  /** The daemon's error code, or null when the call never got an answer. */
  get code(): number | null {
    return this.info.kind === "rpc" ? this.info.code : null;
  }
  get unsupported(): boolean {
    return this.code === RpcCode.UNSUPPORTED || this.code === RpcCode.METHOD_NOT_FOUND;
  }
}

function describe(e: BridgeError): string {
  switch (e.kind) {
    case "disconnected":
      return `daemon not reachable: ${e.message}`;
    case "rpc":
      return e.message;
    case "protocol":
      return `daemon protocol: ${e.message}`;
    case "forbidden":
      return `${e.method} is not allowed from the cockpit`;
  }
}

export function toDaemonError(e: unknown): DaemonError {
  if (e instanceof DaemonError) return e;
  if (e && typeof e === "object" && "kind" in e) return new DaemonError(e as BridgeError);
  return new DaemonError({ kind: "protocol", message: String(e) });
}

export async function rpc<T>(method: string, params: object = {}): Promise<T> {
  try {
    return await invoke<T>("rpc", { method, params });
  } catch (e) {
    throw toDaemonError(e);
  }
}

/** (Re)starts the bridge's event stream; messages arrive in order on `onMsg`. */
export async function subscribe(onMsg: (m: StreamMsg) => void): Promise<void> {
  const ch = new Channel<StreamMsg>();
  ch.onmessage = onMsg;
  await invoke("subscribe", { onMsg: ch });
}

export type AttachMsg = { type: "data"; b64: string } | { type: "end" };

export interface Attached {
  id: number;
  input: boolean;
}

function fromBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export async function attachOpen(
  task: string,
  rows: number,
  cols: number,
  onData: (bytes: Uint8Array) => void,
  onEnd: () => void,
): Promise<Attached> {
  const ch = new Channel<AttachMsg>();
  ch.onmessage = (m) => (m.type === "data" ? onData(fromBase64(m.b64)) : onEnd());
  try {
    return await invoke<Attached>("attach_open", { task, rows, cols, onMsg: ch });
  } catch (e) {
    throw toDaemonError(e);
  }
}

export async function attachWrite(id: number, text: string): Promise<void> {
  const data = Array.from(new TextEncoder().encode(text));
  await invoke("attach_write", { id, data }).catch((e) => {
    throw toDaemonError(e);
  });
}

export async function attachClose(id: number): Promise<void> {
  await invoke("attach_close", { id });
}

/** Starts `mirante-daemon` if nothing answers on the socket. Rejects with a message. */
export async function daemonStart(): Promise<void> {
  await invoke("daemon_start");
}
```

- [ ] **Step 14: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/lib/daemon.test.ts && pnpm --dir apps/cockpit typecheck`

Expected: exit 0; output contains `Tests  3 passed (3)`

- [ ] **Step 15: Commit**

```bash
git add apps/cockpit/package.json apps/cockpit/pnpm-lock.yaml apps/cockpit/tsconfig.json apps/cockpit/vite.config.ts apps/cockpit/index.html apps/cockpit/.gitignore apps/cockpit/src
git commit -F- <<'EOF'
feat(cockpit): frontend scaffold and the daemon transport

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 8: Tauri shell, CSP and capabilities

The Rust side is six commands over `mirante-bridge`: `rpc`, `subscribe` (Channel of `StreamMsg`), `attach_open` (Channel of base64 chunks + end), `attach_write`, `attach_close`, `daemon_start`. It is its own Cargo workspace so the repo's `cargo test --workspace` never compiles Tauri/WebKit. Locked down: CSP with no remote origins and no eval, no global `__TAURI__`, capabilities = notifications + opening `https://github.com/*` only (no fs/shell/http plugins). `security.test.ts` pins that. Do not enable `freezePrototype`: with it on, the bundle never ran (no IPC call reached the bridge) — found while verifying this plan.

**Files:**
- Create: `apps/cockpit/src-tauri/Cargo.toml`
- Create: `apps/cockpit/src-tauri/build.rs`
- Create: `apps/cockpit/src-tauri/src/main.rs`
- Create: `apps/cockpit/src-tauri/src/lib.rs`
- Create: `apps/cockpit/src-tauri/capabilities/default.json`
- Create: `apps/cockpit/src-tauri/tauri.conf.json`
- Create: `apps/cockpit/app-icon.svg`
- Create: `apps/cockpit/src-tauri/icons/*`
- Create: `apps/cockpit/src/security.test.ts`

- [ ] **Step 1: Create `apps/cockpit/src/security.test.ts`**

```ts
// The desktop shell's attack surface, pinned: a CSP with no remote origins or eval, and
// capabilities limited to notifications plus opening github.com pages. Widening either
// must be a deliberate edit of this test.
import { describe, expect, it } from "vitest";
import capabilities from "../src-tauri/capabilities/default.json";
import conf from "../src-tauri/tauri.conf.json";

describe("Tauri security baseline", () => {
  const csp = conf.app.security.csp;
  const directive = (name: string) =>
    csp
      .split(";")
      .map((d) => d.trim())
      .find((d) => d.startsWith(`${name} `)) ?? "";

  it("loads code only from the app and talks only to the IPC bridge", () => {
    expect(directive("default-src")).toBe("default-src 'self'");
    expect(directive("script-src")).toBe("script-src 'self'");
    expect(directive("connect-src")).toBe("connect-src ipc: http://ipc.localhost");
    expect(csp).not.toMatch(/unsafe-eval|https?:\/\/(?!ipc\.localhost)|\*/);
    expect(directive("object-src")).toBe("object-src 'none'");
  });

  it("the dev policy only adds what Vite's dev server needs", () => {
    const dev = conf.app.security.devCsp;
    expect(dev.replace("script-src 'self' 'unsafe-inline'", "script-src 'self'").replace(" ws://127.0.0.1:1420", "")).toBe(csp);
  });

  it("no global Tauri object, no remote content", () => {
    expect(conf.app.withGlobalTauri).toBe(false);
    expect(conf.build.devUrl).toBe("http://127.0.0.1:1420");
  });

  it("grants only notifications and opening github.com URLs", () => {
    const ids = capabilities.permissions.map((p) => (typeof p === "string" ? p : p.identifier));
    expect(ids).toEqual(["notification:default", "opener:allow-open-url"]);
    const opener = capabilities.permissions.find((p) => typeof p !== "string");
    expect(opener).toEqual({ identifier: "opener:allow-open-url", allow: [{ url: "https://github.com/*" }] });
    expect(capabilities.windows).toEqual(["main"]);
  });
});
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/security.test.ts`

Expected: FAIL — Failed to resolve import "../src-tauri/capabilities/default.json"

- [ ] **Step 3: Create `apps/cockpit/src-tauri/Cargo.toml`**

```toml
[package]
name = "mirante-cockpit"
version = "0.1.0"
edition = "2021"
publish = false
license = "Apache-2.0"
description = "Mirante desktop cockpit (Tauri shell over mirante-bridge)."

# Its own workspace: `cargo test --workspace` at the repo root stays free of Tauri/WebKit.
[workspace]

[lib]
name = "mirante_cockpit_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
mirante-bridge = { path = "../../../crates/mirante-bridge" }
mirante-ipc = { path = "../../../crates/mirante-ipc" }
tauri = { version = "2", features = [] }
tauri-plugin-notification = "2"
tauri-plugin-opener = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
base64 = "0.22"
```

- [ ] **Step 4: Create `apps/cockpit/src-tauri/build.rs`**

```rust
fn main() {
    tauri_build::build()
}
```

- [ ] **Step 5: Create `apps/cockpit/src-tauri/src/main.rs`**

```rust
// No console window on Windows release builds (macOS/Linux ignore this).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    mirante_cockpit_lib::run()
}
```

- [ ] **Step 6: Create `apps/cockpit/src-tauri/src/lib.rs`**

```rust
//! Tauri shell of the cockpit: six commands that forward to `mirante-bridge`. No state of
//! its own beyond the running stream and open attachments; the daemon owns everything.
use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine;
use mirante_bridge::attach::{Attachments, Opened};
use mirante_bridge::stream::{self, Stream, StreamMsg};
use mirante_bridge::{rpc as bridge_rpc, BridgeError};
use mirante_ipc::Paths;
use serde::Serialize;
use serde_json::Value;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, WindowEvent};

struct Cockpit {
    paths: Paths,
    socket: PathBuf,
    stream: Mutex<Option<Stream>>,
    attachments: Attachments,
}

/// Terminal output for the frontend: base64 keeps binary-safe bytes cheap over JSON IPC.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AttachMsg {
    Data { b64: String },
    End,
}

fn joined<T>(r: Result<Result<T, BridgeError>, tauri::Error>) -> Result<T, BridgeError> {
    r.map_err(|e| BridgeError::Protocol { message: e.to_string() })?
}

#[tauri::command]
async fn rpc(app: AppHandle, method: String, params: Value) -> Result<Value, BridgeError> {
    let socket = app.state::<Cockpit>().socket.clone();
    joined(tauri::async_runtime::spawn_blocking(move || bridge_rpc::call(&socket, &method, params)).await)
}

/// (Re)starts the event stream into `on_msg`; a previous stream is stopped.
#[tauri::command]
fn subscribe(app: State<'_, Cockpit>, on_msg: Channel<StreamMsg>) {
    let s = stream::spawn(app.socket.clone(), move |m| {
        let _ = on_msg.send(m);
    });
    let old = app.stream.lock().unwrap().replace(s);
    if let Some(old) = old {
        std::thread::spawn(move || old.stop());
    }
}

#[tauri::command]
async fn attach_open(
    app: AppHandle,
    task: String,
    rows: u16,
    cols: u16,
    on_msg: Channel<AttachMsg>,
) -> Result<Opened, BridgeError> {
    joined(
        tauri::async_runtime::spawn_blocking(move || {
            let c = app.state::<Cockpit>();
            let out = on_msg.clone();
            c.attachments.open(
                &c.socket,
                &task,
                rows,
                cols,
                move |bytes| {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                    let _ = out.send(AttachMsg::Data { b64 });
                },
                move || {
                    let _ = on_msg.send(AttachMsg::End);
                },
            )
        })
        .await,
    )
}

#[tauri::command]
fn attach_write(app: State<'_, Cockpit>, id: u32, data: Vec<u8>) -> Result<(), BridgeError> {
    app.attachments.write(id, &data)
}

#[tauri::command]
async fn attach_close(app: AppHandle, id: u32) -> Result<(), BridgeError> {
    joined(
        tauri::async_runtime::spawn_blocking(move || {
            app.state::<Cockpit>().attachments.close(id);
            Ok(())
        })
        .await,
    )
}

#[tauri::command]
async fn daemon_start(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || mirante_bridge::daemon::ensure_running(&app.state::<Cockpit>().paths))
        .await
        .map_err(|e| e.to_string())?
}

pub fn run() {
    let paths = Paths::from_env().expect("cannot locate ~/.mirante (is HOME set?)");
    let socket = paths.socket();
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Cockpit { paths, socket, stream: Mutex::new(None), attachments: Attachments::default() })
        .invoke_handler(tauri::generate_handler![rpc, subscribe, attach_open, attach_write, attach_close, daemon_start])
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                window.state::<Cockpit>().attachments.close_all();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running the Mirante cockpit");
}
```

- [ ] **Step 7: Create `apps/cockpit/src-tauri/capabilities/default.json`**

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "The cockpit window: native notifications, and opening GitHub pull-request pages in the browser. Nothing else (no fs, shell, http).",
  "windows": ["main"],
  "permissions": [
    "notification:default",
    {
      "identifier": "opener:allow-open-url",
      "allow": [{ "url": "https://github.com/*" }]
    }
  ]
}
```

- [ ] **Step 8: Create `apps/cockpit/src-tauri/tauri.conf.json`**

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Mirante",
  "version": "0.1.0",
  "identifier": "dev.mirante.cockpit",
  "build": {
    "frontendDist": "../dist",
    "devUrl": "http://127.0.0.1:1420",
    "beforeDevCommand": "pnpm dev",
    "beforeBuildCommand": "pnpm build"
  },
  "app": {
    "withGlobalTauri": false,
    "windows": [
      {
        "label": "main",
        "title": "Mirante",
        "width": 1360,
        "height": 860,
        "minWidth": 960,
        "minHeight": 600
      }
    ],
    "security": {
      "csp": "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
      "devCsp": "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src ipc: http://ipc.localhost ws://127.0.0.1:1420; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
      "dangerousDisableAssetCspModification": ["style-src"]
    }
  },
  "bundle": {
    "active": true,
    "targets": ["app"],
    "category": "DeveloperTool",
    "icon": ["icons/32x32.png", "icons/128x128.png", "icons/128x128@2x.png", "icons/icon.icns", "icons/icon.png"],
    "macOS": { "minimumSystemVersion": "12.0" }
  }
}
```

- [ ] **Step 9: Create `apps/cockpit/app-icon.svg`**

```xml
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <rect width="1024" height="1024" rx="224" fill="#12161c"/>
  <circle cx="512" cy="512" r="300" fill="none" stroke="#5eead4" stroke-width="56"/>
  <circle cx="512" cy="512" r="96" fill="#5eead4"/>
  <path d="M512 92v160M512 772v160M92 512h160M772 512h160" stroke="#5eead4" stroke-width="40" stroke-linecap="round"/>
</svg>
```

- [ ] **Step 10: Run**

```bash
cd apps/cockpit && pnpm tauri icon app-icon.svg && rm -rf src-tauri/icons/android src-tauri/icons/ios src-tauri/icons/Square* src-tauri/icons/StoreLogo.png src-tauri/icons/64x64.png src-tauri/icons/icon.ico
```

- [ ] **Step 11: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/security.test.ts`

Expected: exit 0; output contains `Tests  4 passed (4)`

- [ ] **Step 12: Run and confirm**

Run: `pnpm --dir apps/cockpit build && cargo build --manifest-path apps/cockpit/src-tauri/Cargo.toml && cargo clippy --manifest-path apps/cockpit/src-tauri/Cargo.toml --all-targets -- -D warnings`

Expected: exit 0; output contains `Finished`

- [ ] **Step 13: Commit**

```bash
git add apps/cockpit/src-tauri apps/cockpit/app-icon.svg apps/cockpit/src/security.test.ts
git commit -F- <<'EOF'
feat(cockpit): tauri shell over mirante-bridge with locked CSP and minimal capabilities

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 9: Client store: reducer over snapshot + events

One pure reducer. Live events are ignored when `seq <= state.seq` on the same boot (replays), accepted from a new boot; task rows guessed from events are later overwritten by `task.get` / `task.list` (the daemon is the authority). Feed lines dedupe by `seq` so history and live events merge. Permission cards keep the `seq` of their last change, so an older history page never reopens an answered card. `lib/memguard.ts` holds every memory-guard name the UI reads (`TaskView.memory` / `hibernated`, `daemon.status.memory`, `session.hibernate` / `session.wake`, `memory.*` events, all at HEAD from Plan 2b).

**Files:**
- Create: `apps/cockpit/src/test/fixtures.ts`
- Create: `apps/cockpit/src/lib/memguard.ts`
- Create: `apps/cockpit/src/store/state.ts`
- Create: `apps/cockpit/src/store/state.test.ts`

- [ ] **Step 1: Create `apps/cockpit/src/test/fixtures.ts`**

```ts
import type { DaemonEvent, Snapshot, TaskView } from "../lib/protocol";

export function task(id: string, over: Partial<TaskView> = {}): TaskView {
  return {
    id,
    title: `goal of ${id}`,
    repo_path: "/repos/shop",
    worktree_path: `/wt/${id}`,
    branch: `feat/${id}`,
    agent: "claude",
    goal: `goal of ${id}`,
    notes: "",
    lifecycle: "active",
    status: "idle",
    created_at: 1,
    updated_at: 1,
    live: true,
    boot: "ready",
    needs_you: 0,
    queued: 0,
    detail: null,
    ...over,
  };
}

export function snapshot(tasks: TaskView[], seq = 0, over: Partial<Snapshot> = {}): Snapshot {
  return { boot_id: "b1", seq, tasks, resumed: false, ...over };
}

let ts = 1_000;
export function ev(seq: number, task_id: string | null, kind: string, payload: Record<string, unknown> = {}, boot_id = "b1"): DaemonEvent {
  return { seq, boot_id, task_id, kind, payload, ts_ms: ts++ };
}

/** The stored task as `task_created` carries it (no live facts). */
export function taskRow(id: string, over: Partial<TaskView> = {}): Record<string, unknown> {
  const row: Record<string, unknown> = { ...task(id, over) };
  for (const k of ["live", "boot", "needs_you", "queued", "detail"]) delete row[k];
  return row;
}
```

- [ ] **Step 2: Create `apps/cockpit/src/store/state.test.ts`**

```ts
import { describe, expect, it } from "vitest";
import { ev, snapshot, task, taskRow } from "../test/fixtures";
import { feedItem, initialState, reduce, type State } from "./state";

const connected = (s: State = initialState, tasks = [task("t1")], seq = 5) =>
  reduce(s, { type: "connected", snapshot: snapshot(tasks, seq) });

describe("reduce", () => {
  it("a full snapshot replaces the tasks and sets the resume point", () => {
    const s = connected();
    expect(s.connection).toEqual({ state: "live" });
    expect([s.bootId, s.seq, Object.keys(s.tasks)]).toEqual(["b1", 5, ["t1"]]);
  });

  it("a resumed snapshot keeps the state", () => {
    const s = connected();
    const r = reduce(s, { type: "connected", snapshot: snapshot([], 5, { resumed: true }) });
    expect(r.tasks).toBe(s.tasks);
  });

  it("events apply in seq order and replays are ignored", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "status", { from: "idle", to: "working" }) });
    expect(s.tasks.t1.status).toBe("working");
    const again = reduce(s, { type: "event", event: ev(6, "t1", "status", { from: "working", to: "idle" }) });
    expect(again).toBe(s);
    const old = reduce(s, { type: "event", event: ev(3, "t1", "status", { to: "ended" }) });
    expect(old).toBe(s);
  });

  it("an event from a new boot is applied even with a lower seq", () => {
    const s = reduce(connected(), { type: "event", event: ev(2, "t1", "status", { to: "working" }, "b2") });
    expect([s.bootId, s.seq, s.tasks.t1.status]).toEqual(["b2", 2, "working"]);
  });

  it("task_created adds a row; refreshed views overwrite guesses", () => {
    let s = connected(initialState, []);
    s = reduce(s, { type: "event", event: ev(6, "t2", "task_created", taskRow("t2")) });
    expect(s.tasks.t2.live).toBe(false);
    s = reduce(s, { type: "task", view: task("t2", { needs_you: 3 }) });
    expect(s.tasks.t2.needs_you).toBe(3);
  });

  it("permission cards follow requested -> answered and never go back", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "rm -rf build" }) });
    expect(s.perms.p1).toMatchObject({ status: "pending", tool: "Bash", task_id: "t1" });
    expect(s.tasks.t1.needs_you).toBe(1);
    s = reduce(s, { type: "event", event: ev(7, "t1", "permission_answered", { permission_id: "p1", answer: "allowed_once" }) });
    expect(s.perms.p1.status).toBe("allowed_once");
    // An older history page replaying the request does not reopen it.
    s = reduce(s, { type: "history", taskId: "t1", events: [ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "Bash" })] });
    expect(s.perms.p1.status).toBe("allowed_once");
  });

  it("permission.list marks pending cards it no longer lists as expired", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "Bash" }) });
    s = reduce(s, { type: "permissions", list: [] });
    expect(s.perms.p1.status).toBe("expired");
  });

  it("history merges into the feed without duplicates, sorted by seq", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(9, "t1", "step", { type: "tool", tool: "Bash", summary: "ls", ok: true }) });
    s = reduce(s, {
      type: "history",
      taskId: "t1",
      events: [ev(7, "t1", "turn_started"), ev(9, "t1", "step", { type: "tool", tool: "Bash", summary: "ls" })],
    });
    expect(s.feed.t1.map((i) => [i.seq, i.kind])).toEqual([
      [7, "turn"],
      [9, "tool"],
    ]);
  });

  it("disconnects keep the data and record the retry", () => {
    const s = reduce(connected(), { type: "disconnected", error: "gone", retryMs: 500 });
    expect(s.connection).toEqual({ state: "down", error: "gone", retryMs: 500 });
    expect(s.tasks.t1).toBeDefined();
  });
});

describe("feedItem", () => {
  it("normalizes Claude and Codex steps", () => {
    expect(feedItem(ev(1, "t", "step", { type: "tool", tool: "Edit", summary: "src/a.rs", ok: false }))).toMatchObject({ kind: "tool", text: "Edit src/a.rs", ok: false });
    expect(feedItem(ev(2, "t", "step", { type: "command", command: "npm test", exit_code: 0 }))).toMatchObject({ kind: "command", text: "$ npm test", ok: true });
    expect(feedItem(ev(3, "t", "step", { type: "agent_message", text: "done" }))).toMatchObject({ kind: "agent_message", text: "done" });
    expect(feedItem(ev(4, "t", "step", { type: "steer_fell_back", error: "no turn" }))).toMatchObject({ kind: "system", text: "steer_fell_back: no turn" });
  });

  it("shows the human's messages with their mode and the delivery verdict", () => {
    expect(feedItem(ev(1, "t", "message_queued", { text: "also docs" }))).toMatchObject({ kind: "message", mode: "queue" });
    expect(feedItem(ev(2, "t", "message_sent", { mode: "steer", text: "stop" }))).toMatchObject({ kind: "message", mode: "steer", text: "stop" });
    expect(feedItem(ev(3, "t", "message_delivery", { delivery: "steered" }))?.text).toBe("message steered");
    expect(feedItem(ev(4, "t", "hook_orphan"))).toBeNull();
  });

  it("memory guard events become system lines", () => {
    expect(feedItem(ev(1, "t", "memory.warn", { state: "soft", bytes: 3 * 2 ** 30, budget: 4 * 2 ** 30 }))?.text).toBe("memory soft (3072 MB of 4096 MB)");
  });
});
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/store/state.test.ts`

Expected: FAIL — Failed to resolve import "./state"

- [ ] **Step 4: Create `apps/cockpit/src/lib/memguard.ts`**

```ts
// The daemon's memory guard as the UI sees it (Plan 2b Tasks 18-19): per-session memory on
// TaskView (`memory`, `hibernated`), the machine's view on `daemon.status.memory`, the
// `session.hibernate` / `session.wake` calls and the `memory.*` events. An older daemon has
// none of these: the panel then says "not available" and the actions answer
// METHOD_NOT_FOUND, which the UI reports instead of failing.
import type { DaemonStatus, TaskView } from "./protocol";
import { M } from "./protocol";

export const HIBERNATE = M.sessionHibernate;
export const WAKE = M.sessionWake;

export type Pressure = "normal" | "elevated" | "critical";

export interface SystemMemory {
  level: Pressure;
  /** New sessions and queued deliveries wait (level elevated or worse). */
  paused: boolean;
  total: number;
  free: number;
  swapUsed: number;
}

export function systemMemory(status: DaemonStatus | null): SystemMemory | null {
  const m = status?.memory;
  if (!m) return null;
  const level: Pressure = m.level === "elevated" || m.level === "critical" ? m.level : "normal";
  return { level, paused: m.paused, total: m.total, free: m.free, swapUsed: m.swap_used };
}

export interface TaskMemory {
  bytes: number;
  budget: number;
  state: "ok" | "soft" | "hard";
}

export function taskMemory(t: TaskView): TaskMemory | null {
  if (!t.memory) return null;
  const state = t.memory.state === "soft" || t.memory.state === "hard" ? t.memory.state : "ok";
  return { bytes: t.memory.bytes, budget: t.memory.budget, state };
}

export function isHibernated(t: TaskView): boolean {
  return t.hibernated === true;
}

export function isMemoryEvent(kind: string): boolean {
  return kind.startsWith("memory.");
}

/** One line for the feed / memory panel. */
export function describeMemoryEvent(kind: string, p: Record<string, unknown>): string {
  const reason = typeof p.reason === "string" ? `: ${p.reason}` : "";
  switch (kind) {
    case "memory.warn":
      return `memory ${String(p.state ?? "warning")} (${mb(p.bytes)} of ${mb(p.budget)})`;
    case "memory.level":
      return `system memory pressure ${String(p.to ?? "changed")}`;
    case "memory.paused":
      return "new sessions and queued deliveries paused (memory pressure)";
    case "memory.resumed":
      return "new sessions and queued deliveries resumed";
    case "memory.offer_hibernate":
      return `hibernate suggested${reason}`;
    case "memory.hibernated":
      return `hibernated (${mb(p.bytes)} freed)`;
    case "memory.auto_hibernated":
      return `hibernated automatically${reason}`;
    case "memory.refused":
      return `not hibernated${reason}`;
    case "memory.leak":
      return `memory keeps growing (${String(p.mb_per_min ?? "?")} MB/min while idle)`;
    default:
      return kind;
  }
}

export function mb(v: unknown): string {
  return typeof v === "number" ? `${Math.round(v / (1 << 20))} MB` : "?";
}
```

- [ ] **Step 5: Create `apps/cockpit/src/store/state.ts`**

```ts
// The client store: one reducer over (snapshot | event | refreshed view | history). The
// daemon's event stream is a hint (seq-ordered, may replay); task views fetched with
// `task.get` / `task.list` are the authority and overwrite what events guessed.
import type { DaemonEvent, PermissionView, SendMode, Snapshot, TaskView } from "../lib/protocol";
import { describeMemoryEvent, isMemoryEvent } from "../lib/memguard";

export type FeedKind = "tool" | "command" | "agent_message" | "message" | "turn" | "system" | "permission";

export interface FeedItem {
  seq: number;
  ts: number;
  kind: FeedKind;
  text: string;
  ok?: boolean;
  mode?: SendMode;
  phase?: "started" | "ended";
}

export type PermState = "pending" | "allowed_once" | "denied" | "expired";

export interface Perm {
  id: string;
  task_id: string;
  tool: string;
  detail: string;
  status: PermState;
  created_at: number;
  /** seq of the last event that changed it (older replays never win). */
  seq: number;
}

export interface MemoryNotice {
  seq: number;
  task_id: string | null;
  kind: string;
  text: string;
}

export type Connection =
  | { state: "connecting" }
  | { state: "live" }
  | { state: "down"; error: string; retryMs: number };

export interface State {
  connection: Connection;
  bootId: string | null;
  seq: number;
  tasks: Record<string, TaskView>;
  feed: Record<string, FeedItem[]>;
  perms: Record<string, Perm>;
  memory: MemoryNotice[];
}

export const FEED_CAP = 300;

export const initialState: State = {
  connection: { state: "connecting" },
  bootId: null,
  seq: 0,
  tasks: {},
  feed: {},
  perms: {},
  memory: [],
};

export type Action =
  | { type: "connected"; snapshot: Snapshot }
  | { type: "disconnected"; error: string; retryMs: number }
  | { type: "event"; event: DaemonEvent }
  | { type: "task"; view: TaskView }
  | { type: "tasks"; views: TaskView[] }
  | { type: "permissions"; list: PermissionView[] }
  | { type: "history"; taskId: string; events: DaemonEvent[] };

const str = (v: unknown) => (typeof v === "string" ? v : v == null ? "" : String(v));

/** The feed line an event adds, if any. */
export function feedItem(e: DaemonEvent): FeedItem | null {
  const p = e.payload ?? {};
  const base = { seq: e.seq, ts: e.ts_ms };
  switch (e.kind) {
    case "step": {
      switch (p.type) {
        case "tool":
          return { ...base, kind: "tool", text: `${str(p.tool)} ${str(p.summary)}`.trim(), ok: p.ok !== false };
        case "command":
          return { ...base, kind: "command", text: `$ ${str(p.command)}`, ok: p.exit_code === 0 };
        case "agent_message":
          return { ...base, kind: "agent_message", text: str(p.text) };
        default:
          return { ...base, kind: "system", text: [str(p.type), str(p.error)].filter(Boolean).join(": ") };
      }
    }
    case "message_queued":
      return { ...base, kind: "message", mode: "queue", text: str(p.text) };
    case "message_sent":
      return p.mode === "queue"
        ? { ...base, kind: "system", text: "queued message delivered" }
        : { ...base, kind: "message", mode: "steer", text: str(p.text) };
    case "message_delivery":
      return { ...base, kind: "system", text: `message ${str(p.delivery)}` };
    case "message_failed":
      return { ...base, kind: "system", text: `message failed: ${str(p.error)}` };
    case "queue_dropped":
      return { ...base, kind: "system", text: "queued messages dropped (session ended)" };
    case "turn_started":
      return { ...base, kind: "turn", phase: "started", text: "turn started" };
    case "turn_ended":
      return { ...base, kind: "turn", phase: "ended", text: "turn ended" };
    case "permission_requested":
      return { ...base, kind: "permission", text: `asks: ${str(p.tool)} ${str(p.detail)}`.trim() };
    case "permission_answered":
    case "permission_settled":
      return { ...base, kind: "system", text: p.answer === "allowed_once" ? "allowed once" : "denied" };
    case "permission_expired":
      return { ...base, kind: "system", text: "permission request expired" };
    case "boot":
      return p.state === "trust_prompt" || p.state === "exited"
        ? { ...base, kind: "system", text: p.state === "exited" ? "agent exited during start" : "asks to trust the folder" }
        : null;
    case "process_exited":
      return { ...base, kind: "system", text: `agent exited (code ${str(p.code) || "none"})` };
    case "interrupt_sent":
      return { ...base, kind: "system", text: "interrupted" };
    default:
      return isMemoryEvent(e.kind) ? { ...base, kind: "system", text: describeMemoryEvent(e.kind, p) } : null;
  }
}

function addFeed(feed: Record<string, FeedItem[]>, taskId: string, items: FeedItem[]): Record<string, FeedItem[]> {
  if (items.length === 0) return feed;
  const cur = feed[taskId] ?? [];
  const seen = new Set(cur.map((i) => i.seq));
  const fresh = items.filter((i) => !seen.has(i.seq));
  if (fresh.length === 0) return feed;
  const merged = [...cur, ...fresh].sort((a, b) => a.seq - b.seq).slice(-FEED_CAP);
  return { ...feed, [taskId]: merged };
}

function permEffect(perms: Record<string, Perm>, e: DaemonEvent): Record<string, Perm> {
  const p = e.payload ?? {};
  const id = str(p.permission_id);
  if (!id || !e.task_id) return perms;
  const cur = perms[id];
  if (cur && cur.seq >= e.seq) return perms;
  if (e.kind === "permission_requested") {
    const status: PermState = cur?.status ?? "pending";
    return {
      ...perms,
      [id]: { id, task_id: e.task_id, tool: str(p.tool), detail: str(p.detail), status, created_at: e.ts_ms, seq: e.seq },
    };
  }
  const status: PermState =
    e.kind === "permission_expired" ? "expired" : p.answer === "allowed_once" ? "allowed_once" : "denied";
  const known = cur ?? { id, task_id: e.task_id, tool: "", detail: "", created_at: e.ts_ms, status, seq: e.seq };
  return { ...perms, [id]: { ...known, status, seq: e.seq } };
}

/** What an event says about the task row itself (refined later by `task.get`). */
function taskEffect(tasks: Record<string, TaskView>, e: DaemonEvent): Record<string, TaskView> {
  const id = e.task_id;
  const p = e.payload ?? {};
  if (!id) return tasks;
  if (e.kind === "task_created") {
    const defaults = { live: false, boot: null, needs_you: 0, queued: 0, detail: null };
    return { ...tasks, [id]: { ...defaults, ...p } as TaskView };
  }
  const t = tasks[id];
  if (!t) return tasks;
  const put = (patch: Partial<TaskView>) => ({ ...tasks, [id]: { ...t, ...patch } });
  switch (e.kind) {
    case "status":
      return put({ status: p.to as TaskView["status"] });
    case "lifecycle":
      return put({ lifecycle: p.to as TaskView["lifecycle"] });
    case "boot":
      return put({ boot: p.state as TaskView["boot"], live: p.state !== "exited" });
    case "session_started":
      return put({ live: true });
    case "session_ended":
    case "process_exited":
      return put({ live: false, boot: null });
    case "permission_requested":
      return put({ needs_you: t.needs_you + 1 });
    case "permission_answered":
    case "permission_settled":
    case "permission_expired":
      return put({ needs_you: Math.max(0, t.needs_you - 1) });
    case "message_queued":
      return put({ queued: t.queued + 1 });
    case "task_updated":
      return put({ goal: str(p.goal), notes: str(p.notes) });
    default:
      return tasks;
  }
}

export function reduce(s: State, a: Action): State {
  switch (a.type) {
    case "connected": {
      const snap = a.snapshot;
      if (snap.resumed) return { ...s, connection: { state: "live" }, bootId: snap.boot_id, seq: snap.seq };
      const tasks: Record<string, TaskView> = {};
      for (const t of snap.tasks) tasks[t.id] = t;
      return { ...s, connection: { state: "live" }, bootId: snap.boot_id, seq: snap.seq, tasks };
    }
    case "disconnected":
      return { ...s, connection: { state: "down", error: a.error, retryMs: a.retryMs } };
    case "event": {
      const e = a.event;
      if (e.boot_id === s.bootId && e.seq <= s.seq) return s;
      const item = feedItem(e);
      const memory = isMemoryEvent(e.kind)
        ? [...s.memory, { seq: e.seq, task_id: e.task_id, kind: e.kind, text: describeMemoryEvent(e.kind, e.payload ?? {}) }].slice(-20)
        : s.memory;
      return {
        ...s,
        bootId: e.boot_id,
        seq: e.seq,
        tasks: taskEffect(s.tasks, e),
        perms: e.kind.startsWith("permission_") ? permEffect(s.perms, e) : s.perms,
        feed: item && e.task_id ? addFeed(s.feed, e.task_id, [item]) : s.feed,
        memory,
      };
    }
    case "task":
      return { ...s, tasks: { ...s.tasks, [a.view.id]: a.view } };
    case "tasks": {
      const tasks: Record<string, TaskView> = {};
      for (const t of a.views) tasks[t.id] = t;
      return { ...s, tasks };
    }
    case "permissions": {
      const perms = { ...s.perms };
      for (const p of a.list) {
        const cur = perms[p.id];
        perms[p.id] = {
          id: p.id,
          task_id: p.task_id,
          tool: p.tool,
          detail: p.detail,
          status: p.status,
          created_at: p.created_at,
          seq: cur?.seq ?? 0,
        };
      }
      // A pending card the daemon no longer lists was answered elsewhere (CLI) or expired.
      for (const id of Object.keys(perms)) {
        if (perms[id].status === "pending" && !a.list.some((p) => p.id === id)) {
          perms[id] = { ...perms[id], status: "expired" };
        }
      }
      return { ...s, perms };
    }
    case "history": {
      const items = a.events.map(feedItem).filter((i): i is FeedItem => i !== null);
      let perms = s.perms;
      for (const e of a.events) if (e.kind.startsWith("permission_")) perms = permEffect(perms, e);
      return { ...s, feed: addFeed(s.feed, a.taskId, items), perms };
    }
  }
}
```

- [ ] **Step 6: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/store/state.test.ts`

Expected: exit 0; output contains `Tests  12 passed (12)`

- [ ] **Step 7: Commit**

```bash
git add apps/cockpit/src/test/fixtures.ts apps/cockpit/src/lib/memguard.ts apps/cockpit/src/store
git commit -F- <<'EOF'
feat(cockpit): event-sourced client store reconciling snapshot and seq

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 10: Store wiring and selectors

`createCockpit(transport)`: subscribe; on a full snapshot load each task's history (`task.events`, 200) and the pending permissions; on a resumed snapshot reload `task.list`; on each event refresh that task with `task.get` (coalesced per task over 120 ms) and reload permissions after `permission_*` events. Selectors derive the board columns (Needs you / Working / Ready for review), projects, grid tiles and "newly needs you" for notifications.

**Files:**
- Create: `apps/cockpit/src/store/select.ts`
- Create: `apps/cockpit/src/store/select.test.ts`
- Create: `apps/cockpit/src/store/cockpit.ts`
- Create: `apps/cockpit/src/store/cockpit.test.ts`

- [ ] **Step 1: Create `apps/cockpit/src/store/select.test.ts`**

```ts
import { describe, expect, it } from "vitest";
import { ev, snapshot, task } from "../test/fixtures";
import { columnOf, gridTasks, newlyNeedingYou, projects, tasksIn } from "./select";
import { initialState, reduce } from "./state";

const withTasks = (...ts: ReturnType<typeof task>[]) => reduce(initialState, { type: "connected", snapshot: snapshot(ts) });

describe("columns", () => {
  it("needs-you wins over everything; trust prompt and pending cards count", () => {
    expect(columnOf(task("a", { status: "needs_you" }), [])).toBe("needs_you");
    expect(columnOf(task("a", { status: "working", needs_you: 1 }), [])).toBe("needs_you");
    expect(columnOf(task("a", { boot: "trust_prompt" }), [])).toBe("needs_you");
  });

  it("working, review after a finished turn, idle before any", () => {
    expect(columnOf(task("a", { status: "working" }), [])).toBe("working");
    expect(columnOf(task("a", { boot: "booting" }), [])).toBe("working");
    const ended = [{ seq: 1, ts: 1, kind: "turn" as const, phase: "ended" as const, text: "" }];
    expect(columnOf(task("a", { status: "idle" }), ended)).toBe("review");
    expect(columnOf(task("a", { status: "ended", live: false }), ended)).toBe("review");
    expect(columnOf(task("a"), [])).toBe("idle");
    expect(columnOf(task("a", { lifecycle: "archived", status: "needs_you" }), [])).toBe("archived");
  });

  it("tasksIn filters by repo, newest first", () => {
    const s = withTasks(task("a", { status: "working", updated_at: 1 }), task("b", { status: "working", updated_at: 2 }), task("c", { status: "working", repo_path: "/repos/other" }));
    expect(tasksIn(s, "working", "/repos/shop").map((t) => t.id)).toEqual(["b", "a"]);
  });
});

describe("projects", () => {
  it("merges the registry with task repos and counts needs-you", () => {
    const s = withTasks(task("a", { status: "needs_you" }), task("b"));
    expect(projects(s, ["/repos/mirante", "/repos/shop"])).toEqual([
      { path: "/repos/mirante", name: "mirante", tasks: 0, needsYou: 0 },
      { path: "/repos/shop", name: "shop", tasks: 2, needsYou: 1 },
    ]);
  });
});

describe("notifications input", () => {
  it("reports only tasks that newly need the user", () => {
    const prev = withTasks(task("a"), task("b", { status: "needs_you" }));
    let next = reduce(prev, { type: "event", event: ev(1, "a", "status", { to: "needs_you" }) });
    expect(newlyNeedingYou(prev, next).map((t) => t.id)).toEqual(["a"]);
    next = reduce(next, { type: "event", event: ev(2, "b", "step", { type: "tool" }) });
    expect(newlyNeedingYou(prev, next).map((t) => t.id)).toEqual(["a"]);
  });
});

describe("grid", () => {
  it("orders needs-you, working, review and caps at six", () => {
    const many = Array.from({ length: 8 }, (_, i) => task(`w${i}`, { status: "working", updated_at: i }));
    const s = withTasks(task("n", { status: "needs_you" }), ...many);
    const g = gridTasks(s);
    expect(g).toHaveLength(6);
    expect(g[0].id).toBe("n");
    expect(g[1].id).toBe("w7");
  });
});
```

- [ ] **Step 2: Create `apps/cockpit/src/store/cockpit.test.ts`**

```ts
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { StreamMsg } from "../lib/protocol";
import { ev, snapshot, task } from "../test/fixtures";
import { createCockpit, REFRESH_MS } from "./cockpit";

function fakeTransport(answers: Record<string, unknown>) {
  let push: (m: StreamMsg) => void = () => {};
  const calls: [string, object | undefined][] = [];
  return {
    calls,
    push: (m: StreamMsg) => push(m),
    rpc: vi.fn(async (method: string, params?: object) => {
      calls.push([method, params]);
      const a = answers[method];
      return (typeof a === "function" ? a(params) : a) as never;
    }),
    subscribe: vi.fn(async (fn: (m: StreamMsg) => void) => {
      push = fn;
    }),
  };
}

describe("createCockpit", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("loads history and permissions after a full snapshot", async () => {
    const t = fakeTransport({
      "permission.list": [],
      "task.events": [ev(1, "t1", "turn_started"), ev(2, "t1", "turn_ended")],
    });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 2) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    expect(t.calls).toContainEqual(["task.events", { task: "t1", limit: 200 }]);
    expect(t.calls.map((x) => x[0])).toContain("permission.list");
    expect(c.getState().feed.t1).toHaveLength(2);
  });

  it("refreshes a task once for a burst of its events", async () => {
    const t = fakeTransport({ "task.events": [], "permission.list": [], "task.get": task("t1", { status: "working", queued: 2 }) });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 0) });
    for (let i = 1; i <= 5; i++) t.push({ type: "event", event: ev(i, "t1", "step", { type: "tool" }) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    expect(t.calls.filter((x) => x[0] === "task.get")).toEqual([["task.get", { task: "t1" }]]);
    expect(c.getState().tasks.t1.queued).toBe(2);
  });

  it("after a resumed snapshot reloads the task list instead of history", async () => {
    const t = fakeTransport({ "permission.list": [], "task.list": [task("t1"), task("t2")] });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([], 4, { resumed: true }) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    expect(Object.keys(c.getState().tasks)).toEqual(["t1", "t2"]);
    expect(t.calls.map((x) => x[0])).not.toContain("task.events");
  });

  it("tells watchers about each change with prev and next", async () => {
    const t = fakeTransport({ "task.events": [], "permission.list": [], "task.get": task("t1", { status: "needs_you" }) });
    const c = createCockpit(t);
    const seen: string[] = [];
    c.onChange((p, n) => seen.push(`${p.tasks.t1?.status ?? "-"}>${n.tasks.t1?.status ?? "-"}`));
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 0) });
    t.push({ type: "event", event: ev(1, "t1", "status", { to: "needs_you" }) });
    expect(seen).toEqual(["->idle", "idle>needs_you"]);
  });
});
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/store`

Expected: FAIL — Failed to resolve import "./select"

- [ ] **Step 4: Create `apps/cockpit/src/store/select.ts`**

```ts
// Derived views of the store. Pure functions, unit-tested.
import type { TaskView } from "../lib/protocol";
import type { FeedItem, Perm, State } from "./state";

export type Column = "needs_you" | "working" | "review" | "idle" | "archived";

export const COLUMN_LABEL: Record<Column, string> = {
  needs_you: "Needs you",
  working: "Working",
  review: "Ready for review",
  idle: "Idle",
  archived: "Archived",
};

/** The three board columns, in order. */
export const BOARD: Column[] = ["needs_you", "working", "review"];

export function needsYou(t: TaskView): boolean {
  return t.lifecycle !== "archived" && (t.status === "needs_you" || t.needs_you > 0 || t.boot === "trust_prompt");
}

export function turnsEnded(feed: FeedItem[] | undefined): number {
  return (feed ?? []).filter((i) => i.kind === "turn" && i.phase === "ended").length;
}

export function columnOf(t: TaskView, feed: FeedItem[] | undefined): Column {
  if (t.lifecycle === "archived" || t.lifecycle === "archiving") return "archived";
  if (needsYou(t)) return "needs_you";
  if (t.status === "working" || t.boot === "booting") return "working";
  if (turnsEnded(feed) > 0) return "review";
  return "idle";
}

export function tasksIn(s: State, col: Column, repo?: string): TaskView[] {
  return Object.values(s.tasks)
    .filter((t) => (repo ? t.repo_path === repo : true) && columnOf(t, s.feed[t.id]) === col)
    .sort((a, b) => b.updated_at - a.updated_at);
}

export function pendingPerms(s: State, taskId: string): Perm[] {
  return Object.values(s.perms)
    .filter((p) => p.task_id === taskId && p.status === "pending")
    .sort((a, b) => a.created_at - b.created_at);
}

export function repoName(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || path;
}

export interface Project {
  path: string;
  name: string;
  tasks: number;
  needsYou: number;
}

/** Repos from the registry plus every repo a task runs in, by name. */
export function projects(s: State, registry: string[]): Project[] {
  const paths = new Set(registry);
  for (const t of Object.values(s.tasks)) if (t.lifecycle !== "archived") paths.add(t.repo_path);
  return [...paths]
    .map((path) => {
      const ts = Object.values(s.tasks).filter((t) => t.repo_path === path && t.lifecycle !== "archived");
      return { path, name: repoName(path), tasks: ts.length, needsYou: ts.filter(needsYou).length };
    })
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** The last `n` feed lines worth showing on a small tile. */
export function lastLines(feed: FeedItem[] | undefined, n: number): FeedItem[] {
  return (feed ?? []).filter((i) => i.kind !== "turn").slice(-n);
}

/** Tasks that just started needing the user (for notifications). */
export function newlyNeedingYou(prev: State, next: State): TaskView[] {
  return Object.values(next.tasks).filter((t) => {
    const before = prev.tasks[t.id];
    return needsYou(t) && !(before && needsYou(before));
  });
}

/** The grid's tiles: needs-you first, then working, then the most recently active; 2..6. */
export function gridTasks(s: State, max = 6): TaskView[] {
  const rank: Record<Column, number> = { needs_you: 0, working: 1, review: 2, idle: 3, archived: 9 };
  return Object.values(s.tasks)
    .filter((t) => t.lifecycle !== "archived" && (t.live || columnOf(t, s.feed[t.id]) !== "idle"))
    .sort((a, b) => rank[columnOf(a, s.feed[a.id])] - rank[columnOf(b, s.feed[b.id])] || b.updated_at - a.updated_at)
    .slice(0, max);
}
```

- [ ] **Step 5: Create `apps/cockpit/src/store/cockpit.ts`**

```ts
// The store instance and its wiring to the daemon: subscribe, then on every event refresh
// the task it names with `task.get` (coalesced), reload pending permissions after
// permission events, and load each task's history (`task.events`) after a full snapshot.
import { useSyncExternalStore } from "react";
import type { DaemonEvent, PermissionView, StreamMsg, TaskView } from "../lib/protocol";
import { M } from "../lib/protocol";
import { type Action, initialState, reduce, type State } from "./state";

export interface Transport {
  rpc<T>(method: string, params?: object): Promise<T>;
  subscribe(onMsg: (m: StreamMsg) => void): Promise<void>;
}

export const REFRESH_MS = 120;

export interface Cockpit {
  getState(): State;
  subscribe(listener: () => void): () => void;
  dispatch(a: Action): void;
  connect(): Promise<void>;
  /** Called after each state change with (previous, next). */
  onChange(fn: (prev: State, next: State) => void): () => void;
}

export function createCockpit(t: Transport): Cockpit {
  let state = initialState;
  const listeners = new Set<() => void>();
  const watchers = new Set<(prev: State, next: State) => void>();
  const timers = new Map<string, ReturnType<typeof setTimeout>>();

  const dispatch = (a: Action) => {
    const prev = state;
    state = reduce(state, a);
    if (state === prev) return;
    for (const w of watchers) w(prev, state);
    for (const l of listeners) l();
  };

  const later = (key: string, fn: () => Promise<void>) => {
    if (timers.has(key)) return;
    timers.set(
      key,
      setTimeout(() => {
        timers.delete(key);
        fn().catch(() => {});
      }, REFRESH_MS),
    );
  };

  const refreshTask = (id: string) =>
    later(`task:${id}`, async () => dispatch({ type: "task", view: await t.rpc<TaskView>(M.taskGet, { task: id }) }));
  const loadPermissions = () =>
    later("perms", async () => dispatch({ type: "permissions", list: await t.rpc<PermissionView[]>(M.permissionList) }));
  const loadHistory = async (id: string) =>
    dispatch({ type: "history", taskId: id, events: await t.rpc<DaemonEvent[]>(M.taskEvents, { task: id, limit: 200 }) });

  const onMsg = (m: StreamMsg) => {
    switch (m.type) {
      case "connected":
        dispatch({ type: "connected", snapshot: m.snapshot });
        loadPermissions();
        if (m.snapshot.resumed) {
          // Missed events follow; the rows they touch are refreshed one by one, but rows
          // changed by events older than the stream's buffer are only right after a list.
          later("list", async () => dispatch({ type: "tasks", views: await t.rpc<TaskView[]>(M.taskList) }));
        } else {
          for (const v of m.snapshot.tasks) loadHistory(v.id).catch(() => {});
        }
        break;
      case "event":
        dispatch({ type: "event", event: m.event });
        if (m.event.task_id) refreshTask(m.event.task_id);
        if (m.event.kind.startsWith("permission_")) loadPermissions();
        break;
      case "disconnected":
        dispatch({ type: "disconnected", error: m.error, retryMs: m.retry_ms });
        break;
    }
  };

  return {
    getState: () => state,
    subscribe(l) {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    onChange(fn) {
      watchers.add(fn);
      return () => watchers.delete(fn);
    },
    dispatch,
    connect: () => t.subscribe(onMsg),
  };
}

/** The whole state (a stable reference until it changes); derive with useMemo. */
export function useCockpitState(c: Cockpit): State {
  return useSyncExternalStore(c.subscribe, c.getState);
}
```

- [ ] **Step 6: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/store`

Expected: exit 0; output contains `Tests  22 passed (22)`

- [ ] **Step 7: Commit**

```bash
git add apps/cockpit/src/store
git commit -F- <<'EOF'
feat(cockpit): store wiring (history, coalesced refresh, resume) and board selectors

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


## Phase 3-ii — V3 home (group chat)

### Task 11: Dispatch parser and repo registry

`@claude shop/feat/login add a login page`: the repo is everything before the first `/`, the branch is the rest of that word (it may contain `/`), the goal the rest of the text (newlines kept). Branch names are checked against git's rules before the daemon sees them. Repo names resolve against known projects (registry + task repos); ambiguous names are refused. The registry is per machine in localStorage.

**Files:**
- Create: `apps/cockpit/src/lib/dispatch.ts`
- Create: `apps/cockpit/src/lib/dispatch.test.ts`
- Create: `apps/cockpit/src/lib/repos.ts`
- Create: `apps/cockpit/src/lib/repos.test.ts`

- [ ] **Step 1: Create `apps/cockpit/src/lib/dispatch.test.ts`**

```ts
import { describe, expect, it } from "vitest";
import { parseDispatch, resolveRepo } from "./dispatch";

describe("parseDispatch", () => {
  it("parses agent, repo, a branch with slashes and a multi-word goal", () => {
    expect(parseDispatch("@claude shop/feat/login add a login page")).toEqual({
      ok: true,
      value: { agent: "claude", repo: "shop", branch: "feat/login", goal: "add a login page" },
    });
    expect(parseDispatch("  @Codex   api/fix-42   fix the\nflaky test ")).toEqual({
      ok: true,
      value: { agent: "codex", repo: "api", branch: "fix-42", goal: "fix the\nflaky test" },
    });
  });

  it.each([
    ["fix it", "start with an agent"],
    ["@gpt shop/x do", 'unknown agent "@gpt"'],
    ["@claude", "say where"],
    ["@claude shop do it", '"shop" must be <repo>/<branch>'],
    ["@claude /main do it", "must be <repo>/<branch>"],
    ["@claude shop/ do it", "must be <repo>/<branch>"],
    ["@claude shop/feat/x", "say what the agent should do"],
    ["@claude shop/-rf do", "cannot start with '-'"],
    ["@claude shop/a..b do", "cannot contain '..'"],
    ["@claude shop/x.lock do", "not a valid branch name"],
    ["@claude shop/a:b do", "git does not allow"],
  ])("rejects %j", (input, msg) => {
    const r = parseDispatch(input);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain(msg);
  });
});

describe("resolveRepo", () => {
  const known = [
    { name: "shop", path: "/a/shop" },
    { name: "api", path: "/a/api" },
    { name: "api", path: "/b/api" },
  ];
  it("finds a unique name, refuses ambiguous and unknown ones", () => {
    expect(resolveRepo("shop", known)).toEqual({ ok: true, path: "/a/shop" });
    expect(resolveRepo("api", known)).toMatchObject({ ok: false });
    expect(resolveRepo("web", known)).toEqual({ ok: false, error: 'unknown repo "web": add it under Projects first' });
  });
});
```

- [ ] **Step 2: Create `apps/cockpit/src/lib/repos.test.ts`**

```ts
import { describe, expect, it } from "vitest";
import { addRepo, loadRepos, removeRepo, REPOS_KEY } from "./repos";

function mem() {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
}

describe("repo registry", () => {
  it("adds absolute paths once, without trailing slashes", () => {
    const s = mem();
    expect(addRepo(s, "/Users/me/shop/")).toEqual({ ok: true, repos: ["/Users/me/shop"] });
    addRepo(s, "/Users/me/shop");
    expect(loadRepos(s)).toEqual(["/Users/me/shop"]);
    expect(removeRepo(s, "/Users/me/shop")).toEqual([]);
  });

  it("refuses relative paths and survives garbage in storage", () => {
    const s = mem();
    expect(addRepo(s, "shop")).toMatchObject({ ok: false });
    s.setItem(REPOS_KEY, "{not json");
    expect(loadRepos(s)).toEqual([]);
  });
});
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/lib/dispatch.test.ts src/lib/repos.test.ts`

Expected: FAIL — Failed to resolve import "./dispatch"

- [ ] **Step 4: Create `apps/cockpit/src/lib/dispatch.ts`**

```ts
// "@claude shop/feat/login add a login page" -> agent claude, repo "shop", branch
// "feat/login", goal "add a login page". The repo is everything before the first "/";
// the branch is the rest of that word (it may contain "/"); the goal is the rest of the text.
import type { AgentKind } from "./protocol";

export interface Dispatch {
  agent: AgentKind;
  repo: string;
  branch: string;
  goal: string;
}

export type Parsed = { ok: true; value: Dispatch } | { ok: false; error: string };

export const DISPATCH_HELP = "@claude <repo>/<branch> <goal>  or  @codex <repo>/<branch> <goal>";

const AGENTS: Record<string, AgentKind> = { "@claude": "claude", "@codex": "codex" };

function branchProblem(b: string): string | null {
  if (b.startsWith("-")) return "a branch cannot start with '-'";
  if (b.includes("..")) return "a branch cannot contain '..'";
  if (b.endsWith("/") || b.endsWith(".lock") || b.endsWith(".")) return `"${b}" is not a valid branch name`;
  if (/[~^:?*[\\\x00-\x20\x7f]/.test(b)) return `"${b}" has a character git does not allow in branch names`;
  if (b.includes("//") || b.includes("@{")) return `"${b}" is not a valid branch name`;
  return null;
}

export function parseDispatch(input: string): Parsed {
  const text = input.trim();
  const m = /^(\S+)(?:\s+(\S+))?(?:\s+([\s\S]*))?$/.exec(text);
  if (!text.startsWith("@") || !m) return { ok: false, error: `start with an agent: ${DISPATCH_HELP}` };
  const agent = AGENTS[m[1].toLowerCase()];
  if (!agent) return { ok: false, error: `unknown agent "${m[1]}": use @claude or @codex` };
  const target = m[2];
  if (!target) return { ok: false, error: `say where: ${DISPATCH_HELP}` };
  const slash = target.indexOf("/");
  if (slash <= 0 || slash === target.length - 1) {
    return { ok: false, error: `"${target}" must be <repo>/<branch>, e.g. shop/feat/login` };
  }
  const repo = target.slice(0, slash);
  const branch = target.slice(slash + 1);
  const bad = branchProblem(branch);
  if (bad) return { ok: false, error: bad };
  const goal = (m[3] ?? "").trim();
  if (!goal) return { ok: false, error: "say what the agent should do after the branch" };
  return { ok: true, value: { agent, repo, branch, goal } };
}

export type Resolved = { ok: true; path: string } | { ok: false; error: string };

/** The repo path for a name typed in a dispatch, from the known projects. */
export function resolveRepo(name: string, known: { name: string; path: string }[]): Resolved {
  const hits = known.filter((p) => p.name === name);
  if (hits.length === 1) return { ok: true, path: hits[0].path };
  if (hits.length > 1) {
    return { ok: false, error: `"${name}" names several repos (${hits.map((h) => h.path).join(", ")}); rename one folder` };
  }
  return { ok: false, error: `unknown repo "${name}": add it under Projects first` };
}
```

- [ ] **Step 5: Create `apps/cockpit/src/lib/repos.ts`**

```ts
// Repos the user added by path (tasks add theirs implicitly). Stored in the webview's
// localStorage: per machine, never sent anywhere.
export const REPOS_KEY = "mirante.repos";

type KV = Pick<Storage, "getItem" | "setItem">;

export function loadRepos(store: KV): string[] {
  try {
    const v = JSON.parse(store.getItem(REPOS_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((p): p is string => typeof p === "string") : [];
  } catch {
    return [];
  }
}

export type Added = { ok: true; repos: string[] } | { ok: false; error: string };

export function addRepo(store: KV, raw: string): Added {
  const path = raw.trim().replace(/\/+$/, "");
  if (!path.startsWith("/")) return { ok: false, error: "use the repo's absolute path, e.g. /Users/me/code/shop" };
  const repos = loadRepos(store);
  if (!repos.includes(path)) repos.push(path);
  store.setItem(REPOS_KEY, JSON.stringify(repos));
  return { ok: true, repos };
}

export function removeRepo(store: KV, path: string): string[] {
  const repos = loadRepos(store).filter((p) => p !== path);
  store.setItem(REPOS_KEY, JSON.stringify(repos));
  return repos;
}
```

- [ ] **Step 6: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/lib/dispatch.test.ts src/lib/repos.test.ts`

Expected: exit 0; output contains `Tests  15 passed (15)`

- [ ] **Step 7: Commit**

```bash
git add apps/cockpit/src/lib/dispatch.ts apps/cockpit/src/lib/dispatch.test.ts apps/cockpit/src/lib/repos.ts apps/cockpit/src/lib/repos.test.ts
git commit -F- <<'EOF'
feat(cockpit): @agent repo/branch goal parser and repo registry

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 12: Agent card: progress, permission, trust and done cards

Components get the outside world from `Env` (store, `rpc`, storage, `openUrl`, `startDaemon`, `notify`) so tests inject fakes (`src/test/env.tsx`). The card shows the dispatch line, labels (Codex **auto-edits**; a `grantRoot` request, which the daemon names `file_change (session-wide grant)`, gets **session-wide grant**), the last five feed lines, Allow once / Deny cards (a CONFLICT answer means "already answered"), the trust card, and when the turn ended the diff summary with **Open PR** (disabled until pushed).

**Files:**
- Create: `apps/cockpit/src/env.tsx`
- Create: `apps/cockpit/src/lib/labels.ts`
- Create: `apps/cockpit/src/components/Badges.tsx`
- Create: `apps/cockpit/src/components/PermissionCard.tsx`
- Create: `apps/cockpit/src/components/TrustCard.tsx`
- Create: `apps/cockpit/src/components/useDiff.ts`
- Create: `apps/cockpit/src/components/DoneCard.tsx`
- Create: `apps/cockpit/src/components/AgentCard.tsx`
- Create: `apps/cockpit/src/lib/labels.test.ts`
- Create: `apps/cockpit/src/test/env.tsx`
- Create: `apps/cockpit/src/components/AgentCard.test.tsx`

- [ ] **Step 1: Create `apps/cockpit/src/test/env.tsx`**

```tsx
// Test harness: a real store fed by hand, a recording rpc, and the Env around a component.
import { render } from "@testing-library/react";
import type { ReactElement } from "react";
import { vi } from "vitest";
import { type Env, EnvContext } from "../env";
import { DaemonError } from "../lib/daemon";
import type { BridgeError, DaemonEvent, TaskView } from "../lib/protocol";
import { createCockpit } from "../store/cockpit";
import { snapshot } from "./fixtures";

type Answer = unknown | ((params: Record<string, unknown>) => unknown);

export function makeEnv(answers: Record<string, Answer> = {}) {
  const calls: [string, Record<string, unknown>][] = [];
  const rpc = vi.fn(async (method: string, params: object = {}) => {
    calls.push([method, params as Record<string, unknown>]);
    const a = answers[method];
    const v = typeof a === "function" ? (a as (p: Record<string, unknown>) => unknown)(params as Record<string, unknown>) : a;
    if (v && typeof v === "object" && "kind" in v) throw new DaemonError(v as BridgeError);
    return v as never;
  });
  const cockpit = createCockpit({ rpc, subscribe: async () => {} });
  const mem = new Map<string, string>();
  const env: Env = {
    cockpit,
    rpc,
    storage: { getItem: (k) => mem.get(k) ?? null, setItem: (k, v) => void mem.set(k, v) },
    openUrl: vi.fn(async () => {}),
    startDaemon: vi.fn(async () => {}),
    notify: vi.fn(),
  };
  return {
    env,
    calls,
    load(tasks: TaskView[], events: DaemonEvent[] = []) {
      cockpit.dispatch({ type: "connected", snapshot: snapshot(tasks, 0) });
      for (const e of events) cockpit.dispatch({ type: "event", event: e });
    },
  };
}

export function renderWith(env: Env, ui: ReactElement) {
  return render(<EnvContext.Provider value={env}>{ui}</EnvContext.Provider>);
}

export const rpcError = (code: number, message = "refused"): BridgeError => ({ kind: "rpc", code, message });
```

- [ ] **Step 2: Create `apps/cockpit/src/lib/labels.test.ts`**

```ts
import { describe, expect, it } from "vitest";
import { agentModeLabel, GRANT_ROOT_TOOL, permissionScope } from "./labels";

describe("labels", () => {
  it("only Codex carries the auto-edits label", () => {
    expect(agentModeLabel("codex")?.text).toBe("auto-edits");
    expect(agentModeLabel("claude")).toBeNull();
  });

  it("grantRoot requests are session-wide, everything else once", () => {
    expect(permissionScope(GRANT_ROOT_TOOL).text).toBe("session-wide grant");
    expect(permissionScope("Bash").text).toBe("once");
  });
});
```

- [ ] **Step 3: Create `apps/cockpit/src/components/AgentCard.test.tsx`**

```tsx
import { act, fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { GRANT_ROOT_TOOL } from "../lib/labels";
import type { TaskDiff, TaskView } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { AgentCard } from "./AgentCard";

const diff: TaskDiff = {
  base: "main",
  merge_base: "abc12345ff",
  files: [{ path: "src/a.ts", added: 10, removed: 2 }],
  untracked: ["notes.md"],
  patch: "+x",
  truncated: false,
  compare_url: "https://github.com/acme/shop/compare/main...feat/b?expand=1",
  pushed: true,
};

function setup(answers: Record<string, unknown>, t: TaskView, events: ReturnType<typeof ev>[] = []) {
  const h = makeEnv(answers);
  h.load([t], events);
  renderWith(h.env, <AgentCard t={h.env.cockpit.getState().tasks[t.id]} onOpen={() => {}} />);
  return h;
}

describe("AgentCard", () => {
  it("shows the dispatch, live steps, and answers a permission card once", async () => {
    const h = setup({ "permission.answer": { status: "allowed_once" } }, task("a", { status: "working" }), [
      ev(1, "a", "step", { type: "tool", tool: "Bash", summary: "npm test", ok: true }),
      ev(2, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "rm -rf dist" }),
    ]);
    const card = screen.getByRole("article", { name: "claude on feat/a" });
    expect(within(card).getByText("Bash npm test")).toBeInTheDocument();
    const perm = within(card).getByRole("group", { name: "Permission request: Bash" });
    expect(within(perm).getByText("once")).toBeInTheDocument();
    await userEvent.click(within(perm).getByRole("button", { name: "Allow once" }));
    expect(h.calls).toContainEqual(["permission.answer", { id: "p1", allow: true }]);
  });

  it("an already-answered card says so", async () => {
    setup({ "permission.answer": rpcError(RpcCode.CONFLICT) }, task("a"), [
      ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" }),
    ]);
    await userEvent.click(screen.getByRole("button", { name: "Deny" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("already answered");
  });

  it("labels Codex auto-edits and session-wide grants", () => {
    setup({}, task("c", { agent: "codex", status: "needs_you" }), [
      ev(1, "c", "permission_requested", { permission_id: "p2", tool: GRANT_ROOT_TOOL, detail: "write access under /wt/c for the rest of the session" }),
    ]);
    expect(screen.getByText("auto-edits")).toBeInTheDocument();
    expect(screen.getByText("session-wide grant")).toBeInTheDocument();
  });

  it("a finished turn shows the diff summary and opens the PR page", async () => {
    const h = setup({ "task.diff": diff }, task("b", { status: "idle" }), [ev(1, "b", "turn_started"), ev(2, "b", "turn_ended")]);
    expect(await screen.findByText(/2 files changed/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Open PR" }));
    expect(h.env.openUrl).toHaveBeenCalledWith(diff.compare_url);
  });

  it("an unpushed branch cannot open a PR yet", async () => {
    setup({ "task.diff": { ...diff, pushed: false } }, task("b"), [ev(1, "b", "turn_ended")]);
    expect(await screen.findByRole("button", { name: "Open PR" })).toBeDisabled();
  });

  it("trust prompts are answered by the user from the card", async () => {
    const h = setup({ "session.trust": {} }, task("t", { boot: "trust_prompt" }));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Trust folder" })));
    expect(h.calls).toContainEqual(["session.trust", { task: "t", accept: true }]);
  });
});
```

- [ ] **Step 4: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/AgentCard.test.tsx src/lib/labels.test.ts`

Expected: FAIL — Failed to resolve import "../lib/labels"

- [ ] **Step 5: Create `apps/cockpit/src/env.tsx`**

```tsx
// What components need from the outside world, injected so tests can fake it.
import { createContext, useContext } from "react";
import type { TaskView } from "./lib/protocol";
import type { Cockpit } from "./store/cockpit";
import { useCockpitState } from "./store/cockpit";
import type { State } from "./store/state";

export interface Env {
  cockpit: Cockpit;
  rpc<T>(method: string, params?: object): Promise<T>;
  /** Where the repo registry lives (localStorage in the app). */
  storage: Pick<Storage, "getItem" | "setItem">;
  openUrl(url: string): Promise<void>;
  startDaemon(): Promise<void>;
  /** Native notification (already decided: the task is not on screen). */
  notify(t: TaskView): void;
}

export const EnvContext = createContext<Env | null>(null);

export function useEnv(): Env {
  const env = useContext(EnvContext);
  if (!env) throw new Error("EnvContext missing");
  return env;
}

export function useStore(): State {
  return useCockpitState(useEnv().cockpit);
}
```

- [ ] **Step 6: Create `apps/cockpit/src/lib/labels.ts`**

```ts
// Small labels that tell the user how much an agent may do on its own.
import type { AgentKind } from "./protocol";

export interface Label {
  text: string;
  title: string;
}

/** Codex runs with `approvalPolicy: on-request` in a `workspace-write` sandbox (Plan 2b). */
export function agentModeLabel(agent: AgentKind): Label | null {
  return agent === "codex"
    ? {
        text: "auto-edits",
        title: "Codex edits files inside this worktree without asking. Commands outside the sandbox, network and other folders still ask you.",
      }
    : null;
}

/** The daemon's tool name for a Codex `grantRoot` file-change approval (driver/codex.rs). */
export const GRANT_ROOT_TOOL = "file_change (session-wide grant)";

/** A `grantRoot` approval lets Codex write under that folder for the rest of the session. */
export function permissionScope(tool: string): Label {
  return tool === GRANT_ROOT_TOOL
    ? { text: "session-wide grant", title: "Allowing lets the agent write under this folder until the session ends, not just once." }
    : { text: "once", title: "Allowing applies to this request only." };
}
```

- [ ] **Step 7: Create `apps/cockpit/src/components/Badges.tsx`**

```tsx
import type { TaskView } from "../lib/protocol";
import { agentModeLabel } from "../lib/labels";
import { isHibernated } from "../lib/memguard";
import { type Column, COLUMN_LABEL } from "../store/select";

export function StatusPill({ column }: { column: Column }) {
  return <span className={`pill pill-${column}`}>{COLUMN_LABEL[column]}</span>;
}

/** Agent, branch and the labels that say how much it may do alone. */
export function TaskBadges({ t }: { t: TaskView }) {
  const mode = agentModeLabel(t.agent);
  return (
    <span className="badges">
      <span className={`agent agent-${t.agent}`}>@{t.agent}</span>
      <code className="branch">{t.branch}</code>
      {mode && (
        <span className="tag" title={mode.title}>
          {mode.text}
        </span>
      )}
      {isHibernated(t) && <span className="tag tag-muted">hibernated</span>}
      {t.queued > 0 && <span className="tag">{t.queued} queued</span>}
      {t.detail && <span className="tag tag-muted">{t.detail}</span>}
      {t.usage && <span className="tag tag-muted">{t.usage}</span>}
    </span>
  );
}
```

- [ ] **Step 8: Create `apps/cockpit/src/components/PermissionCard.tsx`**

```tsx
import { useState } from "react";
import { useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import { permissionScope } from "../lib/labels";
import { M, RpcCode } from "../lib/protocol";
import type { Perm } from "../store/state";

/** A pending permission request. Answers are one-time; there is no "always allow". */
export function PermissionCard({ perm, compact = false }: { perm: Perm; compact?: boolean }) {
  const { rpc } = useEnv();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const scope = permissionScope(perm.tool);
  const answer = async (allow: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await rpc(M.permissionAnswer, { id: perm.id, allow });
    } catch (e) {
      const conflict = e instanceof DaemonError && e.code === RpcCode.CONFLICT;
      setError(conflict ? "already answered" : String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className={`perm ${compact ? "perm-compact" : ""}`} role="group" aria-label={`Permission request: ${perm.tool}`}>
      <div className="perm-what">
        <strong>{perm.tool}</strong>
        <span className={`tag ${scope.text === "once" ? "" : "tag-warn"}`} title={scope.title}>
          {scope.text}
        </span>
        <code className="perm-detail">{perm.detail}</code>
      </div>
      <div className="perm-actions">
        <button className="btn btn-primary" disabled={busy} onClick={() => answer(true)}>
          Allow once
        </button>
        <button className="btn" disabled={busy} onClick={() => answer(false)}>
          Deny
        </button>
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
```

- [ ] **Step 9: Create `apps/cockpit/src/components/TrustCard.tsx`**

```tsx
import { useState } from "react";
import { useEnv } from "../env";
import { M } from "../lib/protocol";

/** Claude's workspace-trust dialog: only the user answers it (never auto-accepted). */
export function TrustCard({ taskId }: { taskId: string }) {
  const { rpc } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const answer = (accept: boolean) =>
    rpc(M.sessionTrust, { task: taskId, accept }).catch((e) => setError(e instanceof Error ? e.message : String(e)));
  return (
    <div className="perm" role="group" aria-label="Trust this folder">
      <div className="perm-what">
        <strong>Claude asks whether to trust this worktree folder.</strong>
        <span className="muted">Declining ends the session.</span>
      </div>
      <div className="perm-actions">
        <button className="btn btn-primary" onClick={() => answer(true)}>
          Trust folder
        </button>
        <button className="btn" onClick={() => answer(false)}>
          Decline
        </button>
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
```

- [ ] **Step 10: Create `apps/cockpit/src/components/useDiff.ts`**

```ts
import { useEffect, useState } from "react";
import { useEnv } from "../env";
import { M, type TaskDiff } from "../lib/protocol";

export type DiffState = { state: "loading" } | { state: "ready"; diff: TaskDiff } | { state: "error"; error: string };

/** `task.diff`, refetched when `version` changes (e.g. the number of finished turns). */
export function useDiff(taskId: string, version: number): DiffState {
  const { rpc } = useEnv();
  const [s, setS] = useState<DiffState>({ state: "loading" });
  useEffect(() => {
    let live = true;
    rpc<TaskDiff>(M.taskDiff, { task: taskId })
      .then((diff) => live && setS({ state: "ready", diff }))
      .catch((e) => live && setS({ state: "error", error: e instanceof Error ? e.message : String(e) }));
    return () => {
      live = false;
    };
  }, [rpc, taskId, version]);
  return s;
}

export function diffTotals(d: TaskDiff): { files: number; added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const f of d.files) {
    added += f.added ?? 0;
    removed += f.removed ?? 0;
  }
  return { files: d.files.length + d.untracked.length, added, removed };
}
```

- [ ] **Step 11: Create `apps/cockpit/src/components/DoneCard.tsx`**

```tsx
import { useEnv } from "../env";
import { diffTotals, useDiff } from "./useDiff";

/** A task whose turn ended: what changed, and the way to a pull request. */
export function DoneCard({ taskId, version, onOpen }: { taskId: string; version: number; onOpen(): void }) {
  const { openUrl } = useEnv();
  const d = useDiff(taskId, version);
  if (d.state === "loading") return <div className="done muted">Reading the diff…</div>;
  if (d.state === "error") return <div className="done error">Diff unavailable: {d.error}</div>;
  const t = diffTotals(d.diff);
  const url = d.diff.compare_url;
  return (
    <div className="done" aria-label="Turn finished">
      <span>
        {t.files} file{t.files === 1 ? "" : "s"} changed, <span className="add">+{t.added}</span>{" "}
        <span className="del">−{t.removed}</span> vs <code>{d.diff.base}</code>
      </span>
      <span className="done-actions">
        <button className="btn" onClick={onOpen}>
          Review diff
        </button>
        {url && (
          <button
            className="btn btn-primary"
            disabled={!d.diff.pushed}
            title={d.diff.pushed ? "Open GitHub's pull request page" : "Push the branch first (ask the agent, or git push)"}
            onClick={() => openUrl(url)}
          >
            Open PR
          </button>
        )}
      </span>
    </div>
  );
}
```

- [ ] **Step 12: Create `apps/cockpit/src/components/AgentCard.tsx`**

```tsx
import { useMemo } from "react";
import { useStore } from "../env";
import type { TaskView } from "../lib/protocol";
import { columnOf, lastLines, pendingPerms, turnsEnded } from "../store/select";
import type { FeedItem } from "../store/state";
import { StatusPill, TaskBadges } from "./Badges";
import { DoneCard } from "./DoneCard";
import { PermissionCard } from "./PermissionCard";
import { TrustCard } from "./TrustCard";

export function FeedLine({ item }: { item: FeedItem }) {
  const who = item.kind === "message" ? (item.mode === "queue" ? "you · queued" : "you · steer") : null;
  return (
    <li className={`line line-${item.kind} ${item.ok === false ? "line-failed" : ""}`}>
      {who && <span className="who">{who}</span>}
      <span className="text">{item.text}</span>
    </li>
  );
}

/** One task in the project feed: the dispatch, live progress, and whatever needs the user. */
export function AgentCard({ t, onOpen }: { t: TaskView; onOpen(id: string): void }) {
  const s = useStore();
  const feed = s.feed[t.id];
  const col = columnOf(t, feed);
  const perms = useMemo(() => pendingPerms(s, t.id), [s, t.id]);
  const lines = lastLines(feed, 5);
  return (
    <article className={`card card-${col}`} aria-label={`${t.agent} on ${t.branch}`}>
      <p className="dispatch">
        <span className="who">you</span> @{t.agent} <code>{t.branch}</code> {t.goal}
      </p>
      <header className="card-head">
        <button className="link" onClick={() => onOpen(t.id)}>
          {t.title}
        </button>
        <TaskBadges t={t} />
        <StatusPill column={col} />
      </header>
      {lines.length > 0 && (
        <ol className="lines" aria-live="polite">
          {lines.map((i) => (
            <FeedLine key={i.seq} item={i} />
          ))}
        </ol>
      )}
      {t.boot === "trust_prompt" && <TrustCard taskId={t.id} />}
      {perms.map((p) => (
        <PermissionCard key={p.id} perm={p} />
      ))}
      {col === "review" && <DoneCard taskId={t.id} version={turnsEnded(feed)} onOpen={() => onOpen(t.id)} />}
    </article>
  );
}
```

- [ ] **Step 13: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/AgentCard.test.tsx src/lib/labels.test.ts && pnpm --dir apps/cockpit typecheck`

Expected: exit 0; output contains `Tests  8 passed (8)`

- [ ] **Step 14: Commit**

```bash
git add apps/cockpit/src/env.tsx apps/cockpit/src/lib/labels.ts apps/cockpit/src/components/Badges.tsx apps/cockpit/src/components/PermissionCard.tsx apps/cockpit/src/components/TrustCard.tsx apps/cockpit/src/components/useDiff.ts apps/cockpit/src/components/DoneCard.tsx apps/cockpit/src/components/AgentCard.tsx apps/cockpit/src/lib/labels.test.ts apps/cockpit/src/test/env.tsx apps/cockpit/src/components/AgentCard.test.tsx
git commit -F- <<'EOF'
feat(cockpit): live agent card with one-time permission, trust and done cards

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 13: V3 home: rail, project feed, dispatch, board

The group-chat home: left rail (projects with needs-you badges, add-repo by absolute path, agents — clicking one starts `@agent <project>/` in the composer), the selected project's feed of agent cards with the dispatch composer (Enter dispatches: `task.create`, then `session.start` with the goal as first prompt, not awaited), and the right-rail board. The memory panel joins the rail in Task 19.

**Files:**
- Create: `apps/cockpit/src/components/DispatchComposer.tsx`
- Create: `apps/cockpit/src/components/Board.tsx`
- Create: `apps/cockpit/src/components/LeftRail.tsx`
- Create: `apps/cockpit/src/components/Home.tsx`
- Create: `apps/cockpit/src/components/Home.test.tsx`

- [ ] **Step 1: Create `apps/cockpit/src/components/Home.test.tsx`**

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import type { TaskDiff } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith } from "../test/env";
import { projects } from "../store/select";
import { Home } from "./Home";

const diff: TaskDiff = {
  base: "main",
  merge_base: "abc12345ff",
  files: [{ path: "src/a.ts", added: 10, removed: 2 }],
  untracked: ["notes.md"],
  patch: "+x",
  truncated: false,
  compare_url: "https://github.com/acme/shop/compare/main...feat/b?expand=1",
  pushed: true,
};

function Harness({ env }: { env: ReturnType<typeof makeEnv>["env"] }) {
  const [draft, setDraft] = useState("");
  return (
    <Home
      projects={projects(env.cockpit.getState(), ["/repos/shop"])}
      project="/repos/shop"
      onProject={() => {}}
      onReposChanged={() => {}}
      onOpen={() => {}}
      status={null}
      draft={draft}
      onDraft={setDraft}
    />
  );
}

function setup(answers = {}, tasks = [task("a", { status: "working" })], events = [] as ReturnType<typeof ev>[]) {
  const h = makeEnv(answers);
  h.load(tasks, events);
  renderWith(h.env, <Harness env={h.env} />);
  return h;
}

describe("Home (V3 group chat)", () => {
  it("dispatches @agent repo/branch goal: task.create, then session.start with the goal", async () => {
    const h = setup({ "task.create": task("new", { branch: "feat/login" }), "session.start": { boot: "booting" } });
    await userEvent.type(screen.getByLabelText("Dispatch an agent"), "@codex shop/feat/login add a login page{Enter}");
    await waitFor(() => expect(h.calls.map((c) => c[0])).toEqual(["task.create", "session.start"]));
    expect(h.calls[0][1]).toEqual({ repo: "/repos/shop", agent: "codex", branch: "feat/login", goal: "add a login page" });
    expect(h.calls[1][1]).toEqual({ task: "new", prompt: "add a login page" });
  });

  it("explains a bad dispatch without calling the daemon", async () => {
    const h = setup();
    await userEvent.type(screen.getByLabelText("Dispatch an agent"), "@claude web/x do it{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent('unknown repo "web"');
    expect(h.calls).toEqual([]);
  });

  it("the board groups tasks into Needs you / Working / Ready for review", () => {
    setup({ "task.diff": diff }, [task("n", { status: "needs_you" }), task("w", { status: "working" }), task("r")], [ev(1, "r", "turn_ended")]);
    const board = screen.getByRole("complementary", { name: "Board" });
    const col = (name: string) => within(within(board).getByRole("region", { name })).getAllByRole("button").map((b) => b.textContent);
    expect(col("Needs you")).toEqual(["goal of n@claude · feat/n"]);
    expect(col("Working")).toEqual(["goal of w@claude · feat/w"]);
    expect(col("Ready for review")).toEqual(["goal of r@claude · feat/r"]);
  });

});
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/Home.test.tsx`

Expected: FAIL — Failed to resolve import "./Home"

- [ ] **Step 3: Create `apps/cockpit/src/components/DispatchComposer.tsx`**

```tsx
import { type KeyboardEvent, useState } from "react";
import { useEnv } from "../env";
import { DISPATCH_HELP, parseDispatch, resolveRepo } from "../lib/dispatch";
import { M, type StartResult, type TaskView } from "../lib/protocol";
import type { Project } from "../store/select";

/** "@claude shop/feat/login add a login page" + Enter: new worktree, task and session. */
export function DispatchComposer(props: {
  projects: Project[];
  value: string;
  onChange(v: string): void;
  onDispatched?(taskId: string): void;
}) {
  const { rpc, cockpit } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    const p = parseDispatch(props.value);
    if (!p.ok) return setError(p.error);
    const repo = resolveRepo(p.value.repo, props.projects);
    if (!repo.ok) return setError(repo.error);
    const { agent, branch, goal } = p.value;
    setBusy(true);
    setError(null);
    try {
      const v = await rpc<TaskView>(M.taskCreate, { repo: repo.path, agent, branch, goal });
      cockpit.dispatch({ type: "task", view: v });
      props.onChange("");
      props.onDispatched?.(v.id);
      // Boot can take up to 45 s; progress arrives as events, so do not wait here.
      rpc<StartResult>(M.sessionStart, { task: v.id, prompt: goal }).catch((e) =>
        setError(`${branch} was created but the agent did not start: ${e instanceof Error ? e.message : e}`),
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void submit();
    }
  };

  return (
    <form
      className="composer"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <label htmlFor="dispatch" className="sr-only">
        Dispatch an agent
      </label>
      <textarea
        id="dispatch"
        rows={2}
        placeholder={DISPATCH_HELP}
        value={props.value}
        onChange={(e) => props.onChange(e.target.value)}
        onKeyDown={onKey}
        aria-describedby={error ? "dispatch-error" : undefined}
      />
      <button className="btn btn-primary" type="submit" disabled={busy || !props.value.trim()}>
        Dispatch
      </button>
      {error && (
        <p id="dispatch-error" className="error" role="alert">
          {error}
        </p>
      )}
    </form>
  );
}
```

- [ ] **Step 4: Create `apps/cockpit/src/components/Board.tsx`**

```tsx
import { useMemo } from "react";
import { useStore } from "../env";
import { BOARD, COLUMN_LABEL, tasksIn } from "../store/select";

/** Right rail: every project's tasks by what they need. */
export function Board({ onOpen }: { onOpen(id: string): void }) {
  const s = useStore();
  const cols = useMemo(() => BOARD.map((c) => ({ c, tasks: tasksIn(s, c) })), [s]);
  return (
    <aside className="board" aria-label="Board">
      {cols.map(({ c, tasks }) => (
        <section key={c} aria-label={COLUMN_LABEL[c]}>
          <h2>
            {COLUMN_LABEL[c]} <span className="count">{tasks.length}</span>
          </h2>
          <ul>
            {tasks.map((t) => (
              <li key={t.id}>
                <button className={`chip chip-${c}`} onClick={() => onOpen(t.id)}>
                  <span className="chip-title">{t.title}</span>
                  <span className="muted">
                    @{t.agent} · {t.branch}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </aside>
  );
}
```

- [ ] **Step 5: Create `apps/cockpit/src/components/LeftRail.tsx`**

```tsx
import { useMemo, useState } from "react";
import { useEnv, useStore } from "../env";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import { addRepo } from "../lib/repos";
import type { Project } from "../store/select";

export function LeftRail(props: {
  projects: Project[];
  selected: string | null;
  onSelect(path: string): void;
  onReposChanged(repos: string[]): void;
  onMention(agent: AgentKind): void;
  status: DaemonStatus | null;
}) {
  const { storage } = useEnv();
  const s = useStore();
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);
  const agents = useMemo(() => {
    const count = (a: AgentKind) => Object.values(s.tasks).filter((t) => t.agent === a && t.live).length;
    return [
      { agent: "claude" as const, live: count("claude") },
      { agent: "codex" as const, live: count("codex") },
    ];
  }, [s.tasks]);

  const add = () => {
    const r = addRepo(storage, path);
    if (!r.ok) return setError(r.error);
    setError(null);
    setPath("");
    props.onReposChanged(r.repos);
  };

  return (
    <nav className="rail" aria-label="Projects and agents">
      <h2>Projects</h2>
      <ul>
        {props.projects.map((p) => (
          <li key={p.path}>
            <button
              className={`rail-item ${props.selected === p.path ? "selected" : ""}`}
              aria-current={props.selected === p.path ? "page" : undefined}
              title={p.path}
              onClick={() => props.onSelect(p.path)}
            >
              <span>{p.name}</span>
              {p.needsYou > 0 && (
                <span className="badge" aria-label={`${p.needsYou} need you`}>
                  {p.needsYou}
                </span>
              )}
            </button>
          </li>
        ))}
      </ul>
      <form
        className="add-repo"
        onSubmit={(e) => {
          e.preventDefault();
          add();
        }}
      >
        <label htmlFor="repo-path" className="sr-only">
          Repository path
        </label>
        <input id="repo-path" placeholder="/path/to/repo" value={path} onChange={(e) => setPath(e.target.value)} />
        <button className="btn btn-small" type="submit">
          Add
        </button>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </form>
      <h2>Agents</h2>
      <ul>
        {agents.map((a) => (
          <li key={a.agent}>
            <button className="rail-item" onClick={() => props.onMention(a.agent)} title={`Start a message to @${a.agent}`}>
              <span>@{a.agent}</span>
              <span className="muted small">{a.live} live</span>
            </button>
          </li>
        ))}
      </ul>
    </nav>
  );
}
```

- [ ] **Step 6: Create `apps/cockpit/src/components/Home.tsx`**

```tsx
import { useMemo } from "react";
import { useStore } from "../env";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import { type Project, repoName } from "../store/select";
import { AgentCard } from "./AgentCard";
import { Board } from "./Board";
import { DispatchComposer } from "./DispatchComposer";
import { LeftRail } from "./LeftRail";

/** V3, the group chat: projects and agents | the project's feed | the board. */
export function Home(props: {
  projects: Project[];
  project: string | null;
  onProject(path: string): void;
  onReposChanged(repos: string[]): void;
  onOpen(taskId: string): void;
  status: DaemonStatus | null;
  draft: string;
  onDraft(v: string): void;
}) {
  const s = useStore();
  const cards = useMemo(
    () =>
      Object.values(s.tasks)
        .filter((t) => t.repo_path === props.project && t.lifecycle !== "archived")
        .sort((a, b) => a.created_at - b.created_at),
    [s.tasks, props.project],
  );
  const mention = (agent: AgentKind) => {
    const repo = props.project ? `${repoName(props.project)}/` : "";
    props.onDraft(`@${agent} ${repo}`);
    setTimeout(() => document.getElementById("dispatch")?.focus(), 0);
  };
  return (
    <div className="home">
      <LeftRail
        projects={props.projects}
        selected={props.project}
        onSelect={props.onProject}
        onReposChanged={props.onReposChanged}
        onMention={mention}
        status={props.status}
      />
      <main className="feed" aria-label="Project feed">
        <header className="feed-head">
          <h1>{props.project ? repoName(props.project) : "No project yet"}</h1>
          {props.project && <span className="muted small">{props.project}</span>}
        </header>
        <div className="cards">
          {cards.length === 0 && (
            <p className="muted empty">
              {props.project ? "No agents here yet. Dispatch one below." : "Add a repository on the left to start."}
            </p>
          )}
          {cards.map((t) => (
            <AgentCard key={t.id} t={t} onOpen={props.onOpen} />
          ))}
        </div>
        <DispatchComposer projects={props.projects} value={props.draft} onChange={props.onDraft} />
      </main>
      <Board onOpen={props.onOpen} />
    </div>
  );
}
```

- [ ] **Step 7: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/Home.test.tsx && pnpm --dir apps/cockpit typecheck`

Expected: exit 0; output contains `Tests  3 passed (3)`

- [ ] **Step 8: Commit**

```bash
git add apps/cockpit/src/components/DispatchComposer.tsx apps/cockpit/src/components/Board.tsx apps/cockpit/src/components/LeftRail.tsx apps/cockpit/src/components/Home.tsx apps/cockpit/src/components/Home.test.tsx
git commit -F- <<'EOF'
feat(cockpit): V3 home with project feed, dispatch composer and board

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


## Phase 3-iii — V1 detail and live terminal

### Task 14: Terminal tab: xterm.js over attach

xterm.js renders the bytes of `attach`; keystrokes go back through `attach_write` unless the stream is read-only (Codex); a ResizeObserver refits and sends `session.resize`; unmount detaches (the session keeps running).

**Files:**
- Create: `apps/cockpit/src/components/TerminalTab.tsx`
- Create: `apps/cockpit/src/components/TerminalTab.test.tsx`

- [ ] **Step 1: Create `apps/cockpit/src/components/TerminalTab.test.tsx`**

```tsx
import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  written: [] as Uint8Array[],
  onData: null as ((d: string) => void) | null,
  disposed: false,
  attachOpen: vi.fn(),
  attachWrite: vi.fn(async () => {}),
  attachClose: vi.fn(async () => {}),
}));

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    rows = 30;
    cols = 100;
    loadAddon() {}
    open() {}
    write(b: Uint8Array) {
      h.written.push(b);
    }
    onData(fn: (d: string) => void) {
      h.onData = fn;
    }
    dispose() {
      h.disposed = true;
    }
  },
}));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class { fit() {} } }));
vi.mock("../lib/daemon", () => ({ attachOpen: h.attachOpen, attachWrite: h.attachWrite, attachClose: h.attachClose }));

import { EnvContext } from "../env";
import { makeEnv } from "../test/env";
import { task } from "../test/fixtures";
import { TerminalTab } from "./TerminalTab";

globalThis.ResizeObserver = class {
  observe() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

describe("TerminalTab", () => {
  beforeEach(() => {
    h.written = [];
    h.onData = null;
    h.disposed = false;
    h.attachOpen.mockReset();
    h.attachWrite.mockClear();
    h.attachClose.mockClear();
  });

  it("attaches at the terminal's size, writes output, sends keys, detaches on unmount", async () => {
    h.attachOpen.mockImplementation(async (_t: string, _r: number, _c: number, onData: (b: Uint8Array) => void) => {
      onData(new TextEncoder().encode("hi"));
      return { id: 4, input: true };
    });
    const { env } = makeEnv();
    const r = render(
      <EnvContext.Provider value={env}>
        <TerminalTab t={task("a")} />
      </EnvContext.Provider>,
    );
    await waitFor(() => expect(h.onData).not.toBeNull());
    expect(h.attachOpen.mock.calls[0].slice(0, 3)).toEqual(["a", 30, 100]);
    expect(new TextDecoder().decode(h.written[0])).toBe("hi");
    h.onData!("ls\r");
    expect(h.attachWrite).toHaveBeenCalledWith(4, "ls\r");
    r.unmount();
    expect(h.attachClose).toHaveBeenCalledWith(4);
    expect(h.disposed).toBe(true);
  });

  it("a read-only stream says so and drops keys", async () => {
    h.attachOpen.mockResolvedValue({ id: 5, input: false });
    const { env } = makeEnv();
    render(
      <EnvContext.Provider value={env}>
        <TerminalTab t={task("c", { agent: "codex" })} />
      </EnvContext.Provider>,
    );
    expect(await screen.findByRole("status")).toHaveTextContent("read-only stream");
    h.onData!("x");
    expect(h.attachWrite).not.toHaveBeenCalled();
  });

  it("no live session, no attach", () => {
    const { env } = makeEnv();
    render(
      <EnvContext.Provider value={env}>
        <TerminalTab t={task("a", { live: false })} />
      </EnvContext.Provider>,
    );
    expect(screen.getByText(/No live session/)).toBeInTheDocument();
    expect(h.attachOpen).not.toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/TerminalTab.test.tsx`

Expected: FAIL — Failed to resolve import "./TerminalTab"

- [ ] **Step 3: Create `apps/cockpit/src/components/TerminalTab.tsx`**

```tsx
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";
import { useEnv } from "../env";
import { attachClose, attachOpen, attachWrite } from "../lib/daemon";
import { M, type TaskView } from "../lib/protocol";

/** The task's live PTY over `attach`; keystrokes go back unless the stream is read-only. */
export function TerminalTab({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const host = useRef<HTMLDivElement>(null);
  const [note, setNote] = useState<string | null>(null);

  useEffect(() => {
    if (!t.live || !host.current) return;
    const term = new Terminal({ convertEol: false, cursorBlink: true, fontSize: 13, scrollback: 5000 });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host.current);
    fit.fit();
    let id: number | null = null;
    let closed = false;
    const resize = () => {
      fit.fit();
      if (id !== null) rpc(M.sessionResize, { task: t.id, rows: term.rows, cols: term.cols }).catch(() => {});
    };
    const ro = new ResizeObserver(resize);
    ro.observe(host.current);
    attachOpen(
      t.id,
      term.rows,
      term.cols,
      (bytes) => term.write(bytes),
      () => setNote("stream ended"),
    )
      .then((a) => {
        if (closed) {
          void attachClose(a.id);
          return;
        }
        id = a.id;
        if (!a.input) setNote("read-only stream (this agent takes messages from the composer)");
        term.onData((data) => {
          if (a.input) attachWrite(a.id, data).catch(() => setNote("input not delivered"));
        });
      })
      .catch((e) => setNote(e instanceof Error ? e.message : String(e)));
    return () => {
      closed = true;
      ro.disconnect();
      if (id !== null) void attachClose(id);
      term.dispose();
    };
  }, [rpc, t.id, t.live]);

  if (!t.live) return <p className="muted">No live session. Resume the task to see its terminal.</p>;
  return (
    <div className="terminal-wrap">
      {note && (
        <p className="small muted" role="status">
          {note}
        </p>
      )}
      <div className="terminal" ref={host} aria-label={`Terminal of ${t.branch}`} />
    </div>
  );
}
```

- [ ] **Step 4: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/TerminalTab.test.tsx`

Expected: exit 0; output contains `Tests  3 passed (3)`

- [ ] **Step 5: Commit**

```bash
git add apps/cockpit/src/components/TerminalTab.tsx apps/cockpit/src/components/TerminalTab.test.tsx
git commit -F- <<'EOF'
feat(cockpit): live terminal tab over the attach stream

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 15: V1 detail: task list, tabs, steer/queue composer

Tasks grouped by state on the left; the selected task with its pending cards, tabs Steps / Terminal / Diff / Memory (arrow keys move between tabs; Memory is a placeholder until the memory layer), session actions (Interrupt, Stop, Resume) and the composer: **Steer now** / **Queue** (⌘↵ / ⌥↵). An UNSUPPORTED steer offers "Interrupt and send" (spec: fall back to cancel + re-prompt). The Hibernate button joins the header in Task 19.

**Files:**
- Create: `apps/cockpit/src/components/MessageComposer.tsx`
- Create: `apps/cockpit/src/components/DiffTab.tsx`
- Create: `apps/cockpit/src/components/Detail.tsx`
- Create: `apps/cockpit/src/components/Detail.test.tsx`

- [ ] **Step 1: Create `apps/cockpit/src/components/Detail.test.tsx`**

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

vi.mock("./TerminalTab", () => ({ TerminalTab: () => <div>terminal here</div> }));

import { RpcCode } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { Detail } from "./Detail";

function setup(answers = {}, tasks = [task("a", { status: "working" }), task("b", { status: "needs_you" })], events = [ev(1, "a", "step", { type: "command", command: "npm test", exit_code: 1 })]) {
  const h = makeEnv(answers);
  h.load(tasks, events);
  const onSelect = vi.fn();
  renderWith(h.env, <Detail taskId="a" onSelect={onSelect} />);
  return { ...h, onSelect };
}

describe("Detail (V1 classic)", () => {
  it("lists tasks grouped by state and selects one", async () => {
    const h = setup();
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    expect(within(nav).getAllByRole("heading").map((x) => x.textContent)).toEqual(["Needs you", "Working"]);
    await userEvent.click(within(nav).getByRole("button", { name: /goal of b/ }));
    expect(h.onSelect).toHaveBeenCalledWith("b");
  });

  it("shows steps, switches tabs with arrows, and has a memory placeholder", async () => {
    setup();
    expect(screen.getByText("$ npm test")).toBeInTheDocument();
    screen.getByRole("tab", { name: "Steps" }).focus();
    await userEvent.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("terminal here")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("tab", { name: "Memory" }));
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Repo memory arrives with the memory layer");
  });

  it("Steer now and Queue send with their mode", async () => {
    const h = setup({ "message.send": (p: Record<string, unknown>) => ({ delivered: p.mode, queued: p.mode === "queue" ? 1 : 0 }) });
    const box = screen.getByLabelText("Message to the agent");
    await userEvent.type(box, "use the v2 API");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    expect(h.calls).toContainEqual(["message.send", { task: "a", text: "use the v2 API", mode: "steer" }]);
    expect(await screen.findByText("sent now")).toBeInTheDocument();
    await userEvent.type(box, "then update docs");
    await userEvent.click(screen.getByRole("button", { name: "Queue" }));
    expect(await screen.findByText("queued (#1, delivered when the turn ends)")).toBeInTheDocument();
  });

  it("an agent that cannot be steered offers interrupt-and-send", async () => {
    const h = setup({ "message.send": (p: Record<string, unknown>) => (p.mode === "steer" ? rpcError(RpcCode.UNSUPPORTED, "cannot steer") : { delivered: "queue", queued: 1 }), "session.interrupt": {} });
    await userEvent.type(screen.getByLabelText("Message to the agent"), "stop");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    await userEvent.click(await screen.findByRole("button", { name: "Interrupt and send" }));
    await waitFor(() => expect(h.calls.map((c) => c[0])).toEqual(["message.send", "session.interrupt", "message.send"]));
    expect(h.calls[2][1]).toMatchObject({ mode: "queue", text: "stop" });
  });

  it("a stopped task offers Resume instead of the composer", async () => {
    const h = setup({ "session.resume": {} }, [task("a", { live: false, boot: null, status: "ended" })], []);
    expect(screen.getByText(/No live session/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Resume" }));
    expect(h.calls).toContainEqual(["session.resume", { task: "a" }]);
  });
});
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/Detail.test.tsx`

Expected: FAIL — Failed to resolve import "./Detail"

- [ ] **Step 3: Create `apps/cockpit/src/components/MessageComposer.tsx`**

```tsx
import { useState } from "react";
import { useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import { M, RpcCode, type SendMode, type SendResult, type TaskView } from "../lib/protocol";

/**
 * Mid-run messages. "Steer now" reaches the agent at its next step (Claude: next tool
 * boundary; Codex: turn/steer); "Queue" is held by the daemon until the turn ends. When the
 * agent cannot be steered the daemon says so (UNSUPPORTED) and the fallback is offered:
 * interrupt, then send as a new prompt.
 */
export function MessageComposer({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const [text, setText] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const [fallback, setFallback] = useState(false);

  const send = async (mode: SendMode) => {
    if (!text.trim()) return;
    setNote(null);
    setFallback(false);
    try {
      const r = await rpc<SendResult>(M.messageSend, { task: t.id, text, mode });
      setText("");
      setNote(r.delivered === "steer" ? "sent now" : `queued (#${r.queued}, delivered when the turn ends)`);
    } catch (e) {
      const err = e instanceof DaemonError ? e : null;
      if (err?.code === RpcCode.UNSUPPORTED) setFallback(true);
      setNote(err?.code === RpcCode.NEEDS_YOU ? "answer the open request first" : String(err?.message ?? e));
    }
  };

  const interruptThenSend = async () => {
    try {
      await rpc(M.sessionInterrupt, { task: t.id });
      await send("queue");
    } catch (e) {
      setNote(e instanceof Error ? e.message : String(e));
    }
  };

  if (!t.live) return <p className="muted small composer-off">No live session: resume it to talk to the agent.</p>;
  return (
    <div className="composer">
      <label htmlFor={`msg-${t.id}`} className="sr-only">
        Message to the agent
      </label>
      <textarea
        id={`msg-${t.id}`}
        rows={2}
        value={text}
        placeholder="Message the agent (⌘↵ steer now, ⌥↵ queue)"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            void send("steer");
          } else if (e.key === "Enter" && e.altKey) {
            e.preventDefault();
            void send("queue");
          }
        }}
      />
      <div className="composer-actions">
        <button className="btn btn-primary" onClick={() => send("steer")} disabled={!text.trim()}>
          Steer now
        </button>
        <button className="btn" onClick={() => send("queue")} disabled={!text.trim()}>
          Queue
        </button>
        {fallback && (
          <button className="btn btn-warn" onClick={interruptThenSend}>
            Interrupt and send
          </button>
        )}
      </div>
      {note && (
        <p className="small muted" role="status">
          {note}
        </p>
      )}
    </div>
  );
}
```

- [ ] **Step 4: Create `apps/cockpit/src/components/DiffTab.tsx`**

```tsx
import { useEnv } from "../env";
import { diffTotals, useDiff } from "./useDiff";

function lineClass(l: string): string {
  if (l.startsWith("+++") || l.startsWith("---") || l.startsWith("diff ") || l.startsWith("index ")) return "d-meta";
  if (l.startsWith("@@")) return "d-hunk";
  if (l.startsWith("+")) return "d-add";
  if (l.startsWith("-")) return "d-del";
  return "";
}

export function DiffTab({ taskId, version }: { taskId: string; version: number }) {
  const { openUrl } = useEnv();
  const d = useDiff(taskId, version);
  if (d.state === "loading") return <p className="muted">Reading the diff…</p>;
  if (d.state === "error") return <p className="error">Diff unavailable: {d.error}</p>;
  const { diff } = d;
  const t = diffTotals(diff);
  return (
    <div className="diff">
      <p>
        {t.files} files, <span className="add">+{t.added}</span> <span className="del">−{t.removed}</span> against{" "}
        <code>{diff.base}</code> ({diff.merge_base.slice(0, 8)})
        {diff.compare_url && (
          <button className="btn btn-small" disabled={!diff.pushed} onClick={() => openUrl(diff.compare_url!)}>
            {diff.pushed ? "Open PR" : "Open PR (push first)"}
          </button>
        )}
      </p>
      <ul className="diff-files">
        {diff.files.map((f) => (
          <li key={f.path}>
            <code>{f.path}</code>{" "}
            {f.added === null ? <span className="muted">binary</span> : <span className="add">+{f.added}</span>}{" "}
            {f.removed !== null && <span className="del">−{f.removed}</span>}
          </li>
        ))}
        {diff.untracked.map((p) => (
          <li key={p}>
            <code>{p}</code> <span className="muted">new, untracked</span>
          </li>
        ))}
      </ul>
      <pre className="patch" aria-label="Patch">
        {diff.patch.split("\n").map((l, i) => (
          <div key={i} className={lineClass(l)}>
            {l || " "}
          </div>
        ))}
      </pre>
      {diff.truncated && <p className="muted small">Patch cut at 512 KiB.</p>}
    </div>
  );
}
```

- [ ] **Step 5: Create `apps/cockpit/src/components/Detail.tsx`**

```tsx
import { type KeyboardEvent, useMemo, useState } from "react";
import { useEnv, useStore } from "../env";
import { M, type TaskView } from "../lib/protocol";
import { columnOf, COLUMN_LABEL, type Column, pendingPerms, turnsEnded } from "../store/select";
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { DiffTab } from "./DiffTab";
import { MessageComposer } from "./MessageComposer";
import { PermissionCard } from "./PermissionCard";
import { TerminalTab } from "./TerminalTab";
import { TrustCard } from "./TrustCard";

const GROUPS: Column[] = ["needs_you", "working", "review", "idle"];
const TABS = ["Steps", "Terminal", "Diff", "Memory"] as const;
type Tab = (typeof TABS)[number];

function TaskList({ selected, onSelect }: { selected: string; onSelect(id: string): void }) {
  const s = useStore();
  const groups = useMemo(
    () =>
      GROUPS.map((g) => ({
        g,
        tasks: Object.values(s.tasks)
          .filter((t) => columnOf(t, s.feed[t.id]) === g)
          .sort((a, b) => b.updated_at - a.updated_at),
      })).filter((x) => x.tasks.length > 0),
    [s],
  );
  return (
    <nav className="tasklist" aria-label="Tasks">
      {groups.map(({ g, tasks }) => (
        <section key={g}>
          <h2>{COLUMN_LABEL[g]}</h2>
          <ul>
            {tasks.map((t) => (
              <li key={t.id}>
                <button
                  className={`rail-item ${t.id === selected ? "selected" : ""}`}
                  aria-current={t.id === selected ? "page" : undefined}
                  onClick={() => onSelect(t.id)}
                >
                  <span className="chip-title">{t.title}</span>
                  <span className="muted small">
                    @{t.agent} · {t.branch}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </nav>
  );
}

function Actions({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const run = (method: string) => rpc(method, { task: t.id }).catch((e) => setError(e instanceof Error ? e.message : String(e)));
  return (
    <span className="actions">
      {t.live ? (
        <>
          <button className="btn btn-small" onClick={() => run(M.sessionInterrupt)}>
            Interrupt
          </button>
          <button className="btn btn-small" onClick={() => run(M.sessionStop)}>
            Stop
          </button>
        </>
      ) : (
        <button className="btn btn-small" onClick={() => run(M.sessionResume)}>
          Resume
        </button>
      )}
      {error && (
        <span className="error" role="alert">
          {error}
        </span>
      )}
    </span>
  );
}

/** V1, the classic detail: tasks by state | one task with Steps / Terminal / Diff / Memory. */
export function Detail({ taskId, onSelect }: { taskId: string; onSelect(id: string): void }) {
  const s = useStore();
  const [tab, setTab] = useState<Tab>("Steps");
  const t = s.tasks[taskId];
  const feed = s.feed[taskId] ?? [];
  const perms = useMemo(() => pendingPerms(s, taskId), [s, taskId]);
  const onTabKey = (e: KeyboardEvent) => {
    const i = TABS.indexOf(tab);
    if (e.key === "ArrowRight") setTab(TABS[(i + 1) % TABS.length]);
    if (e.key === "ArrowLeft") setTab(TABS[(i + TABS.length - 1) % TABS.length]);
  };
  return (
    <div className="detail">
      <TaskList selected={taskId} onSelect={onSelect} />
      {!t ? (
        <main className="detail-main">
          <p className="muted">This task is gone.</p>
        </main>
      ) : (
        <main className="detail-main" aria-label={`Task ${t.title}`}>
          <header className="detail-head">
            <h1>{t.title}</h1>
            <TaskBadges t={t} />
            <StatusPill column={columnOf(t, feed)} />
            <Actions t={t} />
          </header>
          {t.boot === "trust_prompt" && <TrustCard taskId={t.id} />}
          {perms.map((p) => (
            <PermissionCard key={p.id} perm={p} />
          ))}
          <div role="tablist" aria-label="Task views" className="tabs" onKeyDown={onTabKey}>
            {TABS.map((name) => (
              <button
                key={name}
                role="tab"
                id={`tab-${name}`}
                aria-selected={tab === name}
                aria-controls={`panel-${name}`}
                tabIndex={tab === name ? 0 : -1}
                onClick={() => setTab(name)}
              >
                {name}
              </button>
            ))}
          </div>
          <section role="tabpanel" id={`panel-${tab}`} aria-labelledby={`tab-${tab}`} className="panel">
            {tab === "Steps" &&
              (feed.length === 0 ? (
                <p className="muted">No steps yet.</p>
              ) : (
                <ol className="lines lines-full">
                  {feed.map((i) => (
                    <FeedLine key={i.seq} item={i} />
                  ))}
                </ol>
              ))}
            {tab === "Terminal" && <TerminalTab t={t} />}
            {tab === "Diff" && <DiffTab taskId={t.id} version={turnsEnded(feed)} />}
            {tab === "Memory" && (
              <p className="muted">
                Repo memory arrives with the memory layer (a later plan). Goal: <q>{t.goal}</q>
                {t.notes && <> · Notes: {t.notes}</>}
              </p>
            )}
          </section>
          <MessageComposer t={t} />
        </main>
      )}
    </div>
  );
}
```

- [ ] **Step 6: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/Detail.test.tsx && pnpm --dir apps/cockpit typecheck`

Expected: exit 0; output contains `Tests  5 passed (5)`

- [ ] **Step 7: Commit**

```bash
git add apps/cockpit/src/components/MessageComposer.tsx apps/cockpit/src/components/DiffTab.tsx apps/cockpit/src/components/Detail.tsx apps/cockpit/src/components/Detail.test.tsx
git commit -F- <<'EOF'
feat(cockpit): V1 detail with steps, terminal, diff and steer/queue composer

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


## Phase 3-iv — V2 grid, shortcuts, notifications, memory guard UI

### Task 16: V2 agent grid

2–6 tiles: needs-you first, then working, then recently finished. Each tile: last four lines, status, inline Allow once / Deny for the first pending card, trust card, and a mini input (Enter steers, ⌥Enter queues).

**Files:**
- Create: `apps/cockpit/src/components/Grid.tsx`
- Create: `apps/cockpit/src/components/Grid.test.tsx`

- [ ] **Step 1: Create `apps/cockpit/src/components/Grid.test.tsx`**

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith } from "../test/env";
import { Grid } from "./Grid";

describe("Grid (V2)", () => {
  it("shows live tiles with last lines, inline allow and a mini input", async () => {
    const h = makeEnv({ "permission.answer": {}, "message.send": {} });
    h.load([task("a", { status: "working" }), task("b", { status: "needs_you" }), task("idle", { live: false, boot: null })], [
      ev(1, "a", "step", { type: "agent_message", text: "refactoring the cart" }),
      ev(2, "b", "permission_requested", { permission_id: "p9", tool: "command", detail: "npm publish" }),
    ]);
    renderWith(h.env, <Grid onOpen={() => {}} />);
    const tiles = screen.getAllByRole("article");
    expect(tiles.map((t) => t.getAttribute("aria-label"))).toEqual(["claude on feat/b", "claude on feat/a"]);
    expect(within(tiles[1]).getByText("refactoring the cart")).toBeInTheDocument();
    await userEvent.click(within(tiles[0]).getByRole("button", { name: "Allow once" }));
    expect(h.calls).toContainEqual(["permission.answer", { id: "p9", allow: true }]);
    await userEvent.type(within(tiles[1]).getByRole("textbox"), "add tests{Alt>}{Enter}{/Alt}");
    expect(h.calls).toContainEqual(["message.send", { task: "a", text: "add tests", mode: "queue" }]);
  });
});
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/Grid.test.tsx`

Expected: FAIL — Failed to resolve import "./Grid"

- [ ] **Step 3: Create `apps/cockpit/src/components/Grid.tsx`**

```tsx
import { useMemo, useState } from "react";
import { useEnv, useStore } from "../env";
import { M, type TaskView } from "../lib/protocol";
import { columnOf, gridTasks, lastLines, pendingPerms } from "../store/select";
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { PermissionCard } from "./PermissionCard";
import { TrustCard } from "./TrustCard";

function Tile({ t, onOpen }: { t: TaskView; onOpen(id: string): void }) {
  const s = useStore();
  const { rpc } = useEnv();
  const [text, setText] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const perm = useMemo(() => pendingPerms(s, t.id)[0], [s, t.id]);
  const send = (mode: "steer" | "queue") =>
    rpc(M.messageSend, { task: t.id, text, mode })
      .then(() => {
        setText("");
        setNote(mode === "steer" ? "sent" : "queued");
      })
      .catch((e) => setNote(e instanceof Error ? e.message : String(e)));
  return (
    <article className={`tile card-${columnOf(t, s.feed[t.id])}`} aria-label={`${t.agent} on ${t.branch}`}>
      <header className="card-head">
        <button className="link" onClick={() => onOpen(t.id)}>
          {t.title}
        </button>
        <StatusPill column={columnOf(t, s.feed[t.id])} />
      </header>
      <TaskBadges t={t} />
      <ol className="lines">
        {lastLines(s.feed[t.id], 4).map((i) => (
          <FeedLine key={i.seq} item={i} />
        ))}
      </ol>
      {t.boot === "trust_prompt" && <TrustCard taskId={t.id} />}
      {perm && <PermissionCard perm={perm} compact />}
      {t.live && (
        <input
          className="mini"
          aria-label={`Message ${t.branch} (Enter steer, Alt+Enter queue)`}
          placeholder="Enter steer · ⌥↵ queue"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && text.trim()) {
              e.preventDefault();
              void send(e.altKey ? "queue" : "steer");
            }
          }}
        />
      )}
      <footer className="tile-foot">
        {note && <span className="small muted">{note}</span>}
      </footer>
    </article>
  );
}

/** V2, the agent grid: 2–6 live tiles. */
export function Grid({ onOpen }: { onOpen(id: string): void }) {
  const s = useStore();
  const tiles = useMemo(() => gridTasks(s), [s]);
  if (tiles.length === 0) return <main className="grid-empty muted">No active agents. Dispatch one from Home (⌘1).</main>;
  return (
    <main className={`grid grid-${Math.max(2, tiles.length)}`} aria-label="Agent grid">
      {tiles.map((t) => (
        <Tile key={t.id} t={t} onOpen={onOpen} />
      ))}
    </main>
  );
}
```

- [ ] **Step 4: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/Grid.test.tsx`

Expected: exit 0; output contains `Tests  1 passed (1)`

- [ ] **Step 5: Commit**

```bash
git add apps/cockpit/src/components/Grid.tsx apps/cockpit/src/components/Grid.test.tsx
git commit -F- <<'EOF'
feat(cockpit): V2 agent grid with inline answers and mini input

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 17: Shortcuts, theme and the ⌘K command bar

`mod` is ⌘ on macOS and Ctrl elsewhere: ⌘K command bar, ⌘G grid toggle, ⌘1 home, Esc leaves the detail, ⇧⌘J cycles the theme (system → light → dark; "system" follows `prefers-color-scheme`). The command bar filters commands, projects and tasks; text starting with `@` becomes "Dispatch: …".

**Files:**
- Create: `apps/cockpit/src/lib/shortcuts.ts`
- Create: `apps/cockpit/src/lib/shortcuts.test.ts`
- Create: `apps/cockpit/src/lib/theme.ts`
- Create: `apps/cockpit/src/lib/theme.test.ts`
- Create: `apps/cockpit/src/components/CommandBar.tsx`

- [ ] **Step 1: Create `apps/cockpit/src/lib/shortcuts.test.ts`**

```ts
import { describe, expect, it } from "vitest";
import { shortcutFor } from "./shortcuts";

const k = (key: string, o: Partial<KeyboardEvent> = {}) => ({ key, metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...o });

describe("shortcutFor", () => {
  it("uses ⌘ on macOS and Ctrl elsewhere", () => {
    expect(shortcutFor(k("k", { metaKey: true }), true)).toBe("palette");
    expect(shortcutFor(k("k", { ctrlKey: true }), true)).toBeNull();
    expect(shortcutFor(k("g", { ctrlKey: true }), false)).toBe("grid");
    expect(shortcutFor(k("1", { metaKey: true }), true)).toBe("home");
    expect(shortcutFor(k("J", { metaKey: true, shiftKey: true }), true)).toBe("theme");
  });

  it("plain keys and Alt combos are left to inputs; Escape always counts", () => {
    expect(shortcutFor(k("g"), true)).toBeNull();
    expect(shortcutFor(k("k", { metaKey: true, altKey: true }), true)).toBeNull();
    expect(shortcutFor(k("Escape"), true)).toBe("escape");
  });
});
```

- [ ] **Step 2: Create `apps/cockpit/src/lib/theme.test.ts`**

```ts
import { describe, expect, it } from "vitest";
import { applyTheme, loadTheme, nextTheme, THEME_KEY } from "./theme";

describe("theme", () => {
  it("cycles system -> light -> dark -> system and pins via data-theme", () => {
    expect([nextTheme("system"), nextTheme("light"), nextTheme("dark")]).toEqual(["light", "dark", "system"]);
    const el = document.createElement("html");
    applyTheme("dark", el);
    expect(el.dataset.theme).toBe("dark");
    applyTheme("system", el);
    expect(el.dataset.theme).toBeUndefined();
  });

  it("loads only known values", () => {
    const store = (v: string | null) => ({ getItem: (k: string) => (k === THEME_KEY ? v : null), setItem: () => {} });
    expect(loadTheme(store("dark"))).toBe("dark");
    expect(loadTheme(store("neon"))).toBe("system");
  });
});
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/lib/shortcuts.test.ts src/lib/theme.test.ts`

Expected: FAIL — Failed to resolve import "./shortcuts"

- [ ] **Step 4: Create `apps/cockpit/src/lib/shortcuts.ts`**

```ts
// App-wide keys. "mod" is ⌘ on macOS and Ctrl elsewhere.
export type ShortcutAction = "palette" | "grid" | "home" | "escape" | "theme";

export function shortcutFor(e: Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey">, mac: boolean): ShortcutAction | null {
  const mod = mac ? e.metaKey : e.ctrlKey;
  if (e.key === "Escape") return "escape";
  if (!mod || e.altKey) return null;
  switch (e.key.toLowerCase()) {
    case "k":
      return "palette";
    case "g":
      return "grid";
    case "1":
      return "home";
    case "j":
      return e.shiftKey ? "theme" : null;
    default:
      return null;
  }
}

export const SHORTCUT_HINTS: Record<Exclude<ShortcutAction, "escape">, string> = {
  palette: "⌘K",
  grid: "⌘G",
  home: "⌘1",
  theme: "⇧⌘J",
};
```

- [ ] **Step 5: Create `apps/cockpit/src/lib/theme.ts`**

```ts
// "system" follows the OS (prefers-color-scheme); light / dark pin it. Per machine.
export type Theme = "system" | "light" | "dark";
export const THEME_KEY = "mirante.theme";

type KV = Pick<Storage, "getItem" | "setItem">;

export function loadTheme(store: KV): Theme {
  const v = store.getItem(THEME_KEY);
  return v === "light" || v === "dark" ? v : "system";
}

export function nextTheme(t: Theme): Theme {
  return t === "system" ? "light" : t === "light" ? "dark" : "system";
}

export function applyTheme(t: Theme, root: HTMLElement = document.documentElement): void {
  if (t === "system") delete root.dataset.theme;
  else root.dataset.theme = t;
}
```

- [ ] **Step 6: Create `apps/cockpit/src/components/CommandBar.tsx`**

```tsx
import { useEffect, useMemo, useRef, useState } from "react";

export interface Command {
  id: string;
  label: string;
  hint?: string;
  run(): void;
}

/** ⌘K: filter commands by text; "@agent repo/branch goal" becomes a dispatch. */
export function CommandBar(props: { commands: Command[]; onDispatch(text: string): void; onClose(): void }) {
  const [q, setQ] = useState("");
  const [i, setI] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.focus(), []);
  const items = useMemo(() => {
    const needle = q.trim().toLowerCase();
    const hits = props.commands.filter((c) => c.label.toLowerCase().includes(needle));
    if (q.trim().startsWith("@")) {
      return [{ id: "dispatch", label: `Dispatch: ${q.trim()}`, run: () => props.onDispatch(q.trim()) }, ...hits];
    }
    return hits;
  }, [q, props]);
  const run = (c: Command | undefined) => {
    if (!c) return;
    props.onClose();
    c.run();
  };
  return (
    <div className="overlay" onMouseDown={props.onClose}>
      <div className="palette" role="dialog" aria-modal="true" aria-label="Command bar" onMouseDown={(e) => e.stopPropagation()}>
        <input
          ref={input}
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-list"
          aria-activedescendant={items[i] ? `cmd-${items[i].id}` : undefined}
          placeholder="Type a command, a task, or @claude repo/branch goal"
          value={q}
          onChange={(e) => {
            setQ(e.target.value);
            setI(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") props.onClose();
            if (e.key === "ArrowDown") setI((x) => Math.min(x + 1, items.length - 1));
            if (e.key === "ArrowUp") setI((x) => Math.max(x - 1, 0));
            if (e.key === "Enter") run(items[i]);
          }}
        />
        <ul id="palette-list" role="listbox">
          {items.map((c, n) => (
            <li
              key={c.id}
              id={`cmd-${c.id}`}
              role="option"
              aria-selected={n === i}
              className={n === i ? "active" : ""}
              onMouseEnter={() => setI(n)}
              onClick={() => run(c)}
            >
              <span>{c.label}</span>
              {c.hint && <kbd>{c.hint}</kbd>}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
```

- [ ] **Step 7: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/lib/shortcuts.test.ts src/lib/theme.test.ts && pnpm --dir apps/cockpit typecheck`

Expected: exit 0; output contains `Tests  4 passed (4)`

- [ ] **Step 8: Commit**

```bash
git add apps/cockpit/src/lib/shortcuts.ts apps/cockpit/src/lib/shortcuts.test.ts apps/cockpit/src/lib/theme.ts apps/cockpit/src/lib/theme.test.ts apps/cockpit/src/components/CommandBar.tsx
git commit -F- <<'EOF'
feat(cockpit): keyboard shortcuts, theme cycling and the command bar

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 18: App shell and notifications

The three modes of one screen, the connection banner (with **Start daemon**), `daemon.status` polling every 5 s while connected (memory panel), theme tokens for light/dark, and notifications: when a task newly needs the user, `shouldNotify` stays quiet only if that task is on screen (home: the selected project's cards; detail: the task; grid: the tiles) **and** the window has focus. The first snapshot never notifies (it is what the user left, not news). `main.tsx` becomes the real entry.

**Files:**
- Create: `apps/cockpit/src/lib/notify.ts`
- Create: `apps/cockpit/src/lib/notify.test.ts`
- Create: `apps/cockpit/src/App.tsx`
- Create: `apps/cockpit/src/App.test.tsx`
- Create: `apps/cockpit/src/styles.css`
- Modify: `apps/cockpit/src/main.tsx` (replace)

- [ ] **Step 1: Create `apps/cockpit/src/lib/notify.test.ts`**

```ts
import { describe, expect, it } from "vitest";
import { task } from "../test/fixtures";
import { notificationFor, shouldNotify } from "./notify";

describe("shouldNotify", () => {
  it("stays quiet only when the task is on screen in the focused window", () => {
    const visible = new Set(["a"]);
    expect(shouldNotify("a", visible, true)).toBe(false);
    expect(shouldNotify("a", visible, false)).toBe(true);
    expect(shouldNotify("b", visible, true)).toBe(true);
  });

  it("says why", () => {
    expect(notificationFor(task("a", { boot: "trust_prompt" })).title).toBe("feat/a asks to trust its folder");
    expect(notificationFor(task("a", { status: "needs_you" }))).toEqual({ title: "feat/a needs you", body: "goal of a" });
  });
});
```

- [ ] **Step 2: Create `apps/cockpit/src/App.test.tsx`**

```tsx
import { act, fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

vi.mock("./components/TerminalTab", () => ({ TerminalTab: () => null }));

import { App, visibleTasks } from "./App";
import { ev, task } from "./test/fixtures";
import { makeEnv } from "./test/env";
import { render } from "@testing-library/react";

function setup() {
  const h = makeEnv({ "daemon.status": { pid: 1, boot_id: "b1", version: "x", live_sessions: 0 }, "task.get": (p: Record<string, unknown>) => h.env.cockpit.getState().tasks[p.task as string] });
  render(<App env={h.env} />);
  act(() => h.load([task("a", { repo_path: "/repos/shop" }), task("z", { repo_path: "/repos/other" })]));
  return h;
}

describe("App", () => {
  it("notifies when a task off screen starts needing you, not when it is on screen and focused", () => {
    const h = setup();
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    // Home shows the first project (other): z is on screen, a is not.
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(1, "z", "status", { to: "needs_you" }) }));
    expect(h.env.notify).not.toHaveBeenCalled();
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(2, "a", "status", { to: "needs_you" }) }));
    expect(h.env.notify).toHaveBeenCalledWith(expect.objectContaining({ id: "a" }));
  });

  it("the first snapshot never notifies", () => {
    const h = makeEnv();
    render(<App env={h.env} />);
    act(() => h.load([task("n", { status: "needs_you" })]));
    expect(h.env.notify).not.toHaveBeenCalled();
  });

  it("notifies for the visible task when the window is not focused", () => {
    const h = setup();
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(1, "z", "status", { to: "needs_you" }) }));
    expect(h.env.notify).toHaveBeenCalledTimes(1);
  });

  it("⌘G toggles the grid, ⌘K opens the command bar, Escape leaves the detail", async () => {
    setup();
    const mac = /Mac/.test(navigator.platform);
    const mod = mac ? { metaKey: true } : { ctrlKey: true };
    fireEvent.keyDown(window, { key: "g", ...mod });
    expect(screen.getByRole("button", { name: /Grid/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "g", ...mod });
    expect(screen.getByRole("button", { name: /Home/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "k", ...mod });
    const input = screen.getByRole("combobox");
    await userEvent.type(input, "goal of z{Enter}");
    expect(screen.getByRole("button", { name: "Detail" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.getByRole("button", { name: /Home/ })).toHaveAttribute("aria-pressed", "true");
  });

  it("visibleTasks follows the mode", () => {
    const h = makeEnv();
    h.load([task("a"), task("b", { repo_path: "/x" })]);
    const s = h.env.cockpit.getState();
    expect([...visibleTasks(s, "home", "/x", null)]).toEqual(["b"]);
    expect([...visibleTasks(s, "detail", null, "a")]).toEqual(["a"]);
  });
});
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/lib/notify.test.ts src/App.test.tsx`

Expected: FAIL — Failed to resolve import "./App" (and "./notify")

- [ ] **Step 4: Create `apps/cockpit/src/lib/notify.ts`**

```ts
// Native notification when a task starts needing the user, unless that task is already on
// screen in the focused window (the decision is the client's: only it knows what is shown).
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { TaskView } from "./protocol";

export function shouldNotify(taskId: string, visible: ReadonlySet<string>, focused: boolean): boolean {
  return !(focused && visible.has(taskId));
}

export function notificationFor(t: TaskView): { title: string; body: string } {
  const why = t.boot === "trust_prompt" ? "asks to trust its folder" : "needs you";
  return { title: `${t.branch} ${why}`, body: t.title };
}

let granted: boolean | null = null;

export async function notify(t: TaskView): Promise<void> {
  if (granted === null) {
    granted = await isPermissionGranted();
    if (!granted) granted = (await requestPermission()) === "granted";
  }
  if (granted) sendNotification(notificationFor(t));
}
```

- [ ] **Step 5: Create `apps/cockpit/src/App.tsx`**

```tsx
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { CommandBar, type Command } from "./components/CommandBar";
import { Detail } from "./components/Detail";
import { Grid } from "./components/Grid";
import { Home } from "./components/Home";
import { type Env, EnvContext } from "./env";
import { shouldNotify } from "./lib/notify";
import { type DaemonStatus, M } from "./lib/protocol";
import { loadRepos } from "./lib/repos";
import { SHORTCUT_HINTS, shortcutFor } from "./lib/shortcuts";
import { applyTheme, loadTheme, nextTheme, THEME_KEY, type Theme } from "./lib/theme";
import { useCockpitState } from "./store/cockpit";
import { gridTasks, newlyNeedingYou, projects } from "./store/select";
import type { State } from "./store/state";

export type Mode = "home" | "detail" | "grid";

const MAC = typeof navigator !== "undefined" && /Mac/.test(navigator.platform);
export const STATUS_POLL_MS = 5000;

/** The task ids whose card, tile or detail is on screen in this mode. */
export function visibleTasks(s: State, mode: Mode, project: string | null, taskId: string | null): Set<string> {
  if (mode === "detail") return new Set(taskId ? [taskId] : []);
  if (mode === "grid") return new Set(gridTasks(s).map((t) => t.id));
  return new Set(Object.values(s.tasks).filter((t) => t.repo_path === project).map((t) => t.id));
}

function ConnectionBanner({ s, env }: { s: State; env: Env }) {
  const [error, setError] = useState<string | null>(null);
  if (s.connection.state === "live") return null;
  return (
    <div className={`banner banner-${s.connection.state}`} role="status">
      {s.connection.state === "connecting" ? (
        "Connecting to the Mirante daemon…"
      ) : (
        <>
          Daemon not reachable ({s.connection.error}). Retrying in {Math.round(s.connection.retryMs / 100) / 10}s.{" "}
          <button className="btn btn-small" onClick={() => env.startDaemon().catch((e) => setError(String(e)))}>
            Start daemon
          </button>
          {error && <span className="error"> {error}</span>}
        </>
      )}
    </div>
  );
}

export function App({ env }: { env: Env }) {
  const s = useCockpitState(env.cockpit);
  const [mode, setMode] = useState<Mode>("home");
  const [beforeGrid, setBeforeGrid] = useState<Mode>("home");
  const [taskId, setTaskId] = useState<string | null>(null);
  const [repos, setRepos] = useState(() => loadRepos(env.storage));
  const [project, setProject] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [palette, setPalette] = useState(false);
  const [theme, setTheme] = useState<Theme>(() => loadTheme(env.storage));
  const [status, setStatus] = useState<DaemonStatus | null>(null);

  const projs = useMemo(() => projects(s, repos), [s, repos]);
  const current = project ?? projs[0]?.path ?? null;

  const visible = useRef<Set<string>>(new Set());
  visible.current = visibleTasks(s, mode, current, taskId);

  useEffect(() => {
    env.cockpit.connect().catch(() => {});
  }, [env]);

  useEffect(
    () =>
      env.cockpit.onChange((prev, next) => {
        // The first snapshot is what the user left, not news.
        if (prev.bootId === null) return;
        for (const t of newlyNeedingYou(prev, next)) {
          if (shouldNotify(t.id, visible.current, document.hasFocus())) env.notify(t);
        }
      }),
    [env],
  );

  const live = s.connection.state === "live";
  useEffect(() => {
    if (!live) return;
    const poll = () => env.rpc<DaemonStatus>(M.daemonStatus).then(setStatus, () => setStatus(null));
    poll();
    const h = setInterval(poll, STATUS_POLL_MS);
    return () => clearInterval(h);
  }, [env, live]);

  useEffect(() => {
    applyTheme(theme);
    env.storage.setItem(THEME_KEY, theme);
  }, [env, theme]);

  const open = useCallback((id: string) => {
    setTaskId(id);
    setMode("detail");
  }, []);
  const toggleGrid = useCallback(() => {
    setMode((m) => {
      if (m === "grid") return beforeGrid;
      setBeforeGrid(m);
      return "grid";
    });
  }, [beforeGrid]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const a = shortcutFor(e, MAC);
      if (!a) return;
      if (a === "escape") {
        if (!palette && mode === "detail") setMode("home");
        return;
      }
      e.preventDefault();
      if (a === "palette") setPalette((p) => !p);
      if (a === "grid") toggleGrid();
      if (a === "home") setMode("home");
      if (a === "theme") setTheme(nextTheme);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette, mode, toggleGrid]);

  const commands: Command[] = useMemo(
    () => [
      { id: "home", label: "Go to Home", hint: SHORTCUT_HINTS.home, run: () => setMode("home") },
      { id: "grid", label: "Toggle agent grid", hint: SHORTCUT_HINTS.grid, run: toggleGrid },
      { id: "theme", label: `Theme: ${nextTheme(theme)}`, hint: SHORTCUT_HINTS.theme, run: () => setTheme(nextTheme) },
      ...projs.map((p) => ({ id: `p-${p.path}`, label: `Project ${p.name}`, run: () => (setProject(p.path), setMode("home")) })),
      ...Object.values(s.tasks)
        .filter((t) => t.lifecycle !== "archived")
        .map((t) => ({ id: `t-${t.id}`, label: `Open ${t.title} (@${t.agent} ${t.branch})`, run: () => open(t.id) })),
    ],
    [projs, s.tasks, theme, open, toggleGrid],
  );

  return (
    <EnvContext.Provider value={env}>
      <div className="app">
        <header className="topbar">
          <span className="brand">Mirante</span>
          <nav aria-label="Views" className="modes">
            <button aria-pressed={mode === "home"} onClick={() => setMode("home")}>
              Home <kbd>{SHORTCUT_HINTS.home}</kbd>
            </button>
            <button aria-pressed={mode === "detail"} disabled={!taskId} onClick={() => setMode("detail")}>
              Detail
            </button>
            <button aria-pressed={mode === "grid"} onClick={toggleGrid}>
              Grid <kbd>{SHORTCUT_HINTS.grid}</kbd>
            </button>
          </nav>
          <button className="btn btn-small" onClick={() => setPalette(true)}>
            Commands <kbd>{SHORTCUT_HINTS.palette}</kbd>
          </button>
          <button className="btn btn-small" onClick={() => setTheme(nextTheme)} aria-label={`Theme: ${theme}`}>
            {theme === "system" ? "◐" : theme === "light" ? "☀" : "☾"}
          </button>
        </header>
        <ConnectionBanner s={s} env={env} />
        {mode === "home" && (
          <Home
            projects={projs}
            project={current}
            onProject={setProject}
            onReposChanged={setRepos}
            onOpen={open}
            status={status}
            draft={draft}
            onDraft={setDraft}
          />
        )}
        {mode === "detail" && taskId && <Detail taskId={taskId} onSelect={setTaskId} />}
        {mode === "grid" && <Grid onOpen={open} />}
        {palette && (
          <CommandBar
            commands={commands}
            onClose={() => setPalette(false)}
            onDispatch={(text) => {
              setDraft(text);
              setMode("home");
            }}
          />
        )}
      </div>
    </EnvContext.Provider>
  );
}
```

- [ ] **Step 6: Create `apps/cockpit/src/styles.css`**

```css
/* Mirante cockpit. Tokens first; light is the base, dark overrides (OS or data-theme). */
:root {
  --bg: #f6f7f9;
  --panel: #ffffff;
  --panel-2: #eef1f4;
  --text: #15191f;
  --muted: #5b6573;
  --line: #d9dee5;
  --accent: #0f766e;
  --accent-text: #ffffff;
  --needs: #b45309;
  --needs-bg: #fff4e5;
  --work: #1d4ed8;
  --work-bg: #e8efff;
  --review: #15803d;
  --review-bg: #e7f6ec;
  --danger: #b91c1c;
  --add: #15803d;
  --del: #b91c1c;
  --focus: #2563eb;
  --mono: ui-monospace, "SF Mono", Menlo, monospace;
  --sans: -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, sans-serif;
  color-scheme: light;
}

@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --bg: #0f1216;
    --panel: #161a20;
    --panel-2: #1d232b;
    --text: #e6e9ee;
    --muted: #97a1ae;
    --line: #2a313b;
    --accent: #2dd4bf;
    --accent-text: #062a26;
    --needs: #fbbf24;
    --needs-bg: #2d2412;
    --work: #93b4ff;
    --work-bg: #172238;
    --review: #4ade80;
    --review-bg: #12291b;
    --danger: #f87171;
    --add: #4ade80;
    --del: #f87171;
    --focus: #93b4ff;
    color-scheme: dark;
  }
}

:root[data-theme="dark"] {
  --bg: #0f1216;
  --panel: #161a20;
  --panel-2: #1d232b;
  --text: #e6e9ee;
  --muted: #97a1ae;
  --line: #2a313b;
  --accent: #2dd4bf;
  --accent-text: #062a26;
  --needs: #fbbf24;
  --needs-bg: #2d2412;
  --work: #93b4ff;
  --work-bg: #172238;
  --review: #4ade80;
  --review-bg: #12291b;
  --danger: #f87171;
  --add: #4ade80;
  --del: #f87171;
  --focus: #93b4ff;
  color-scheme: dark;
}

* { box-sizing: border-box; }
html, body, #root { height: 100%; margin: 0; }
body { background: var(--bg); color: var(--text); font: 14px/1.45 var(--sans); }
button, input, textarea { font: inherit; color: inherit; }
code, kbd, pre { font-family: var(--mono); font-size: 12.5px; }
kbd { border: 1px solid var(--line); border-radius: 4px; padding: 0 4px; color: var(--muted); }
:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
.sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); white-space: nowrap; }
.muted { color: var(--muted); }
.small { font-size: 12px; }
.error { color: var(--danger); margin: 4px 0 0; }
.add { color: var(--add); }
.del { color: var(--del); }

.app { display: flex; flex-direction: column; height: 100%; }
.topbar { display: flex; align-items: center; gap: 12px; padding: 8px 16px; border-bottom: 1px solid var(--line); background: var(--panel); }
.brand { font-weight: 700; letter-spacing: 0.02em; }
.modes { display: flex; gap: 4px; margin-right: auto; }
.modes button { background: none; border: 1px solid transparent; border-radius: 6px; padding: 4px 10px; cursor: pointer; }
.modes button[aria-pressed="true"] { border-color: var(--line); background: var(--panel-2); }
.banner { padding: 8px 16px; border-bottom: 1px solid var(--line); }
.banner-down { background: var(--needs-bg); color: var(--needs); }

.btn { border: 1px solid var(--line); background: var(--panel); border-radius: 6px; padding: 6px 12px; cursor: pointer; }
.btn:disabled { opacity: 0.5; cursor: not-allowed; }
.btn-primary { background: var(--accent); border-color: var(--accent); color: var(--accent-text); }
.btn-warn { border-color: var(--needs); color: var(--needs); }
.btn-small { padding: 2px 8px; font-size: 12px; }
.link { background: none; border: none; padding: 0; color: var(--text); font-weight: 600; cursor: pointer; text-align: left; }
.link:hover { text-decoration: underline; }

.home { display: grid; grid-template-columns: 240px 1fr 280px; flex: 1; min-height: 0; }
.rail, .board, .tasklist { background: var(--panel); border-right: 1px solid var(--line); padding: 12px; overflow: auto; }
.board { border-right: none; border-left: 1px solid var(--line); }
.rail h2, .board h2, .tasklist h2, .memory h2 { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); margin: 16px 0 6px; }
.rail ul, .board ul, .tasklist ul { list-style: none; margin: 0; padding: 0; }
.rail-item { display: flex; width: 100%; justify-content: space-between; align-items: center; gap: 6px; background: none; border: none; border-radius: 6px; padding: 6px 8px; cursor: pointer; text-align: left; flex-wrap: wrap; }
.rail-item:hover, .rail-item.selected { background: var(--panel-2); }
.badge { background: var(--needs); color: var(--panel); border-radius: 999px; padding: 0 7px; font-size: 11px; font-weight: 700; }
.add-repo { display: flex; gap: 4px; flex-wrap: wrap; margin-top: 8px; }
.add-repo input { flex: 1; min-width: 0; border: 1px solid var(--line); border-radius: 6px; padding: 4px 6px; background: var(--bg); }

.feed { display: flex; flex-direction: column; min-height: 0; }
.feed-head { padding: 12px 20px; border-bottom: 1px solid var(--line); display: flex; align-items: baseline; gap: 12px; }
.feed-head h1 { font-size: 16px; margin: 0; }
.cards { flex: 1; overflow: auto; padding: 16px 20px; display: flex; flex-direction: column; gap: 14px; }
.empty { margin: auto; }
.card, .tile { background: var(--panel); border: 1px solid var(--line); border-left: 4px solid var(--line); border-radius: 8px; padding: 10px 12px; }
.card-needs_you { border-left-color: var(--needs); }
.card-working { border-left-color: var(--work); }
.card-review { border-left-color: var(--review); }
.dispatch { margin: 0 0 6px; color: var(--muted); font-size: 12.5px; }
.who { font-weight: 600; margin-right: 6px; color: var(--muted); }
.card-head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.badges { display: inline-flex; gap: 6px; align-items: center; flex-wrap: wrap; }
.agent { font-weight: 600; }
.agent-claude { color: #c2410c; }
.agent-codex { color: var(--work); }
.branch { background: var(--panel-2); border-radius: 4px; padding: 0 4px; }
.tag { border: 1px solid var(--line); border-radius: 999px; padding: 0 7px; font-size: 11px; }
.tag-muted { color: var(--muted); }
.tag-warn { border-color: var(--needs); color: var(--needs); }
.pill { margin-left: auto; border-radius: 999px; padding: 1px 8px; font-size: 11px; font-weight: 600; }
.pill-needs_you { background: var(--needs-bg); color: var(--needs); }
.pill-working { background: var(--work-bg); color: var(--work); }
.pill-review { background: var(--review-bg); color: var(--review); }
.pill-idle, .pill-archived { background: var(--panel-2); color: var(--muted); }
.lines { list-style: none; margin: 8px 0 0; padding: 0; font-family: var(--mono); font-size: 12px; }
.line { padding: 1px 0; white-space: pre-wrap; word-break: break-word; }
.line-system, .line-turn { color: var(--muted); }
.line-failed .text { color: var(--danger); }
.line-message .text { font-family: var(--sans); }
.lines-full { max-height: none; }
.perm { margin-top: 10px; border: 1px solid var(--needs); background: var(--needs-bg); border-radius: 8px; padding: 8px 10px; }
.perm-what { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
.perm-detail { white-space: pre-wrap; word-break: break-all; }
.perm-actions { display: flex; gap: 8px; margin-top: 8px; }
.perm-compact { padding: 6px 8px; }
.done { margin-top: 10px; display: flex; justify-content: space-between; align-items: center; gap: 8px; flex-wrap: wrap; background: var(--review-bg); border-radius: 8px; padding: 8px 10px; }
.done-actions { display: flex; gap: 6px; }

.composer { border-top: 1px solid var(--line); padding: 10px 20px; display: flex; gap: 8px; align-items: flex-start; flex-wrap: wrap; background: var(--panel); }
.composer textarea { flex: 1; min-width: 200px; resize: vertical; border: 1px solid var(--line); border-radius: 6px; padding: 6px 8px; background: var(--bg); }
.composer .error, .composer [role="status"] { flex-basis: 100%; }
.composer-actions { display: flex; flex-direction: column; gap: 6px; }
.composer-off { padding: 10px 20px; }

.board section { margin-bottom: 12px; }
.count { font-weight: 400; }
.chip { display: flex; flex-direction: column; gap: 2px; width: 100%; text-align: left; background: var(--panel-2); border: 1px solid transparent; border-left: 3px solid var(--line); border-radius: 6px; padding: 6px 8px; margin-bottom: 6px; cursor: pointer; }
.chip-needs_you { border-left-color: var(--needs); }
.chip-working { border-left-color: var(--work); }
.chip-review { border-left-color: var(--review); }
.chip-title { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }

.memory { margin-top: 16px; border-top: 1px solid var(--line); }
.mem-list, .mem-notices { list-style: none; padding: 0; margin: 6px 0; }
.mem-list li { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; padding: 2px 0; }
.mem-soft .mem-name { color: var(--needs); }
.mem-hard .mem-name { color: var(--danger); }
.level-elevated strong { color: var(--needs); }
.level-critical strong { color: var(--danger); }

.detail { display: grid; grid-template-columns: 260px 1fr; flex: 1; min-height: 0; }
.detail-main { display: flex; flex-direction: column; min-height: 0; padding: 12px 20px 0; overflow: hidden; }
.detail-head { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.detail-head h1 { font-size: 16px; margin: 0; }
.actions { display: inline-flex; gap: 6px; align-items: center; }
.tabs { display: flex; gap: 2px; margin-top: 12px; border-bottom: 1px solid var(--line); }
.tabs [role="tab"] { background: none; border: none; border-bottom: 2px solid transparent; padding: 6px 12px; cursor: pointer; }
.tabs [role="tab"][aria-selected="true"] { border-bottom-color: var(--accent); font-weight: 600; }
.panel { flex: 1; overflow: auto; padding: 12px 0; min-height: 0; }
.terminal-wrap { height: 100%; display: flex; flex-direction: column; }
.terminal { flex: 1; min-height: 320px; background: #0b0d10; border-radius: 6px; padding: 6px; }
.diff-files { list-style: none; padding: 0; }
.patch { background: var(--panel-2); border-radius: 6px; padding: 8px; overflow: auto; }
.d-add { color: var(--add); }
.d-del { color: var(--del); }
.d-hunk { color: var(--work); }
.d-meta { color: var(--muted); }

.grid { flex: 1; display: grid; gap: 12px; padding: 12px; min-height: 0; overflow: auto; grid-template-columns: repeat(2, 1fr); }
.grid-3, .grid-5, .grid-6 { grid-template-columns: repeat(3, 1fr); }
.grid-empty { margin: auto; }
.tile { display: flex; flex-direction: column; gap: 6px; min-height: 200px; }
.tile .lines { flex: 1; }
.mini { border: 1px solid var(--line); border-radius: 6px; padding: 4px 8px; background: var(--bg); }
.tile-foot { display: flex; justify-content: space-between; align-items: center; }

.overlay { position: fixed; inset: 0; background: rgb(0 0 0 / 0.35); display: flex; justify-content: center; align-items: flex-start; padding-top: 12vh; }
.palette { width: min(640px, 90vw); background: var(--panel); border: 1px solid var(--line); border-radius: 10px; box-shadow: 0 20px 50px rgb(0 0 0 / 0.3); overflow: hidden; }
.palette input { width: 100%; border: none; border-bottom: 1px solid var(--line); padding: 12px 14px; background: var(--panel); outline: none; }
.palette ul { list-style: none; margin: 0; padding: 6px; max-height: 50vh; overflow: auto; }
.palette li { display: flex; justify-content: space-between; padding: 6px 10px; border-radius: 6px; cursor: pointer; }
.palette li.active { background: var(--panel-2); }

@media (prefers-reduced-motion: no-preference) {
  .card, .chip, .tile { transition: border-color 150ms ease; }
}
```

- [ ] **Step 7: Create `apps/cockpit/src/main.tsx`**

```tsx
import "@xterm/xterm/css/xterm.css";
import "./styles.css";
import { openUrl } from "@tauri-apps/plugin-opener";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import type { Env } from "./env";
import { daemonStart, rpc, subscribe } from "./lib/daemon";
import { notify } from "./lib/notify";
import { createCockpit } from "./store/cockpit";

const env: Env = {
  cockpit: createCockpit({ rpc, subscribe }),
  rpc,
  storage: window.localStorage,
  openUrl: (url) => openUrl(url),
  startDaemon: daemonStart,
  notify: (t) => void notify(t).catch(() => {}),
};

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App env={env} />
  </StrictMode>,
);
```

- [ ] **Step 8: Run and confirm**

Run: `pnpm --dir apps/cockpit exec vitest run src/lib/notify.test.ts src/App.test.tsx && pnpm --dir apps/cockpit build`

Expected: exit 0; output contains `Tests  7 passed (7)`

- [ ] **Step 9: Commit**

```bash
git add apps/cockpit/src/lib/notify.ts apps/cockpit/src/lib/notify.test.ts apps/cockpit/src/App.tsx apps/cockpit/src/App.test.tsx apps/cockpit/src/styles.css apps/cockpit/src/main.tsx
git commit -F- <<'EOF'
feat(cockpit): app shell with three modes, connection banner and focused-window notification suppression

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 19: Memory guard UI

Left-rail panel: system pressure (`daemon.status.memory`: level, free, paused), the five heaviest sessions (`TaskView.memory` vs budget, soft/hard coloured) and the last three `memory.*` notices (warn, offer to hibernate, refused, leak…). **Hibernate** / **Wake** call `session.hibernate` / `session.wake`; a daemon without them (METHOD_NOT_FOUND) is reported, not crashed on. The same button goes into the detail header and the grid tiles.

**Files:**
- Create: `apps/cockpit/src/components/MemoryPanel.tsx`
- Create: `apps/cockpit/src/components/MemoryPanel.test.tsx`
- Modify: `apps/cockpit/src/components/LeftRail.tsx`
- Modify: `apps/cockpit/src/components/Detail.tsx`
- Modify: `apps/cockpit/src/components/Grid.tsx`

- [ ] **Step 1: Create `apps/cockpit/src/components/MemoryPanel.test.tsx`**

```tsx
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { RpcCode } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { MemoryPanel } from "./MemoryPanel";

const status = { pid: 1, boot_id: "b1", version: "x", live_sessions: 1 };

describe("MemoryPanel", () => {
  it("says when the daemon has no memory guard", () => {
    const h = makeEnv();
    h.load([task("a")]);
    renderWith(h.env, <MemoryPanel status={status} />);
    expect(screen.getByText(/Memory guard not available/)).toBeInTheDocument();
  });

  it("shows pressure, per-session memory, notices, and hibernates on request", async () => {
    const h = makeEnv({ "session.hibernate": {} });
    const memory = { bytes: 3 * 2 ** 30, peak: 3 * 2 ** 30, budget: 4 * 2 ** 30, state: "soft" };
    h.load([task("a", { memory })], [ev(1, "a", "memory.offer_hibernate", { reason: "memory pressure elevated" })]);
    const sys = { level: "elevated", total: 16 * 2 ** 30, free: 512 * 2 ** 20, swap_used: 0, swap_total: 0, paused: true };
    renderWith(h.env, <MemoryPanel status={{ ...status, memory: sys }} />);
    expect(screen.getByText("elevated")).toBeInTheDocument();
    expect(screen.getByText(/512 MB free/)).toBeInTheDocument();
    expect(screen.getByText(/new sessions paused/)).toBeInTheDocument();
    expect(screen.getByText("3072 MB / 4096 MB")).toBeInTheDocument();
    expect(screen.getByText("feat/a: hibernate suggested: memory pressure elevated")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    expect(h.calls).toContainEqual(["session.hibernate", { task: "a" }]);
  });

  it("a daemon without hibernate answers METHOD_NOT_FOUND and the UI says so", async () => {
    const h = makeEnv({ "session.hibernate": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method") });
    h.load([task("a", { memory: { bytes: 1, peak: 1, budget: 2, state: "ok" } })]);
    renderWith(h.env, <MemoryPanel status={status} />);
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("memory guard not available in this daemon");
  });
});
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `pnpm --dir apps/cockpit exec vitest run src/components/MemoryPanel.test.tsx`

Expected: FAIL — Failed to resolve import "./MemoryPanel"

- [ ] **Step 3: Create `apps/cockpit/src/components/MemoryPanel.tsx`**

```tsx
import { useMemo, useState } from "react";
import { useEnv, useStore } from "../env";
import { DaemonError } from "../lib/daemon";
import { HIBERNATE, isHibernated, mb, systemMemory, taskMemory, WAKE } from "../lib/memguard";
import type { DaemonStatus, TaskView } from "../lib/protocol";

/** Hibernate (end the process, keep the native session) or wake (resume it). */
export function HibernateButton({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const asleep = isHibernated(t);
  if (!t.live && !asleep) return null;
  const run = () =>
    rpc(asleep ? WAKE : HIBERNATE, { task: t.id }).catch((e) =>
      setError(e instanceof DaemonError && e.unsupported ? "memory guard not available in this daemon" : String(e?.message ?? e)),
    );
  return (
    <>
      <button className="btn btn-small" onClick={run} title={asleep ? "Resume the agent's session" : "Stop the process, keep the session to resume later"}>
        {asleep ? "Wake" : "Hibernate"}
      </button>
      {error && (
        <span className="error" role="alert">
          {error}
        </span>
      )}
    </>
  );
}

export function MemoryPanel({ status }: { status: DaemonStatus | null }) {
  const s = useStore();
  const sys = systemMemory(status);
  const heavy = useMemo(
    () =>
      Object.values(s.tasks)
        .filter((t) => taskMemory(t) || isHibernated(t))
        .sort((a, b) => (taskMemory(b)?.bytes ?? 0) - (taskMemory(a)?.bytes ?? 0))
        .slice(0, 5),
    [s.tasks],
  );
  const notices = s.memory.slice(-3).reverse();
  return (
    <section className="memory" aria-label="Memory guard">
      <h2>Memory</h2>
      {!sys ? (
        <p className="muted small">Memory guard not available in this daemon.</p>
      ) : (
        <p className={`small level-${sys.level}`}>
          System pressure <strong>{sys.level}</strong>
          {" "}· {mb(sys.free)} free
          {sys.paused && <> · new sessions paused</>}
        </p>
      )}
      <ul className="mem-list">
        {heavy.map((t) => {
          const m = taskMemory(t);
          return (
            <li key={t.id} className={m ? `mem-${m.state}` : ""}>
              <span className="mem-name">{t.branch}</span>
              <span className="muted small">{m ? `${mb(m.bytes)} / ${mb(m.budget)}` : "hibernated"}</span>
              <HibernateButton t={t} />
            </li>
          );
        })}
      </ul>
      {notices.length > 0 && (
        <ul className="mem-notices" aria-label="Memory notices">
          {notices.map((n) => (
            <li key={n.seq} className="small">
              {n.task_id && s.tasks[n.task_id] ? `${s.tasks[n.task_id].branch}: ` : ""}
              {n.text}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
```

- [ ] **Step 4: Import — `apps/cockpit/src/components/LeftRail.tsx`**

Replace:

```tsx
import { addRepo } from "../lib/repos";
import type { Project } from "../store/select";
```

with:

```tsx
import { addRepo } from "../lib/repos";
import type { Project } from "../store/select";
import { MemoryPanel } from "./MemoryPanel";
```

- [ ] **Step 5: Render the panel at the end of the rail — `apps/cockpit/src/components/LeftRail.tsx`**

Replace:

```tsx
        ))}
      </ul>
    </nav>
```

with:

```tsx
        ))}
      </ul>
      <MemoryPanel status={props.status} />
    </nav>
```

- [ ] **Step 6: Import — `apps/cockpit/src/components/Detail.tsx`**

Replace:

```tsx
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { DiffTab } from "./DiffTab";
```

with:

```tsx
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { DiffTab } from "./DiffTab";
import { HibernateButton } from "./MemoryPanel";
```

- [ ] **Step 7: In `Actions`, after the Resume button — `apps/cockpit/src/components/Detail.tsx`**

Replace:

```tsx
        </button>
      )}
      {error && (
```

with:

```tsx
        </button>
      )}
      <HibernateButton t={t} />
      {error && (
```

- [ ] **Step 8: Import — `apps/cockpit/src/components/Grid.tsx`**

Replace:

```tsx
import { StatusPill, TaskBadges } from "./Badges";
```

with:

```tsx
import { StatusPill, TaskBadges } from "./Badges";
import { HibernateButton } from "./MemoryPanel";
```

- [ ] **Step 9: In the tile footer — `apps/cockpit/src/components/Grid.tsx`**

Replace:

```tsx
        {note && <span className="small muted">{note}</span>}
```

with:

```tsx
        {note && <span className="small muted">{note}</span>}
        <HibernateButton t={t} />
```

- [ ] **Step 10: Run and confirm**

Run: `pnpm --dir apps/cockpit test && pnpm --dir apps/cockpit typecheck`

Expected: exit 0; output contains `Tests  78 passed (78)`

- [ ] **Step 11: Commit**

```bash
git add apps/cockpit/src/components/MemoryPanel.tsx apps/cockpit/src/components/MemoryPanel.test.tsx apps/cockpit/src/components/LeftRail.tsx apps/cockpit/src/components/Detail.tsx apps/cockpit/src/components/Grid.tsx
git commit -F- <<'EOF'
feat(cockpit): memory guard panel with hibernate / wake

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


## Phase 3-v — Packaging and dogfood

### Task 20: Packaging, smoke test and dev instructions

A WebDriver-free smoke test: build the debug `.app`, start the fake daemon on a temporary `MIRANTE_HOME`, launch the app binary against it, and require the calls the frontend only makes after the snapshot crossed the Tauri channel (`permission.list`, `task.events` per task, `daemon.status`). Then the unsigned release `.app` for local use, and the README with dev instructions.

**Files:**
- Create: `apps/cockpit/scripts/smoke.sh`, `apps/cockpit/README.md`

- [ ] **Step 1: Create `apps/cockpit/scripts/smoke.sh`**

```bash
#!/usr/bin/env bash
# Smoke test of the built app against the scripted fake daemon: the app must start, its
# frontend must subscribe through the bridge, receive the snapshot over the Tauri channel
# (proved by the follow-up calls the store makes only after a snapshot arrives) and poll
# daemon.status. Opens the app window for a few seconds. macOS; needs a prior
# `pnpm tauri build --debug --bundles app` and `cargo build -p mirante-bridge --features fake`.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$here/../.." && pwd)"
app="$here/src-tauri/target/debug/bundle/macos/Mirante.app/Contents/MacOS/mirante-cockpit"
fake="${CARGO_TARGET_DIR:-$root/target}/debug/mirante-fake-daemon"
[ -x "$app" ] || { echo "missing $app: run pnpm tauri build --debug --bundles app"; exit 1; }
[ -x "$fake" ] || { echo "missing $fake: run cargo build -p mirante-bridge --features fake"; exit 1; }
home="$(mktemp -d /tmp/mr-smoke-XXXX)"
log="$home/fake.log"
"$fake" "$home/daemon.sock" >"$log" 2>&1 &
fake_pid=$!
MIRANTE_HOME="$home" "$app" >"$home/app.log" 2>&1 &
app_pid=$!
cleanup() {
  kill "$app_pid" "$fake_pid" 2>/dev/null || true
  wait "$app_pid" "$fake_pid" 2>/dev/null || true
  rm -rf "$home"
}
trap cleanup EXIT
ok=0
for _ in $(seq 1 60); do
  if grep -q "call events.subscribe" "$log" && grep -q "call permission.list" "$log" \
    && grep -q "call task.events" "$log" && grep -q "call daemon.status" "$log"; then
    ok=1
    break
  fi
  sleep 0.5
done
sort "$log" | uniq -c
if [ "$ok" = 1 ]; then echo "SMOKE PASS"; else echo "SMOKE FAIL"; cat "$home/app.log"; exit 1; fi
```

- [ ] **Step 2: Run**

```bash
chmod +x apps/cockpit/scripts/smoke.sh
```

- [ ] **Step 3: Create `apps/cockpit/README.md`**

````markdown
# Mirante cockpit (desktop app)

Tauri 2 + React. A thin client of `mirante-daemon`: every call goes over the daemon's Unix
socket through `crates/mirante-bridge`; the app has no network access, no login and no
telemetry.

## Run it

Prerequisites: Rust (stable), Node 22.12+ and pnpm 10, Xcode command line tools (macOS).

```bash
# The daemon, the hook and the CLI, once (the app finds the daemon in ~/.cargo/bin,
# /opt/homebrew/bin or /usr/local/bin, or at $MIRANTE_DAEMON_BIN):
cargo install --path crates/mirante-daemon
cargo install --path crates/mirante-hook
cargo install --path crates/mirante-cli

pnpm --dir apps/cockpit install
pnpm --dir apps/cockpit tauri dev
```

If no daemon is running, the banner offers **Start daemon**.

### Without agents: the fake daemon

The scripted fake daemon serves three demo tasks (working, needs-you with a permission card,
ready for review) and adds a step every two seconds:

```bash
mkdir -p /tmp/mr-ui
cargo run -p mirante-bridge --features fake --bin mirante-fake-daemon -- /tmp/mr-ui/daemon.sock &
MIRANTE_HOME=/tmp/mr-ui pnpm --dir apps/cockpit tauri dev
```

(`MIRANTE_HOME` must be a short path: Unix socket paths are limited to about 104 bytes.)

## Test

```bash
pnpm --dir apps/cockpit test            # Vitest: store, parser, components
pnpm --dir apps/cockpit typecheck
cargo test -p mirante-bridge            # the bridge against a fake daemon
```

Smoke test of the built app against the fake daemon (opens the window for a few seconds):

```bash
cargo build -p mirante-bridge --features fake
pnpm --dir apps/cockpit tauri build --debug --bundles app
apps/cockpit/scripts/smoke.sh
```

## Build for local use (macOS, unsigned)

```bash
pnpm --dir apps/cockpit tauri build --bundles app
open apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app
```

The bundle is not signed or notarized. Copied to another Mac it is quarantined; clear that
with `xattr -dr com.apple.quarantine Mirante.app` (only for a build you made yourself).

## Keys

| Keys | What |
|---|---|
| ⌘K | Command bar (tasks, projects, `@claude repo/branch goal` to dispatch) |
| ⌘G | Toggle the agent grid |
| ⌘1 | Home |
| Esc | Leave a task's detail |
| ⇧⌘J | Theme: system → light → dark |
| ⌘↵ / ⌥↵ | In a task's composer: steer now / queue |
````

- [ ] **Step 4: Run and confirm**

Run: `cargo build -p mirante-bridge --features fake && pnpm --dir apps/cockpit tauri build --debug --bundles app && apps/cockpit/scripts/smoke.sh`

Expected: exit 0; output contains `SMOKE PASS`

- [ ] **Step 5: Run and confirm**

Run: `pnpm --dir apps/cockpit tauri build --bundles app`

Expected: exit 0; output contains `Finished 1 bundle at:` (.../apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app)

- [ ] **Step 6: Commit**

```bash
git add apps/cockpit/scripts/smoke.sh apps/cockpit/README.md
git commit -F- <<'EOF'
build(cockpit): app smoke test against the fake daemon; dev and packaging instructions

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


### Task 21: Dogfood checklist and full verification

The manual checklist for the founder and the first external users (real daemon, real agents), then every automated check once more.

**Files:**
- Create: `docs/dogfood/cockpit.md`

- [ ] **Step 1: Create `docs/dogfood/cockpit.md`**

```markdown
# Cockpit dogfood checklist

Run against the real daemon and real agents, on a throwaway repo first, then on a real one.
Tick each line on the day you try it; write what went wrong next to it. The spec's success
metric is two consecutive weeks of using this instead of cmux / plain terminals.

Setup: `cargo install --path crates/mirante-daemon`, `crates/mirante-hook`,
`crates/mirante-cli`; build and open the app (`apps/cockpit/README.md`). Keep
`mirante watch` open in a terminal the first day to compare.

## A. Start and connect

- [ ] With no daemon running, the banner says it is not reachable; **Start daemon** starts it
      and the banner disappears within 2 s.
- [ ] `mirante daemon restart` while the app is open: the banner shows, then the app
      reconnects by itself; tasks and feeds are intact (no duplicate lines).
- [ ] Quit and reopen the app: every task, its feed (last 200 events) and pending
      permission cards are back.

## B. Dispatch (V3 home)

- [ ] Add a repo by absolute path; it appears under Projects.
- [ ] `@claude <repo>/<new-branch> <goal>` creates the worktree and starts Claude; the card
      appears at once and moves to Working.
- [ ] A new folder: the trust card appears; **Trust folder** continues, **Decline** ends the
      session (try both, on two tasks).
- [ ] `@codex <repo>/<branch> <goal>`: the card shows the **auto-edits** label.
- [ ] A typo (`@claud`, missing branch, unknown repo) shows the reason and calls nothing.

## C. Permissions

- [ ] Claude asks for a command: the card shows tool and command; **Allow once** runs it,
      the card leaves; **Deny** stops it, the status leaves Needs you within 2 s.
- [ ] Codex asks for write access outside the sandbox: the card says
      **session-wide grant**.
- [ ] Answer a request from the CLI (`mirante allow <perm>`): the card in the app
      disappears without a click.

## D. Notifications

- [ ] App in the background, a task needs you: one macOS notification.
- [ ] App focused on that task's detail (or its project feed / grid tile): no notification.
- [ ] App focused on another task's detail: a notification.

## E. Detail (V1)

- [ ] Click a card: the detail opens with Steps; Esc returns home.
- [ ] Terminal tab: Claude's TUI is live and typing works; resizing the window resizes it.
      Codex shows a read-only stream with the note.
- [ ] **Steer now** mid-turn: the feed shows "you · steer", then "message steered" when the
      transcript confirms it. **Queue**: "you · queued", delivered after the turn.
- [ ] Diff tab after a turn: files, +/− counts and the patch match `git diff` in the
      worktree.
- [ ] Ready for review card: **Open PR** is disabled until the branch is pushed, then opens
      GitHub's PR page in the browser.

## F. Grid (V2) and command bar

- [ ] ⌘G with 3+ live agents: tiles show last lines and status; allow/deny inline works; the
      mini input steers (Enter) and queues (⌥Enter).
- [ ] ⌘K → type part of a task title → Enter opens it; `@claude …` in ⌘K pre-fills the
      dispatch box.

## G. Memory guard

- [ ] The left rail shows system pressure and the heaviest sessions.
- [ ] **Hibernate** on an idle task: the process ends, the task shows "hibernated";
      **Wake** resumes the same conversation.
- [ ] Follow `docs/dogfood/memory-stress.md` §B with the app open: the pressure level and the
      "hibernate suggested" notice appear in the panel.

## H. Look and feel

- [ ] Light and dark (⇧⌘J, and the OS setting with theme "system") are both readable.
- [ ] Every action above works with the keyboard alone (Tab, Enter, arrows in tabs and ⌘K).
- [ ] VoiceOver reads card titles, permission groups and the connection banner.

## Log

| Date | Section | Result | Notes |
|---|---|---|---|
```

- [ ] **Step 2: Run and confirm**

Run: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace --all-targets --features mirante-bridge/fake -- -D warnings`

Expected: exit 0; output contains `test result: ok`

- [ ] **Step 3: Run and confirm**

Run: `pnpm --dir apps/cockpit test && pnpm --dir apps/cockpit typecheck`

Expected: exit 0; output contains `Tests  78 passed (78)`

- [ ] **Step 4: Commit**

```bash
git add docs/dogfood/cockpit.md
git commit -F- <<'EOF'
docs(dogfood): cockpit checklist

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```


## Self-review: requirements → tasks

| Requirement | Where | Proved by |
|---|---|---|
| Tauri 2 + React + Vite + TS; xterm.js terminals | 7, 8, 14 | `TerminalTab.test.tsx`; Task 20 builds and launches the `.app` |
| Rust side is a thin client over the socket, reusing `mirante-ipc` | 4–6, 8 | bridge tests against `FakeDaemon` (12); the shell is six forwarding commands |
| No new daemon features unless necessary, listed first | 1–3 (+ CLI split in 4) | "Daemon changes this plan makes" at the top; daemon/hub/server tests |
| subscribe → frontend; attach bytes both ways; resize | 5, 6, 8, 14 | `stream::tests` (resume, new boot, backoff), `attach::tests`, `TerminalTab.test.tsx` (`session.resize` on ResizeObserver) |
| No network / login / telemetry; CSP locked; minimal capabilities | 8 | `security.test.ts` (CSP directives, no remote origins, capabilities = notification + github.com opener) |
| Native notification on needs_you, suppressed when visible in the focused window | 18 | `notify.test.ts`, `App.test.tsx` (focused + on screen = quiet, unfocused = notify, first snapshot = quiet) |
| Single event-sourced store, snapshot + seq, reconnect since_seq/boot_id | 5, 9, 10 | `state.test.ts` (dedupe by seq, new boot, history merge), `cockpit.test.ts` (resume path), `stream::tests` |
| Dispatch parser `@agent repo/branch goal` with tests | 11, 13 | `dispatch.test.ts` (13 cases), `Home.test.tsx` (create → start) |
| V3 home: rail, feed with live cards, permission Allow once/Deny, done card with diff + Open PR, board | 12, 13 | `AgentCard.test.tsx`, `Home.test.tsx` |
| V1 detail: grouped list, tabs Steps/Terminal/Diff/Memory placeholder, Steer now / Queue | 14, 15 | `Detail.test.tsx` |
| V2 grid 2–6 tiles, inline allow/deny, mini input; ⌘K | 16, 17, 18 | `Grid.test.tsx`, `App.test.tsx` (⌘G, ⌘K, Esc) |
| Codex "auto-edits" label; grantRoot "session-wide grant" label | 12 | `AgentCard.test.tsx`, `labels.test.ts` |
| Memory guard: per-session memory, pressure, hibernate / wake | 9, 19 | `MemoryPanel.test.tsx` (incl. METHOD_NOT_FOUND on an older daemon) |
| Accessibility basics, shortcuts, dark/light | 12–19 | tests query by role/name (articles, groups, tabs, alerts, combobox); `shortcuts.test.ts`, `theme.test.ts`; checklist §H for VoiceOver |
| Vitest + Testing Library; Rust bridge tests with a fake daemon | all | 78 Vitest tests, 12 bridge tests |
| WebDriver-free smoke via `tauri build --debug` against a fake daemon | 20 | `scripts/smoke.sh` → `SMOKE PASS` |
| Dev run instructions; unsigned macOS `.app` | 20 | `apps/cockpit/README.md`; release bundle step |
| Dogfood checklist | 21 | `docs/dogfood/cockpit.md` |
| Spec §2: steer falls back to cancel + re-prompt when unsupported | 15 | `Detail.test.tsx` ("Interrupt and send") |
| Spec §2: permission requests survive a restart | 9, 10 | pending cards reload from `permission.list` after every snapshot |
| FINDINGS rule 5: trust dialog only answered by the user | 12 | `AgentCard.test.tsx` (trust card) |

Placeholder scan: none. Type names were checked across tasks (`StreamMsg`, `Opened`, `BridgeError` ↔ `lib/protocol.ts`; `Perm`, `FeedItem`, `Column`; `Env`); the replay below compiles each task's state.

## Decisions

- **Bridge as its own crate, Tauri shell as its own workspace.** Everything that can be wrong (framing, allowlist, resume, attach lifecycle) is tested without a window; the root `cargo test --workspace` never needs WebKit or Tauri's 400+ crates.
- **One connection per RPC call** (like the CLI). A `session.start` can block 45 s; a pooled connection would make a permission click wait behind it.
- **The stream is a hint** (spec §4): events update the store at once for responsiveness, and every touched task is re-read with `task.get` 120 ms later (coalesced). After a resumed subscription the whole list is re-read.
- **Feed history from the daemon (`task.events`, 200 per task)** rather than caching on the client: the daemon already persists every event; the app stays stateless across restarts except the repo list, theme (localStorage) and nothing else.
- **Attach output as base64 over a Tauri Channel**: binary-safe, simple, measured fine for TUI repaint bursts; switch to raw `InvokeResponseBody` only if profiling says so.
- **"Ready for review" = idle/ended with at least one finished turn**; idle tasks that never ran are not on the board (they are in the feed).
- **"Visible" for notification suppression**: home = cards of the selected project (the board is a summary and does not count), detail = that task, grid = its tiles.
- **Diff base = the repo checkout's current branch** (what `task.create` uses by default); per-turn checkpoint refs (spec §4) remain for a later plan.
- **`freezePrototype` stays off.** With it on, the built app loaded but never reached the bridge (no IPC call at all) — found by the smoke test while writing this plan.

## Risks and open items

- **`tauri dev` was not run** (the environment blocked launching dev servers). `devCsp` allows Vite's inline preamble and HMR websocket on `127.0.0.1:1420`; if the dev window is blank, check the WebKit inspector for CSP errors first. The production bundle is verified.
- **Not verified with real agents.** Everything ran against `FakeDaemon` / the fake daemon binary; the real-agent pass is `docs/dogfood/cockpit.md`.
- **Claude steps are tool-level only** (from hooks): no assistant prose in the Claude feed. Adding `agent_message` steps from the transcript tailer is a small follow-up in the hub.
- **Diff when the user changed the repo checkout's branch** after dispatch: the base follows the checkout, so the diff can include unrelated commits. Recording the base at `task.create` needs a migration; deferred.
- **macOS notifications for an unsigned debug app** may show under "Mirante" without a permission prompt or not at all until the app is in `/Applications`; checklist §D covers it.
- **Unsigned bundle**: local use only; quarantine note in the README.
- **jsdom pinned to 29** (30+ needs Node ≥ 22.22); bump with Node.

## Verification record (scratch clone of `84b90fa`, 2026-11-07)

Replayed every task in order with a script that writes each file, applies each Replace exactly once (anchors checked unique), runs every "fail" step (all failed as stated) and every "pass" step (all passed), and commits with the listed paths (no stray files left):

- Rust: `cargo build --workspace && cargo test --workspace` green (incl. the CLI e2e suite, which needs the build first); `cargo clippy --workspace --all-targets --features mirante-bridge/fake -- -D warnings` clean; `mirante-bridge` 12 tests; Tauri shell `cargo clippy -D warnings` clean.
- Frontend: 78 Vitest tests in 18 files green; `tsc --noEmit` clean; `vite build` ok.
- `pnpm tauri build --debug --bundles app` → `Mirante.app`; `scripts/smoke.sh` → `SMOKE PASS` (app subscribed, received the snapshot over the channel, loaded 3 histories, permissions and `daemon.status`).
- `pnpm tauri build --bundles app` (release, unsigned) → `Mirante.app` (≈11 MiB).

Not verified: `tauri dev`; any interaction through the real window (clicks, notifications, VoiceOver); real Claude/Codex sessions.

## Execution handoff

Plan complete and saved to `docs/plans/2026-11-07-plan-3-tauri-cockpit.md`. Two execution options:

1. **Subagent-Driven (recommended)** — a fresh subagent per task, review between tasks (REQUIRED SUB-SKILL: superpowers:subagent-driven-development).
2. **Inline Execution** — execute tasks in one session with checkpoints (REQUIRED SUB-SKILL: superpowers:executing-plans).
