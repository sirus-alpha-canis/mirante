# Mirante Plan 2b — daemon, agent drivers and CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** At the end of this plan the founder can replace cmux in daily work with the `mirante` CLI: dispatch Claude Code and Codex tasks into their own worktrees, see every task's real status, attach to a task's live terminal (Claude) or event stream (Codex), steer or queue messages mid-run, answer permission requests once, interrupt, stop everything and resume later without losing context.

**Architecture:** Three binaries over one shared wire crate. `mirante-daemon` is the single owner of state: it keeps Plan 2a's `Store`, one live session per task (an agent driver plus status), and serves newline-delimited JSON-RPC 2.0 on `~/.mirante/daemon.sock` (0600, peer-uid checked). `mirante` (CLI) and `mirante-hook` (Claude Code hook command) are thin synchronous clients. Two drivers sit behind one `Driver` trait: Claude Code runs unmodified and interactive in a PTY (status from hooks fused with the transcript tail by Plan 2a's `StatusEngine`; the screen is read only to act on dialogs), Codex runs as `codex app-server` over JSON-RPC stdio (status from protocol notifications). Drivers report through a channel drained by one pump thread, never by calling the hub, so no driver thread can deadlock the daemon.

**Tech Stack:** Rust 1.99 (edition 2021), std threads (no async runtime), `mirante-core` (Plan 2a: store, status engine, delivery classifier, transcript tailer, worktrees, Claude launch config), serde/serde_json, `libc` 0.2 (peer uid, `flock`, `setsid`, termios), `portable-pty` 0.8 and `vt100` 0.18 (as proven in the Week-1 spikes). No CLI-parsing or async dependencies.

**Inputs:** `docs/spec/2026-11-07-cockpit-prototype-design.md` (spec), `docs/spikes/FINDINGS.md` (authoritative measured rules, including Codex rules 8–9 and the terms notes), `docs/plans/2026-11-07-plan-2a-mirante-core.md` and, authoritative over it, the code in `crates/mirante-core/src` (only its public API is used: `StatusEngine::{new, apply, status, turn_open}`, `Input::{Hook, Transcript, PermissionAnswered}`, `Transition`, the `Store` methods, `TranscriptTail::{new, poll, offset}` with its `more` flag, `WorktreeManager`, `claude_launch`), the spike binaries in `crates/spike-kit/src/bin` (proven PTY handling: trust dialog default "No, exit", cursor-position answers, Down+Enter, typing pause, exact option matching, env scrub, real binaries; Codex method names), and the Codex app-server JSON schema generated in Week 1 (`codex app-server generate-json-schema`, Codex 0.167) for the approval request/response shapes.

**How this plan was verified:** every task below was replayed in order in a scratch clone of the repository (base commit `99b8a2a`, which already includes the concurrent `status.rs` fixes): each "fails" step was run and failed with the quoted error, each "passes" step passed with the quoted counts, and the two manual smoke tests were run with the quoted output. The code in this plan is byte-for-byte the replayed code.

**Ground rules for every task:**
- Shell setup for every command: `export PATH=/opt/homebrew/opt/rustup/bin:$PATH && cd ~/Documents/Develop/mirante`.
- The repo has the local commit e-mail set (`git config --local user.email` → `pedrohenriquecandidoferreira@hotmail.com`); check once before the first commit, do not change it.
- Work on the current branch (solo dev, no per-plan branches).
- `mirante-core` is not modified by this plan. `spike-kit` only gets one file renamed (Task 1). Both must keep passing (`spike-kit`: 61 tests; `mirante-core`: 141 tests at `99b8a2a`; if more core fixes land before you start, use the count `cargo test -p mirante-core` prints before Task 1).
- Code blocks are formatted as `rustfmt --edition 2021 --config max_width=120,use_small_heuristics=Max` formats them. Do not run a plain `cargo fmt` on the workspace.
- Tests never touch `~/.mirante`, your Claude or Codex config, or a real repository: every test uses a fresh `/tmp/mr-…` directory (short on purpose: macOS limits Unix socket paths to ~104 bytes). Tests that need the real `claude` / `codex` are `#[ignore]` and run only by hand (Task 17, Task 18).
- Safety rules, all enforced in code and asserted by tests: never `--dangerously-skip-permissions` or any bypass mode; never auto-approve (Claude: only an exact one-time "Yes"/"No" is ever typed; Codex: `approvalPolicy: "on-request"`, `approvalsReviewer: "user"`, only one-time decisions); the trust dialog is answered only by the user; nested-session and cmux variables are scrubbed, auth variables never touched.

**Out of scope (later plans):** Tauri UI (Plan 3) and with it: queue editing / reordering, a step feed and per-turn diffs (hidden checkpoint refs), on-screen suppression of notifications, showing the steer/queue verdict next to each message (2b records it as a `message_delivery` event). Also: cost/token ceilings, protected-path suggestions, repo `setup`/`teardown` scripts, token redaction in logs, quota display (FINDINGS rule 7, open), Windows. Linux compiles the `SO_PEERCRED` branch but is not exercised by this plan (macOS only).

---

## File structure

```
Cargo.toml                                     # workspace: + mirante-ipc, mirante-hook, mirante-daemon, mirante-cli
crates/spike-kit/src/bin/spike-hook.rs         # renamed from mirante-hook.rs (binary name collision)
crates/mirante-ipc/                            # shared wire crate (sync std only)
  src/lib.rs                                   # modules, re-exports, test_dir::TestDir (short /tmp dirs)
  src/paths.rs                                 # ~/.mirante layout ($MIRANTE_HOME override), 0700 home
  src/rpc.rs                                   # JSON-RPC 2.0 line framing, error codes
  src/types.rs                                 # method names, params/results, TaskView, BootState, SendMode
  src/client.rs                                # blocking client: call, next_notification, into_raw (attach)
crates/mirante-hook/                           # bin `mirante-hook` (Claude Code hook command)
  src/lib.rs                                   # deliver(): forward to daemon synchronously, else spool
  src/main.rs                                  # stdin → deliver(), prints nothing, exits 0
crates/mirante-daemon/                         # bin `mirante-daemon`
  src/lib.rs
  src/main.rs                                  # flock, open store, recover, pump, serve
  src/driver/mod.rs                            # Driver trait, DriverEvent, Sink, Attachment, Fanout, factory trait
  src/driver/fake.rs                           # #[cfg(test)] FakeDriver / FakeFactory
  src/driver/real.rs                           # RealFactory: launches real claude / codex
  src/driver/screen.rs                         # Claude TUI dialog matching (UI actions only, never status)
  src/driver/pty.rs                            # PtyProcess: PTY + vt100 mirror + CPR answers + fan-out
  src/driver/claude.rs                         # ClaudeDriver: boot/trust, typist, Esc, answers, transcript tail
  src/driver/codex_status.rs                   # CodexEngine: status from app-server traffic (rule 8)
  src/driver/codex_rpc.rs                      # RpcConn: JSON-RPC over stdio
  src/driver/codex.rs                          # CodexDriver: handshake, steer/start, approvals, read-only stream
  src/hub/mod.rs                               # Hub, State, Live, HubError, task CRUD, publish/subscribe, testkit
  src/hub/session.rs                           # start/resume/trust/stop, hooks, driver events, queue pump, verdicts
  src/hub/messages.rs                          # steer/queue send, permission list/answer
  src/recover.rs                               # daemon start: end stale sessions, ingest hook spool
  src/server.rs                                # socket (0600, peer uid), dispatch, subscribe, attach upgrade
  tests/real_agents.rs                         # #[ignore] real Claude / Codex through the daemon
crates/mirante-cli/                            # bin `mirante` + lib `mirante_cli`
  src/lib.rs, src/args.rs, src/render.rs       # parser, tables
  src/term.rs, src/daemon_ctl.rs               # raw mode / window size, auto-start
  src/main.rs                                  # commands
  tests/fake-claude.sh, tests/e2e.rs           # end to end through the real binaries with a fake claude
docs/dogfood/2026-11-07-plan-2b-dogfood.md     # Task 18: the founder's acceptance checklist and 2-week log
```

Dependency order: `mirante-ipc` → `mirante-hook` → daemon `driver` trait → `hub` (core → `session` → `messages`) → `recover` → `server` + binary → CLI → `screen` + `pty` → `claude` → `codex_status` + `codex_rpc` → `codex` → end to end.

## Runtime picture

```
mirante (CLI) ──┐                               ┌── pump thread ◄── DriverEvent channel ◄──┐
mirante-hook ───┼── ~/.mirante/daemon.sock ──► server threads ──► Hub (Mutex<State>) ──► Store (SQLite WAL)
watch / attach ─┘     (0600, peer uid)          │                                          │
                                                └── Live per task: Driver ─────────────────┤
                         Claude: PtyProcess(reader, waiter) + typist + boot watcher + transcript tail
                         Codex:  RpcConn(reader) + worker  ◄── codex app-server (stdio)
claude (in PTY) ──hook──► mirante-hook ──hook.event──► daemon   (or ~/.mirante/hook-spool.jsonl when down)
```

## Protocol summary

| Method | Params | Result |
|---|---|---|
| `task.create` | `{repo, agent, branch, base?, goal, title?}` | `TaskView` |
| `task.list` | `{archived?}` or null | `[TaskView]` |
| `task.get` / `task.archive` | `{task}` | `TaskView` |
| `session.start` / `session.resume` | `{task, prompt?}` | `{task, boot, resumed, screen}` |
| `session.trust` | `{task, accept}` | same as start |
| `session.stop` | `{task}` | `TaskView` |
| `session.interrupt` | `{task}` | `{}` |
| `session.resize` | `{task, rows, cols}` | `{}` (code 3 when the agent cannot resize) |
| `message.send` | `{task, text, mode: steer\|queue}` | `{delivered, queued}` |
| `permission.list` | — | `[PermissionView]` |
| `permission.answer` | `{id, allow}` | `PermissionRequest` |
| `events.subscribe` | `{since_seq?, boot_id?}` or null | `{boot_id, seq, tasks, resumed}` then `event` notifications (`Event` with `seq`, `boot_id`); with a matching `boot_id` it resumes: `resumed`, no tasks, missed events replayed |
| `attach` | `{task, rows, cols}` | `{input}` then raw bytes both ways until either side closes |
| `hook.event` | `{task, payload, ts_ms, event_id?, received_ts_ms?, truncated?, session?}` | `{}` (deduped by `event_id`) |
| `daemon.status` / `daemon.shutdown` | — | `{pid, boot_id, version, live_sessions}` / `{}` |

Error codes: JSON-RPC reserved (`-32700`, `-32600`, `-32601`, `-32602`, `-32603`) plus `1` not found, `2` conflict, `3` unsupported by this agent, `4` driver failure, `5` a dialog needs you, `6` forbidden (peer uid), `7` busy (connection limit; retry).

## Test counts after each task (verified)

| Task | Crate under test | Passing |
|---|---|---|
| 1 | mirante-ipc | 3 |
| 2 | mirante-ipc | 9 |
| 3 | mirante-ipc | 12 |
| 4 | mirante-hook | 3 |
| 5 | mirante-daemon | 2 |
| 6 | mirante-daemon | 9 |
| 7 | mirante-daemon | 24 |
| 8 | mirante-daemon | 33 |
| 9 | mirante-daemon | 35 |
| 10 | mirante-daemon | 41 |
| 11–12 | mirante-cli | 8 |
| 13 | mirante-daemon | 50 |
| 14 | mirante-daemon | 58 |
| 15 | mirante-daemon | 66 |
| 16 | mirante-daemon | 76 |
| 17 | e2e 2; real_agents 2 ignored | — |
| 18 | workspace | 303 passed, 2 ignored (8 cli + 2 e2e + 141 core + 76 daemon + 3 hook + 12 ipc + 61 spike-kit) |

---

## Phase 2b-i — IPC, hook transport, daemon core, CLI skeleton

### Task 1: Workspace, spike hook rename, `mirante-ipc` paths

**Files:**
- Modify: `Cargo.toml`
- Rename: `crates/spike-kit/src/bin/mirante-hook.rs → crates/spike-kit/src/bin/spike-hook.rs`
- Create: `crates/mirante-ipc/Cargo.toml`
- Create: `crates/mirante-ipc/src/lib.rs`
- Create: `crates/mirante-ipc/src/paths.rs`

The new `mirante-hook` crate (Task 4) produces a binary named `mirante-hook`; `spike-kit` already has one with that name, and two workspace binaries with one name collide in `target/`. The spike logger is renamed `spike-hook` (its code is unchanged). **If you re-run a Week-1 spike, point its `~/.mirante-spike/settings.json` at `target/debug/spike-hook`**: `target/debug/mirante-hook` is now the forwarding hook.

`mirante-ipc` is the shared wire crate (sync std only) used by the daemon, the CLI and the hook. This task adds its on-disk layout: everything under one 0700 directory, `$MIRANTE_HOME` or `~/.mirante`, never the current directory. `test_dir::TestDir` makes short `/tmp/mr-…` directories: Unix socket paths are limited to about 104 bytes on macOS, and the per-user temp dir (`/var/folders/…`) is too long for a socket path.

- [ ] **Step 1: Rename the spike hook binary**

```bash
git mv crates/spike-kit/src/bin/mirante-hook.rs crates/spike-kit/src/bin/spike-hook.rs
```

- [ ] **Step 2: Write the failing tests**

`Cargo.toml`:
```toml
[workspace]
members = [
    "crates/spike-kit",
    "crates/mirante-core",
    "crates/mirante-ipc",
]
resolver = "2"
```

`crates/mirante-ipc/Cargo.toml`:
```toml
[package]
name = "mirante-ipc"
version = "0.1.0"
edition = "2021"
publish = false
license = "Apache-2.0"

[dependencies]
mirante-core = { path = "../mirante-core" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

`crates/mirante-ipc/src/lib.rs`:
```rust
//! Wire protocol between `mirante-daemon` and its clients (`mirante`, `mirante-hook`):
//! newline-delimited JSON-RPC 2.0 over a Unix socket, the parameter and result types, and
//! the on-disk layout under `~/.mirante`. Synchronous std I/O only.
pub mod paths;

pub use paths::Paths;

/// A short unique directory under `/tmp`, removed on drop. Unix socket paths are limited to
/// ~104 bytes on macOS, so tests must not use the long per-user temp dir.
#[doc(hidden)]
pub mod test_dir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    pub struct TestDir(PathBuf);

    impl TestDir {
        pub fn new() -> TestDir {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let path = PathBuf::from(format!("/tmp/mr-{}-{n}-{nanos:x}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create test dir");
            TestDir(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Default for TestDir {
        fn default() -> Self {
            TestDir::new()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
```

`crates/mirante-ipc/src/paths.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    #[test]
    fn mirante_home_wins_then_home_dot_mirante_and_never_cwd() {
        let p = Paths::resolve(Some("/x/m".into()), Some("/home/u".into())).unwrap();
        assert_eq!(p.socket(), PathBuf::from("/x/m/daemon.sock"));
        let p = Paths::resolve(None, Some("/home/u".into())).unwrap();
        assert_eq!(p.database(), PathBuf::from("/home/u/.mirante/mirante.sqlite"));
        let p = Paths::resolve(Some("".into()), Some("/home/u".into())).unwrap();
        assert_eq!(p.home(), Path::new("/home/u/.mirante"));
        assert!(Paths::resolve(None, None).is_err());
        assert!(Paths::resolve(Some("".into()), Some("".into())).is_err());
    }

    #[test]
    fn layout_is_under_home() {
        let p = Paths::new(Path::new("/h"));
        assert_eq!(p.spool(), PathBuf::from("/h/hook-spool.jsonl"));
        assert_eq!(p.worktrees(), PathBuf::from("/h/worktrees"));
        assert_eq!(p.session_dir("t1"), PathBuf::from("/h/sessions/t1"));
        assert_eq!(p.log(), PathBuf::from("/h/daemon.log"));
        assert_eq!(p.lock(), PathBuf::from("/h/daemon.lock"));
    }

    #[test]
    fn ensure_home_creates_a_private_directory() {
        let d = TestDir::new();
        let p = Paths::new(&d.path().join("home"));
        p.ensure_home().unwrap();
        let mode = std::fs::metadata(p.home()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        std::fs::set_permissions(p.home(), std::fs::Permissions::from_mode(0o755)).unwrap();
        p.ensure_home().unwrap();
        assert_eq!(std::fs::metadata(p.home()).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test -p mirante-ipc`

Expected: FAIL to compile; the first error is
```
error[E0432]: unresolved import `paths::Paths`
```

- [ ] **Step 4: Implement**

Add above the test module in `crates/mirante-ipc/src/paths.rs`:
```rust
//! On-disk layout. Everything lives under one directory (mode 0700): `$MIRANTE_HOME`, else
//! `$HOME/.mirante`. Tests point `MIRANTE_HOME` at a temp dir.
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Overrides the home directory (also passed to every spawned agent, so its hooks find
/// the same daemon).
pub const HOME_ENV: &str = "MIRANTE_HOME";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
}

impl Paths {
    pub fn new(home: &Path) -> Paths {
        Paths { home: home.to_path_buf() }
    }

    /// `$MIRANTE_HOME`, else `$HOME/.mirante`. Fails when neither is set: never falls back
    /// to the current directory.
    pub fn from_env() -> io::Result<Paths> {
        Paths::resolve(std::env::var_os(HOME_ENV), std::env::var_os("HOME"))
    }

    pub fn resolve(mirante_home: Option<OsString>, home: Option<OsString>) -> io::Result<Paths> {
        if let Some(h) = mirante_home.filter(|h| !h.is_empty()) {
            return Ok(Paths::new(Path::new(&h)));
        }
        match home.filter(|h| !h.is_empty()) {
            Some(h) => Ok(Paths::new(&Path::new(&h).join(".mirante"))),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "neither MIRANTE_HOME nor HOME is set")),
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join("daemon.sock")
    }

    pub fn database(&self) -> PathBuf {
        self.home.join("mirante.sqlite")
    }

    /// Hook payloads that could not reach the daemon; ingested on the next daemon start.
    pub fn spool(&self) -> PathBuf {
        self.home.join("hook-spool.jsonl")
    }

    pub fn worktrees(&self) -> PathBuf {
        self.home.join("worktrees")
    }

    /// Per-task scratch: the generated Claude settings file, agent stderr logs.
    pub fn session_dir(&self, task_id: &str) -> PathBuf {
        self.home.join("sessions").join(task_id)
    }

    pub fn log(&self) -> PathBuf {
        self.home.join("daemon.log")
    }

    /// `flock`ed by the running daemon: one daemon per home.
    pub fn lock(&self) -> PathBuf {
        self.home.join("daemon.lock")
    }

    /// Creates the home directory with mode 0700, and tightens an existing one.
    pub fn ensure_home(&self) -> io::Result<()> {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.home)?;
        std::fs::set_permissions(&self.home, std::fs::Permissions::from_mode(0o700))
    }
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p mirante-ipc`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 6: Check spike-kit still passes**

Run: `cargo test -p spike-kit 2>&1 | grep 'test result' | head -1`

Expected (plus `; finished in …`):
```
test result: ok. 61 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/spike-kit crates/mirante-ipc
git commit -F - <<'EOF'
feat(ipc): mirante-ipc crate with the ~/.mirante layout; rename spike hook bin

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 2: JSON-RPC line framing and the method/param types

**Files:**
- Modify: `crates/mirante-ipc/src/lib.rs`
- Create: `crates/mirante-ipc/src/rpc.rs`
- Create: `crates/mirante-ipc/src/types.rs`

Newline-delimited JSON-RPC 2.0. Request ids are integers; a response to an unparseable request has `id: null`. Error codes are the JSON-RPC reserved ones plus Mirante's own small positive codes, so the CLI can tell "not found" from "conflict" from "unsupported by this agent" from "a dialog needs you". Lines longer than 16 MiB are rejected instead of buffered without bound (spec §8: no silent failures, bounded memory).

`types.rs` names every method and its parameters. A task is referenced by full id, branch name, or a unique id suffix of 4+ characters (UUID v7 *prefixes* are time-based and collide for tasks created together; suffixes do not). `TaskView` flattens the stored `Task` and adds live facts: boot state, how many things need the user, queue length, and a status detail (used for FINDINGS rule 8).

- [ ] **Step 1: Write the failing tests**

`crates/mirante-ipc/src/lib.rs`:
```rust
//! Wire protocol between `mirante-daemon` and its clients (`mirante`, `mirante-hook`):
//! newline-delimited JSON-RPC 2.0 over a Unix socket, the parameter and result types, and
//! the on-disk layout under `~/.mirante`. Synchronous std I/O only.
pub mod paths;
pub mod rpc;
pub mod types;

pub use paths::Paths;
pub use rpc::RpcError;

/// A short unique directory under `/tmp`, removed on drop. Unix socket paths are limited to
/// ~104 bytes on macOS, so tests must not use the long per-user temp dir.
#[doc(hidden)]
pub mod test_dir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    pub struct TestDir(PathBuf);

    impl TestDir {
        pub fn new() -> TestDir {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let path = PathBuf::from(format!("/tmp/mr-{}-{n}-{nanos:x}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create test dir");
            TestDir(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Default for TestDir {
        fn default() -> Self {
            TestDir::new()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
```

`crates/mirante-ipc/src/rpc.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_and_response_roundtrip_as_single_lines() {
        let mut out = Vec::new();
        write_line(&mut out, &Request::new(7, "task.list", json!({"archived": false}))).unwrap();
        write_line(&mut out, &Response::ok(7, json!([1, 2]))).unwrap();
        write_line(&mut out, &Response::err(None, RpcError::new(code::PARSE, "bad json"))).unwrap();
        let text = String::from_utf8(out.clone()).unwrap();
        assert_eq!(text.lines().count(), 3);
        let mut r = std::io::Cursor::new(out);
        let req: Request = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!((req.id, req.method.as_str(), req.params["archived"].clone()), (7, "task.list", json!(false)));
        let ok: Response = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(ok.result, Some(json!([1, 2])));
        assert!(ok.error.is_none());
        let err: Response = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(err.id, None);
        assert_eq!(err.error.unwrap().code, code::PARSE);
        assert_eq!(read_line(&mut r).unwrap(), None);
    }

    #[test]
    fn ok_response_omits_error_and_error_response_omits_result() {
        let ok = serde_json::to_value(Response::ok(1, json!(null))).unwrap();
        assert!(ok.get("error").is_none());
        let err = serde_json::to_value(Response::err(Some(1), RpcError::new(1, "x"))).unwrap();
        assert!(err.get("result").is_none());
    }

    #[test]
    fn missing_params_default_to_null_and_crlf_is_trimmed() {
        let mut r = std::io::Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"daemon.status\"}\r\n".to_vec());
        let req: Request = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(req.params, Value::Null);
    }

    #[test]
    fn overlong_line_is_rejected() {
        let mut r = std::io::Cursor::new(vec![b'x'; MAX_LINE + 10]);
        assert_eq!(read_line(&mut r).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
```

`crates/mirante-ipc/src/types.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{Lifecycle, Status};
    use serde_json::json;

    #[test]
    fn task_view_flattens_the_task() {
        let task = Task {
            id: "t1".into(),
            title: "T".into(),
            repo_path: "/r".into(),
            worktree_path: "/w".into(),
            branch: "feat".into(),
            agent: AgentKind::Codex,
            goal: "g".into(),
            notes: String::new(),
            lifecycle: Lifecycle::Active,
            status: Status::NeedsYou,
            created_at: 1,
            updated_at: 2,
        };
        let v = TaskView { task, live: true, boot: Some(BootState::Ready), needs_you: 1, queued: 0, detail: None };
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j["id"], "t1");
        assert_eq!(j["status"], "needs_you");
        assert_eq!(j["boot"], "ready");
        assert_eq!(serde_json::from_value::<TaskView>(j).unwrap(), v);
    }

    #[test]
    fn optional_fields_default() {
        let c: CreateTask =
            serde_json::from_value(json!({"repo": "/r", "agent": "claude", "branch": "b", "goal": "g"})).unwrap();
        assert_eq!((c.base, c.title), (None, None));
        let s: StartSession = serde_json::from_value(json!({"task": "t"})).unwrap();
        assert_eq!(s.prompt, None);
        let l: ListParams = serde_json::from_value(json!({})).unwrap();
        assert!(!l.archived);
        let m: SendMessage = serde_json::from_value(json!({"task": "t", "text": "x", "mode": "queue"})).unwrap();
        assert_eq!(m.mode, SendMode::Queue);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-ipc`

Expected: FAIL to compile; the first error is
```
error[E0432]: unresolved import `rpc::RpcError`
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-ipc/src/rpc.rs`:
```rust
//! JSON-RPC 2.0 messages, one per line (`\n`-terminated JSON). Request ids are integers.
use std::fmt;
use std::io::{self, BufRead, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSONRPC: &str = "2.0";
/// Longest accepted line. A hook payload can carry a large tool input; anything bigger is
/// rejected instead of buffered without limit.
pub const MAX_LINE: usize = 16 << 20;

/// Error codes: the JSON-RPC reserved ones plus Mirante's own (small positive integers).
pub mod code {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL: i64 = -32603;
    /// Unknown task, session or permission request.
    pub const NOT_FOUND: i64 = 1;
    /// Valid request that conflicts with current state (task busy, already answered, ...).
    pub const CONFLICT: i64 = 2;
    /// The agent's driver cannot do this (typed, never a silent failure).
    pub const UNSUPPORTED: i64 = 3;
    /// The agent process or protocol failed.
    pub const DRIVER: i64 = 4;
    /// Refused because the task is waiting on the user (e.g. a permission dialog is open).
    pub const NEEDS_YOU: i64 = 5;
    /// Rejected by the daemon's peer check.
    pub const FORBIDDEN: i64 = 6;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into() }
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Request {
        Request { jsonrpc: JSONRPC.into(), id, method: method.into(), params }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    /// `None` only when the request itself could not be parsed.
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Response {
        Response { jsonrpc: JSONRPC.into(), id: Some(id), result: Some(result), error: None }
    }

    pub fn err(id: Option<u64>, error: RpcError) -> Response {
        Response { jsonrpc: JSONRPC.into(), id, result: None, error: Some(error) }
    }
}

/// Server-to-client message without an id (the event stream).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Notification {
    pub fn new(method: &str, params: Value) -> Notification {
        Notification { jsonrpc: JSONRPC.into(), method: method.into(), params }
    }
}

/// Writes `msg` as one JSON line and flushes.
pub fn write_line<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let mut buf = serde_json::to_vec(msg)?;
    buf.push(b'\n');
    w.write_all(&buf)?;
    w.flush()
}

/// Reads one line without its terminator. `Ok(None)` at end of stream. A line longer than
/// [`MAX_LINE`] or not UTF-8 is an `InvalidData` error.
pub fn read_line<R: BufRead>(r: &mut R) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    } else if buf.len() > MAX_LINE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message line too long"));
    }
    String::from_utf8(buf).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
```

Add above the test module in `crates/mirante-ipc/src/types.rs`:
```rust
//! Method names and their parameter / result types. A task may be referenced by its full
//! id, its branch name, or a unique suffix of its id (at least 4 characters).
use mirante_core::model::{AgentKind, PermissionRequest, Task};
use serde::{Deserialize, Serialize};

pub mod method {
    pub const TASK_CREATE: &str = "task.create";
    pub const TASK_LIST: &str = "task.list";
    pub const TASK_GET: &str = "task.get";
    pub const TASK_ARCHIVE: &str = "task.archive";
    pub const SESSION_START: &str = "session.start";
    pub const SESSION_RESUME: &str = "session.resume";
    pub const SESSION_TRUST: &str = "session.trust";
    pub const SESSION_STOP: &str = "session.stop";
    pub const SESSION_INTERRUPT: &str = "session.interrupt";
    pub const MESSAGE_SEND: &str = "message.send";
    pub const PERMISSION_LIST: &str = "permission.list";
    pub const PERMISSION_ANSWER: &str = "permission.answer";
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
    pub const ATTACH: &str = "attach";
    pub const HOOK_EVENT: &str = "hook.event";
    pub const DAEMON_STATUS: &str = "daemon.status";
    pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
    /// Notification carrying one `mirante_core::model::Event`.
    pub const EVENT: &str = "event";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    /// Deliver now: typed into Claude's TUI (absorbed at the next tool boundary) or
    /// `turn/steer` for Codex. Starts a turn when none is running.
    Steer,
    /// Hold in the daemon and deliver when the running turn ends.
    Queue,
}

/// Where a live session's agent process is in its startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootState {
    Booting,
    /// Claude's workspace-trust dialog is on screen and waits for the user's decision.
    TrustPrompt,
    Ready,
    Exited,
}

/// A task as clients see it: the stored task plus live-session facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    #[serde(flatten)]
    pub task: Task,
    pub live: bool,
    pub boot: Option<BootState>,
    /// Pending permission requests, plus one while a trust prompt waits.
    pub needs_you: usize,
    /// Messages held for delivery at the end of the running turn.
    pub queued: usize,
    /// Extra status detail, e.g. "turn done, 1 command still finishing".
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    pub task: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTask {
    pub repo: String,
    pub agent: AgentKind,
    pub branch: String,
    #[serde(default)]
    pub base: Option<String>,
    pub goal: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartSession {
    pub task: String,
    /// First message, delivered as soon as the agent is ready.
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartResult {
    pub task: String,
    pub boot: BootState,
    /// True when the agent's native session / thread was resumed (context restored).
    pub resumed: bool,
    /// Last lines of the agent's screen (Claude) for the trust prompt / exit reason.
    pub screen: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustAnswer {
    pub task: String,
    pub accept: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessage {
    pub task: String,
    pub text: String,
    pub mode: SendMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendResult {
    /// What actually happened: `Queue` when held, `Steer` when handed to the agent now.
    pub delivered: SendMode,
    /// Queue length after this call.
    pub queued: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionView {
    #[serde(flatten)]
    pub request: PermissionRequest,
    pub task_title: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerPermission {
    /// Full id or a unique suffix (at least 4 characters).
    pub id: String,
    pub allow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachParams {
    pub task: String,
    pub rows: u16,
    pub cols: u16,
}

/// After this response the connection carries raw bytes in both directions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachResult {
    /// False for a read-only stream (Codex): typed bytes are ignored.
    pub input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookParams {
    pub task: String,
    /// The hook's stdin, verbatim (parsed by the daemon so errors are reported, not lost).
    pub payload: String,
    pub ts_ms: i64,
}

/// First response of `events.subscribe`; `event` notifications with `seq > seq` follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub boot_id: String,
    pub seq: u64,
    pub tasks: Vec<TaskView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub boot_id: String,
    pub version: String,
    pub live_sessions: usize,
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-ipc`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-ipc
git commit -F - <<'EOF'
feat(ipc): newline-delimited JSON-RPC framing, typed errors and method params

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 3: Blocking socket client

**Files:**
- Modify: `crates/mirante-ipc/src/lib.rs`
- Create: `crates/mirante-ipc/src/client.rs`

`Client::call` writes one request and reads until the response with the same id, skipping notifications (an `events.subscribe` connection interleaves them). `next_notification` drives the event stream. `into_raw` hands over the connection after `attach`; the caller must keep reading from the returned `BufReader`, which may already hold raw bytes that arrived with the response.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-ipc/src/lib.rs`:
```rust
//! Wire protocol between `mirante-daemon` and its clients (`mirante`, `mirante-hook`):
//! newline-delimited JSON-RPC 2.0 over a Unix socket, the parameter and result types, and
//! the on-disk layout under `~/.mirante`. Synchronous std I/O only.
pub mod client;
pub mod paths;
pub mod rpc;
pub mod types;

pub use client::{Client, ClientError};
pub use paths::Paths;
pub use rpc::RpcError;

/// A short unique directory under `/tmp`, removed on drop. Unix socket paths are limited to
/// ~104 bytes on macOS, so tests must not use the long per-user temp dir.
#[doc(hidden)]
pub mod test_dir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    pub struct TestDir(PathBuf);

    impl TestDir {
        pub fn new() -> TestDir {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let path = PathBuf::from(format!("/tmp/mr-{}-{n}-{nanos:x}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create test dir");
            TestDir(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Default for TestDir {
        fn default() -> Self {
            TestDir::new()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
```

`crates/mirante-ipc/src/client.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::code;
    use crate::test_dir::TestDir;
    use serde_json::json;
    use std::io::Write;
    use std::os::unix::net::UnixListener;

    /// A one-connection server that answers with `script(request) -> lines`.
    fn serve_once(sock: &Path, script: fn(Request) -> Vec<String>) -> std::thread::JoinHandle<()> {
        let listener = UnixListener::bind(sock).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut w = stream;
            while let Some(line) = read_line(&mut r).unwrap() {
                let req: Request = serde_json::from_str(&line).unwrap();
                for l in script(req) {
                    w.write_all(format!("{l}\n").as_bytes()).unwrap();
                }
            }
        })
    }

    #[test]
    fn call_skips_notifications_and_returns_the_typed_result() {
        let d = TestDir::new();
        let sock = d.path().join("s.sock");
        let h = serve_once(&sock, |req| {
            vec![
                serde_json::to_string(&Notification::new("event", json!({"seq": 1}))).unwrap(),
                serde_json::to_string(&Response::ok(req.id, json!({"n": req.params["x"]}))).unwrap(),
            ]
        });
        let mut c = Client::connect(&sock).unwrap();
        let v: Value = c.call("echo", json!({"x": 5})).unwrap();
        assert_eq!(v, json!({"n": 5}));
        let v: Value = c.call("echo", json!({"x": 6})).unwrap();
        assert_eq!(v, json!({"n": 6}));
        drop(c);
        h.join().unwrap();
    }

    #[test]
    fn rpc_errors_are_typed() {
        let d = TestDir::new();
        let sock = d.path().join("s.sock");
        let h = serve_once(&sock, |req| {
            vec![serde_json::to_string(&Response::err(Some(req.id), RpcError::new(code::NOT_FOUND, "no task"))).unwrap()]
        });
        let mut c = Client::connect(&sock).unwrap();
        match c.call::<Value>("task.get", json!({"task": "x"})) {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, code::NOT_FOUND),
            other => panic!("{other:?}"),
        }
        drop(c);
        h.join().unwrap();
    }

    #[test]
    fn closed_connection_is_an_error_not_a_hang() {
        let d = TestDir::new();
        let sock = d.path().join("s.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let h = std::thread::spawn(move || drop(listener.accept().unwrap()));
        let mut c = Client::connect(&sock).unwrap();
        h.join().unwrap();
        assert!(matches!(c.call::<Value>("x", json!({})), Err(ClientError::Protocol(_)) | Err(ClientError::Io(_))));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-ipc`

Expected: FAIL to compile; the first error is
```
error[E0432]: unresolved imports `client::Client`, `client::ClientError`
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-ipc/src/client.rs`:
```rust
//! Blocking client for the daemon socket.
use std::fmt;
use std::io::{self, BufReader};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::rpc::{read_line, write_line, Notification, Request, Response, RpcError};

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    /// The daemon answered with an error.
    Rpc(RpcError),
    /// The daemon sent something that is not the expected JSON-RPC message.
    Protocol(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "daemon connection: {e}"),
            ClientError::Rpc(e) => write!(f, "{e}"),
            ClientError::Protocol(m) => write!(f, "daemon protocol: {m}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        ClientError::Io(e)
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        ClientError::Protocol(e.to_string())
    }
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Client {
    pub fn connect(socket: &Path) -> io::Result<Client> {
        let stream = UnixStream::connect(socket)?;
        Ok(Client { reader: BufReader::new(stream.try_clone()?), writer: stream, next_id: 1 })
    }

    /// Read and write timeout for the whole connection (`None` = wait forever).
    pub fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.writer.set_read_timeout(timeout)?;
        self.writer.set_write_timeout(timeout)
    }

    /// Sends one request and waits for its response, skipping notifications.
    pub fn call<T: DeserializeOwned>(&mut self, method: &str, params: impl Serialize) -> Result<T, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        write_line(&mut self.writer, &Request::new(id, method, serde_json::to_value(params)?))?;
        loop {
            let line = read_line(&mut self.reader)?
                .ok_or_else(|| ClientError::Protocol("daemon closed the connection".into()))?;
            let v: Value = serde_json::from_str(&line)?;
            if v.get("method").is_some() {
                continue;
            }
            let resp: Response = serde_json::from_value(v)?;
            if let Some(e) = resp.error {
                return Err(ClientError::Rpc(e));
            }
            if resp.id != Some(id) {
                continue;
            }
            return Ok(serde_json::from_value(resp.result.unwrap_or(Value::Null))?);
        }
    }

    /// The next notification (after `events.subscribe`). `Ok(None)` when the daemon closes.
    pub fn next_notification(&mut self) -> Result<Option<Notification>, ClientError> {
        loop {
            let Some(line) = read_line(&mut self.reader)? else {
                return Ok(None);
            };
            let v: Value = serde_json::from_str(&line)?;
            if v.get("method").is_some() {
                return Ok(Some(serde_json::from_value(v)?));
            }
        }
    }

    /// The raw connection, for `attach` after its response. Keep reading from the returned
    /// reader: it may already hold bytes that arrived with the response.
    pub fn into_raw(self) -> (BufReader<UnixStream>, UnixStream) {
        (self.reader, self.writer)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-ipc`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-ipc
git commit -F - <<'EOF'
feat(ipc): blocking client with typed RPC errors, notifications and raw handover

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 4: `mirante-hook`: synchronous forward, spool fallback

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/mirante-hook/Cargo.toml`
- Create: `crates/mirante-hook/src/lib.rs`
- Create: `crates/mirante-hook/src/main.rs`

Claude Code runs the hook command for every event (registered by `claude_launch::settings_json`, scoped by `MIRANTE_TASK_ID`). The hook forwards the raw payload to the daemon and waits for the ack (3 s timeout), so the daemon sees events in the order Claude fires them. When the daemon is unreachable, the payload is appended to `~/.mirante/hook-spool.jsonl` (mode 0600, one `write_all` per line so concurrent hooks cannot interleave); the daemon ingests it at its next start (Task 9). The hook prints nothing: stdout of some hook events becomes context for Claude. It always exits 0. Without `MIRANTE_TASK_ID` it does nothing at all.

- [ ] **Step 1: Write the failing tests**

`Cargo.toml`:
```toml
[workspace]
members = [
    "crates/spike-kit",
    "crates/mirante-core",
    "crates/mirante-ipc",
    "crates/mirante-hook",
]
resolver = "2"
```

`crates/mirante-hook/Cargo.toml`:
```toml
[package]
name = "mirante-hook"
version = "0.1.0"
edition = "2021"
publish = false
license = "Apache-2.0"

[dependencies]
mirante-core = { path = "../mirante-core" }
mirante-ipc = { path = "../mirante-ipc" }
serde_json = "1"
```

`crates/mirante-hook/src/main.rs`:
```rust
//! `mirante-hook`: see the library docs. Always exits 0 and prints nothing.
use std::io::Read;

use mirante_core::claude_launch::TASK_ENV;
use mirante_core::model::now_ms;
use mirante_ipc::Paths;

fn main() {
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let task = std::env::var(TASK_ENV).ok();
    if let Ok(paths) = Paths::from_env() {
        let _ = mirante_hook::deliver(&paths, task.as_deref(), &raw, now_ms());
    }
}
```

`crates/mirante-hook/src/lib.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::rpc::{read_line, write_line, Request, Response};
    use mirante_ipc::test_dir::TestDir;
    use serde_json::json;
    use std::io::BufReader;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    #[test]
    fn without_a_task_id_nothing_happens() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        assert_eq!(deliver(&paths, None, "{}", 1), Outcome::Skipped);
        assert_eq!(deliver(&paths, Some(""), "{}", 1), Outcome::Skipped);
        assert!(!paths.spool().exists());
    }

    #[test]
    fn daemon_down_spools_one_private_line_per_event() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        assert_eq!(deliver(&paths, Some("t1"), r#"{"hook_event_name":"Stop"}"#, 5), Outcome::Spooled);
        assert_eq!(deliver(&paths, Some("t1"), r#"{"hook_event_name":"SessionEnd"}"#, 6), Outcome::Spooled);
        let text = std::fs::read_to_string(paths.spool()).unwrap();
        let lines: Vec<HookParams> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].task, "t1");
        assert_eq!(lines[1].ts_ms, 6);
        assert_eq!(std::fs::metadata(paths.spool()).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn daemon_up_receives_the_payload_verbatim() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let listener = UnixListener::bind(paths.socket()).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let req: Request = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
            let mut w = stream;
            write_line(&mut w, &Response::ok(req.id, json!({}))).unwrap();
            req
        });
        let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#;
        assert_eq!(deliver(&paths, Some("t9"), payload, 42), Outcome::Forwarded);
        let req = server.join().unwrap();
        assert_eq!(req.method, method::HOOK_EVENT);
        let p: HookParams = serde_json::from_value(req.params).unwrap();
        assert_eq!((p.task.as_str(), p.payload.as_str(), p.ts_ms), ("t9", payload, 42));
        assert!(!paths.spool().exists());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-hook`

Expected: FAIL to compile; the first error is
```
error[E0425]: cannot find type `HookParams` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-hook/src/lib.rs`:
```rust
//! The Claude Code hook command. Claude runs it for every hook event with the payload on
//! stdin. It forwards the payload to the daemon synchronously (so the daemon sees events
//! in the order Claude fires them) and, when the daemon is unreachable, appends it to the
//! spool file that the daemon ingests on its next start. It prints nothing (hook stdout
//! can become context for Claude) and never fails the hook.
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

use mirante_ipc::types::{method, HookParams};
use mirante_ipc::{Client, ClientError, Paths};
use serde_json::Value;

/// How long the hook waits for the daemon before spooling.
pub const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No `MIRANTE_TASK_ID`: not a Mirante session, do nothing.
    Skipped,
    Forwarded,
    Spooled,
    /// Neither the daemon nor the spool file could be reached.
    Lost,
}

pub fn deliver(paths: &Paths, task: Option<&str>, payload: &str, ts_ms: i64) -> Outcome {
    let Some(task) = task.filter(|t| !t.is_empty()) else {
        return Outcome::Skipped;
    };
    let params = HookParams { task: task.to_string(), payload: payload.to_string(), ts_ms };
    match forward(paths, &params) {
        // The daemon got the payload; an RPC error (e.g. unparseable) is logged there.
        Ok(()) | Err(ClientError::Rpc(_)) => Outcome::Forwarded,
        Err(_) => match spool(paths, &params) {
            Ok(()) => Outcome::Spooled,
            Err(_) => Outcome::Lost,
        },
    }
}

fn forward(paths: &Paths, params: &HookParams) -> Result<(), ClientError> {
    let mut client = Client::connect(&paths.socket())?;
    client.set_timeout(Some(TIMEOUT))?;
    client.call::<Value>(method::HOOK_EVENT, params)?;
    Ok(())
}

/// Appends one JSON line (mode 0600). One `write_all` per line: concurrent hook processes
/// appending with O_APPEND cannot interleave mid-line.
pub fn spool(paths: &Paths, params: &HookParams) -> io::Result<()> {
    paths.ensure_home()?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(paths.spool())?;
    let mut line = serde_json::to_vec(params)?;
    line.push(b'\n');
    f.write_all(&line)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-hook`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/mirante-hook
git commit -F - <<'EOF'
feat(hook): mirante-hook forwards payloads to the daemon, spools when it is down

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 5: Daemon crate: the driver interface and a fake driver

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/mirante-daemon/Cargo.toml`
- Create: `crates/mirante-daemon/src/lib.rs`
- Create: `crates/mirante-daemon/src/driver/mod.rs`
- Create: `crates/mirante-daemon/src/driver/fake.rs`

Spec §4: every agent sits behind one interface; an unsupported operation is a typed result (`DriverError::Unsupported`), never a silent failure. Design rules that keep the daemon deadlock-free:

- Drivers never call the hub. They emit `DriverEvent`s into a channel (`Sink`), tagged with their session id; the hub's pump thread applies them under the hub lock, and events of a session that is no longer live are dropped.
- Driver methods are non-blocking (typing and protocol calls run on the driver's own worker threads), except `stop`, which the hub calls without holding its lock. So the hub may call drivers while locked.

Claude reports status through hooks (to the hub) plus `Transcript` lines (from the driver's tailer); Codex runs its own engine and reports `Status`. `FakeDriver` records calls for the hub tests; `FakeFactory` keeps every sink so tests can emit events.

The manifest already lists every dependency the daemon will need (`libc` for the socket peer check and `flock`, `portable-pty` + `vt100` for the Claude PTY, as in the Week-1 spikes). No async runtime: the daemon uses one thread per connection and per agent stream, which is simpler to test and plenty for a local tool with a handful of clients.

- [ ] **Step 1: Write the failing tests**

`Cargo.toml`:
```toml
[workspace]
members = [
    "crates/spike-kit",
    "crates/mirante-core",
    "crates/mirante-ipc",
    "crates/mirante-hook",
    "crates/mirante-daemon",
]
resolver = "2"
```

`crates/mirante-daemon/Cargo.toml`:
```toml
[package]
name = "mirante-daemon"
version = "0.1.0"
edition = "2021"
publish = false
license = "Apache-2.0"

[dependencies]
mirante-core = { path = "../mirante-core" }
mirante-ipc = { path = "../mirante-ipc" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
libc = "0.2"
portable-pty = "0.8"
vt100 = "0.18"
```

`crates/mirante-daemon/src/lib.rs`:
```rust
//! The Mirante daemon: the single owner of state. It keeps the `Store`, one live session per
//! task (agent process + status engine), serves the Unix socket, and turns hook events,
//! transcript lines and Codex notifications into persisted status, turns and permissions.
pub mod driver;
```

`crates/mirante-daemon/src/driver/fake.rs`:
```rust
//! Test double: records every call, lets the test emit events through the captured sink.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use super::{Attachment, Driver, DriverError, DriverEvent, DriverFactory, Sink, SpawnRequest};

#[derive(Default)]
pub struct FakeDriver {
    pub calls: Mutex<Vec<String>>,
    pub dialog: AtomicBool,
    pub fail_answers: AtomicBool,
    /// The attached client's output channel, kept open until `detach` (like a live agent).
    pub attached: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
}

impl FakeDriver {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Driver for FakeDriver {
    fn send(&self, text: &str) -> Result<(), DriverError> {
        self.record(format!("send:{text}"));
        Ok(())
    }
    fn interrupt(&self) -> Result<(), DriverError> {
        self.record("interrupt".into());
        Ok(())
    }
    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError> {
        if self.fail_answers.load(Ordering::SeqCst) {
            return Err(DriverError::NoDialog("fake".into()));
        }
        self.record(format!("answer:{key}:{allow}"));
        Ok(())
    }
    fn trust(&self, accept: bool) -> Result<(), DriverError> {
        self.record(format!("trust:{accept}"));
        Ok(())
    }
    fn dialog_open(&self) -> bool {
        self.dialog.load(Ordering::SeqCst)
    }
    fn tail_transcript(&self, path: &Path, offset: u64) {
        self.record(format!("tail:{}:{offset}", path.display()));
    }
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError> {
        self.record(format!("attach:{rows}x{cols}"));
        let (tx, rx) = mpsc::channel();
        tx.send(b"live".to_vec()).unwrap();
        *self.attached.lock().unwrap() = Some(tx);
        Ok(Attachment { id: 1, initial: b"screen".to_vec(), output: rx, input: true })
    }
    fn input(&self, _attachment: u64, bytes: &[u8]) -> Result<(), DriverError> {
        self.record(format!("input:{}", String::from_utf8_lossy(bytes)));
        Ok(())
    }
    fn detach(&self, attachment: u64) {
        self.record(format!("detach:{attachment}"));
        self.attached.lock().unwrap().take();
    }
    fn screen_tail(&self) -> String {
        "fake screen".into()
    }
    fn stop(&self) {
        self.record("stop".into());
    }
}

/// Hands out one shared `FakeDriver` and keeps every sink and spawn request.
#[derive(Default)]
pub struct FakeFactory {
    pub driver: Arc<FakeDriver>,
    pub sinks: Mutex<Vec<Sink>>,
    pub resumes: Mutex<Vec<Option<String>>>,
    pub fail: AtomicBool,
}

impl FakeFactory {
    pub fn last_sink(&self) -> Sink {
        self.sinks.lock().unwrap().last().cloned().expect("spawned")
    }

    pub fn emit(&self, event: DriverEvent) {
        self.last_sink().emit(event);
    }
}

impl DriverFactory for Arc<FakeFactory> {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(DriverError::Failed("spawn refused".into()));
        }
        self.resumes.lock().unwrap().push(req.resume);
        self.sinks.lock().unwrap().push(sink);
        Ok(self.driver.clone())
    }
}
```

`crates/mirante-daemon/src/driver/mod.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_drops_dead_receivers() {
        let mut f = Fanout::default();
        let (a, rx_a) = f.subscribe();
        let (_b, rx_b) = f.subscribe();
        f.send(b"x");
        assert_eq!(rx_a.recv().unwrap(), b"x");
        drop(rx_b);
        f.send(b"y");
        assert_eq!(rx_a.recv().unwrap(), b"y");
        f.remove(a);
        assert!(f.is_empty());
    }

    #[test]
    fn sink_tags_events_with_the_session() {
        let (tx, rx) = mpsc::channel();
        Sink::new("s1", tx).emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(rx.recv().unwrap(), ("s1".to_string(), DriverEvent::Boot(BootState::Ready)));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error[E0433]: cannot find module or crate `mpsc` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/driver/mod.rs`:
```rust
//! The agent driver interface. Every agent sits behind [`Driver`]; an operation the agent
//! cannot do is a typed [`DriverError::Unsupported`], never a silent failure. Drivers are
//! non-blocking (typing and protocol calls run on the driver's own threads) except
//! [`Driver::stop`], so the hub may call them while holding its lock.
//!
//! Drivers report back through a [`Sink`] (a channel drained by the hub's pump thread),
//! never by calling the hub directly, so a driver thread can never deadlock on the hub.
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc};

use mirante_core::model::{Status, Task};
use mirante_ipc::types::BootState;
use mirante_ipc::Paths;
use serde_json::Value;

#[cfg(test)]
pub mod fake;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The agent cannot do this (e.g. a trust prompt on Codex).
    Unsupported(&'static str),
    /// The dialog the operation targets is not on screen / not pending.
    NoDialog(String),
    Failed(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Unsupported(what) => write!(f, "not supported by this agent: {what}"),
            DriverError::NoDialog(m) => write!(f, "no matching dialog: {m}"),
            DriverError::Failed(m) => write!(f, "agent driver failed: {m}"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Failed(e.to_string())
    }
}

/// What a driver tells the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// Claude: one main-transcript line, for the hub's `StatusEngine`.
    Transcript(Value),
    /// Claude: where the transcript tailer is (persisted, not audited).
    TranscriptOffset {
        path: String,
        offset: u64,
    },
    /// Codex: a status computed by the driver's own engine. `turn_open` drives the turn
    /// records; `detail` is shown next to the status.
    Status {
        to: Status,
        cause: String,
        turn_open: bool,
        detail: Option<String>,
    },
    /// Codex: an approval request. `key` is what `answer_permission` gets back.
    PermissionRequested {
        key: String,
        tool: String,
        detail: String,
    },
    /// Codex: the thread id, used for `thread/resume`.
    NativeId(String),
    Boot(BootState),
    /// A step for the feed and audit trail (command finished, agent message, ...).
    Step(Value),
    Exited {
        code: Option<i32>,
    },
}

/// Channel end a driver emits into. Every event is tagged with the driver's session id so
/// late events from an ended session are ignored.
#[derive(Clone)]
pub struct Sink {
    session_id: String,
    tx: mpsc::Sender<(String, DriverEvent)>,
}

impl Sink {
    pub fn new(session_id: &str, tx: mpsc::Sender<(String, DriverEvent)>) -> Sink {
        Sink { session_id: session_id.to_string(), tx }
    }

    pub fn emit(&self, event: DriverEvent) {
        let _ = self.tx.send((self.session_id.clone(), event));
    }
}

/// A live view of the agent's terminal (Claude) or event stream (Codex).
pub struct Attachment {
    pub id: u64,
    /// Bytes that repaint the current screen (or recent stream) before live output.
    pub initial: Vec<u8>,
    pub output: mpsc::Receiver<Vec<u8>>,
    /// Whether typed bytes reach the agent (`Driver::input`).
    pub input: bool,
}

/// Fan-out of output bytes to attached clients; dead receivers are dropped on send.
#[derive(Default)]
pub struct Fanout {
    next: u64,
    subs: HashMap<u64, mpsc::Sender<Vec<u8>>>,
}

impl Fanout {
    pub fn subscribe(&mut self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        self.next += 1;
        let (tx, rx) = mpsc::channel();
        self.subs.insert(self.next, tx);
        (self.next, rx)
    }

    pub fn remove(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.subs.retain(|_, tx| tx.send(bytes.to_vec()).is_ok());
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }
}

pub trait Driver: Send + Sync {
    /// Delivers a message now: types it into the TUI (Claude) or steers / starts a turn
    /// (Codex). Returns once the message is handed to the driver's worker.
    fn send(&self, text: &str) -> Result<(), DriverError>;
    /// Esc (Claude) or `turn/interrupt` (Codex).
    fn interrupt(&self) -> Result<(), DriverError>;
    /// Answers the permission dialog / approval request named by `key` (Codex: the request
    /// id; Claude: the request detail, checked against the dialog on screen). Selects only
    /// a one-time "Yes" or "No", never an "always" or "auto" option.
    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError>;
    /// Answers Claude's workspace-trust dialog. `false` ends the session.
    fn trust(&self, accept: bool) -> Result<(), DriverError>;
    /// True while a dialog would capture typed text (a message must not be typed then).
    fn dialog_open(&self) -> bool;
    /// Claude: start (or switch) tailing the session transcript at `offset`.
    fn tail_transcript(&self, path: &Path, offset: u64);
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError>;
    fn input(&self, attachment: u64, bytes: &[u8]) -> Result<(), DriverError>;
    fn detach(&self, attachment: u64);
    /// Last lines of the screen (Claude) or stream (Codex), for the CLI.
    fn screen_tail(&self) -> String;
    /// Ends the agent gracefully, then kills it. Blocks until the process is gone; the
    /// driver still emits `Exited`.
    fn stop(&self);
}

pub struct SpawnRequest {
    pub task: Task,
    pub session_id: String,
    /// Native id to resume (Claude session id / Codex thread id).
    pub resume: Option<String>,
    pub paths: Paths,
}

pub trait DriverFactory: Send + Sync {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError>;
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): driver interface with typed unsupported results and a fake driver

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 6: Hub core: tasks, worktrees, status persistence, subscriptions

**Files:**
- Modify: `crates/mirante-daemon/src/lib.rs`
- Create: `crates/mirante-daemon/src/hub/mod.rs`

The hub owns all state behind one mutex: the `Store`, the live sessions, the last published event `seq`, and the subscribers. Every public method locks, mutates through the store (each mutation persists its audit event in the same transaction, from Plan 2a), then `publish()`es the new events. Clients reconcile from a snapshot (`subscribe` returns one, plus a channel of every later event); the stream is only a hint (spec §4).

- `create_task` makes the worktree first (git is slow, no lock held) on the user's branch name (no prefix, base = the repo's current branch unless given: Plan 2a's `WorktreeManager`), then the task; if the store fails, the worktree is removed again.
- `set_status` persists a status only when it changes and the move is valid, so a late event can never move `Ended` to `Working`.
- `archive` refuses a live session and a dirty worktree (git's own check); the branch is kept.
- `HubError` → `RpcError` mapping gives the CLI typed codes.

`Live` (one per live session) is defined here and used from Task 7 on, so this task builds with `dead_code` warnings; they disappear in Task 7.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/lib.rs`:
```rust
//! The Mirante daemon: the single owner of state. It keeps the `Store`, one live session per
//! task (agent process + status engine), serves the Unix socket, and turns hook events,
//! transcript lines and Codex notifications into persisted status, turns and permissions.
pub mod driver;
pub mod hub;
```

`crates/mirante-daemon/src/hub/mod.rs` (only the test module for now):
```rust
#[cfg(test)]
pub(crate) mod testkit {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;

    use mirante_core::model::AgentKind;
    use mirante_core::store::Store;
    use mirante_ipc::test_dir::TestDir;
    use mirante_ipc::types::{CreateTask, TaskView};
    use mirante_ipc::Paths;

    use super::{EventRx, Hub};
    use crate::driver::fake::FakeFactory;

    pub struct Kit {
        pub hub: Arc<Hub>,
        pub rx: EventRx,
        pub factory: Arc<FakeFactory>,
        pub repo: PathBuf,
        pub dir: TestDir,
    }

    /// A git repo whose only branch is `trunk`, with one commit.
    pub fn git_repo(root: &std::path::Path) -> PathBuf {
        let repo = root.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "trunk"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(repo.join("README"), "hi\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        repo
    }

    impl Kit {
        pub fn new() -> Kit {
            Kit::with_store(|_| Store::open_in_memory().unwrap())
        }

        pub fn with_store(open: impl FnOnce(&Paths) -> Store) -> Kit {
            let dir = TestDir::new();
            let paths = Paths::new(&dir.path().join("home"));
            paths.ensure_home().unwrap();
            let repo = git_repo(dir.path());
            let factory = Arc::new(FakeFactory::default());
            let (mut hub, rx) = Hub::new(open(&paths), paths, Box::new(factory.clone())).unwrap();
            hub.set_boot_timeout(std::time::Duration::ZERO);
            Kit { hub: Arc::new(hub), rx, factory, repo, dir }
        }

        pub fn create(&self, agent: AgentKind, branch: &str) -> TaskView {
            self.hub
                .create_task(CreateTask {
                    repo: self.repo.to_string_lossy().into_owned(),
                    agent,
                    branch: branch.into(),
                    base: None,
                    goal: format!("goal of {branch}"),
                    title: None,
                })
                .unwrap()
        }

        pub fn kinds(&self, task_id: &str) -> Vec<String> {
            self.hub.lock().store.task_events(task_id).unwrap().into_iter().map(|e| e.kind).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::Kit;
    use super::*;
    use mirante_ipc::types::CreateTask;

    #[test]
    fn create_makes_a_worktree_on_the_users_branch_and_a_task() {
        let k = Kit::new();
        let v = k.create(AgentKind::Claude, "feat/login");
        assert_eq!(v.task.branch, "feat/login");
        assert_eq!(v.task.title, "goal of feat/login");
        assert!(Path::new(&v.task.worktree_path).join("README").exists());
        assert!(v.task.worktree_path.ends_with("worktrees/app/feat/login"), "{}", v.task.worktree_path);
        assert!(!v.live);
        assert_eq!(v.task.status, Status::Idle);
    }

    #[test]
    fn create_rejects_an_empty_goal_and_rolls_back_the_worktree_on_failure() {
        let k = Kit::new();
        let bad = CreateTask {
            repo: k.repo.to_string_lossy().into_owned(),
            agent: AgentKind::Codex,
            branch: "b1".into(),
            base: None,
            goal: "  ".into(),
            title: None,
        };
        assert!(matches!(k.hub.create_task(bad), Err(HubError::Invalid(_))));
        let missing = CreateTask {
            repo: "/definitely/not/here".into(),
            agent: AgentKind::Codex,
            branch: "b1".into(),
            base: None,
            goal: "g".into(),
            title: None,
        };
        assert!(matches!(k.hub.create_task(missing), Err(HubError::Invalid(_))));
    }

    #[test]
    fn tasks_resolve_by_id_branch_or_unique_suffix() {
        let k = Kit::new();
        let a = k.create(AgentKind::Claude, "alpha").task;
        let b = k.create(AgentKind::Codex, "beta").task;
        assert_eq!(k.hub.get_task(&a.id).unwrap().task.id, a.id);
        assert_eq!(k.hub.get_task("beta").unwrap().task.id, b.id);
        assert_eq!(k.hub.get_task(&b.id[b.id.len() - 6..]).unwrap().task.id, b.id);
        assert!(matches!(k.hub.get_task("nope"), Err(HubError::NotFound(_))));
        assert!(matches!(k.hub.get_task(""), Err(HubError::Invalid(_))));
        assert_eq!(k.hub.list_tasks(false).unwrap().len(), 2);
    }

    #[test]
    fn archive_removes_the_worktree_keeps_the_branch_and_hides_the_task() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "gone").task;
        let v = k.hub.archive("gone").unwrap();
        assert_eq!(v.task.lifecycle, Lifecycle::Archived);
        assert!(!Path::new(&t.worktree_path).exists());
        let branches = std::process::Command::new("git").arg("-C").arg(&k.repo).args(["branch"]).output().unwrap();
        assert!(String::from_utf8_lossy(&branches.stdout).contains("gone"));
        assert!(k.hub.list_tasks(false).unwrap().is_empty());
        assert_eq!(k.hub.list_tasks(true).unwrap().len(), 1);
        assert!(matches!(k.hub.archive(&t.id), Err(HubError::Conflict(_))));
    }

    #[test]
    fn subscribers_get_every_event_after_the_snapshot() {
        let k = Kit::new();
        k.create(AgentKind::Claude, "one");
        let (snap, rx) = k.hub.subscribe().unwrap();
        assert_eq!(snap.tasks.len(), 1);
        k.create(AgentKind::Claude, "two");
        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(ev.kind, "task_created");
        assert_eq!(ev.seq, snap.seq + 1);
        assert_eq!(ev.boot_id, snap.boot_id);
    }

    #[test]
    fn title_is_the_first_goal_line_capped() {
        assert_eq!(title_from_goal("\n  fix the login bug \nmore"), "fix the login bug");
        let long = "x".repeat(80);
        let t = title_from_goal(&long);
        assert_eq!(t.chars().count(), 60);
        assert!(t.ends_with('…'));
        assert_eq!(title_from_goal(""), "untitled");
    }

    #[test]
    fn errors_map_to_typed_rpc_codes() {
        let e: RpcError = HubError::Core(CoreError::TaskBusy("t".into())).into();
        assert_eq!(e.code, code::CONFLICT);
        let e: RpcError = HubError::Driver(DriverError::Unsupported("trust")).into();
        assert_eq!(e.code, code::UNSUPPORTED);
        let e: RpcError = HubError::NeedsYou("dialog".into()).into();
        assert_eq!(e.code, code::NEEDS_YOU);
        let e: RpcError = HubError::Core(CoreError::NotFound { kind: "task", id: "x".into() }).into();
        assert_eq!(e.code, code::NOT_FOUND);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error[E0432]: unresolved imports `super::EventRx`, `super::Hub`
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/hub/mod.rs`:
```rust
//! The hub: all daemon state behind one mutex. Every public method locks, mutates through
//! the `Store` (which persists each transition with its audit event), then publishes the
//! new events to subscribers. Submodules add the session lifecycle (`session`) and
//! messaging / permissions (`messages`).
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

use mirante_core::model::{AgentKind, Event, Lifecycle, NewTask, Status, Task};
use mirante_core::status::StatusEngine;
use mirante_core::store::Store;
use mirante_core::worktree::WorktreeManager;
use mirante_core::CoreError;
use mirante_ipc::rpc::{code, RpcError};
use mirante_ipc::types::{BootState, CreateTask, DaemonStatus, Snapshot, TaskView};
use mirante_ipc::Paths;
use serde_json::Value;

use crate::driver::{Driver, DriverError, DriverEvent, DriverFactory};

pub type Result<T> = std::result::Result<T, HubError>;
pub type EventRx = mpsc::Receiver<(String, DriverEvent)>;

#[derive(Debug)]
pub enum HubError {
    Core(CoreError),
    Driver(DriverError),
    NotFound(String),
    Conflict(String),
    /// Refused because a dialog waits for the user.
    NeedsYou(String),
    Invalid(String),
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HubError::Core(e) => write!(f, "{e}"),
            HubError::Driver(e) => write!(f, "{e}"),
            HubError::NotFound(m) | HubError::Conflict(m) | HubError::NeedsYou(m) | HubError::Invalid(m) => {
                write!(f, "{m}")
            }
        }
    }
}

impl std::error::Error for HubError {}

impl From<CoreError> for HubError {
    fn from(e: CoreError) -> Self {
        HubError::Core(e)
    }
}

impl From<DriverError> for HubError {
    fn from(e: DriverError) -> Self {
        HubError::Driver(e)
    }
}

impl From<HubError> for RpcError {
    fn from(e: HubError) -> Self {
        let c = match &e {
            HubError::Core(CoreError::NotFound { .. }) | HubError::NotFound(_) => code::NOT_FOUND,
            HubError::Core(CoreError::InvalidBranch { .. }) | HubError::Invalid(_) => code::INVALID_PARAMS,
            HubError::Core(
                CoreError::TaskBusy(_)
                | CoreError::InvalidTransition { .. }
                | CoreError::StaleStatus { .. }
                | CoreError::SessionEnded(_)
                | CoreError::Git { .. }
                | CoreError::OutsideRoot { .. },
            )
            | HubError::Conflict(_)
            | HubError::Driver(DriverError::NoDialog(_)) => code::CONFLICT,
            HubError::Driver(DriverError::Unsupported(_)) => code::UNSUPPORTED,
            HubError::Driver(DriverError::Failed(_)) | HubError::Core(CoreError::ClaudeNotFound(_)) => code::DRIVER,
            HubError::NeedsYou(_) => code::NEEDS_YOU,
            HubError::Core(_) => code::INTERNAL,
        };
        RpcError::new(c, e.to_string())
    }
}

/// One live session: the agent process (through its driver) plus the state the hub keeps
/// about it. Removed when the session ends.
pub(crate) struct Live {
    pub(crate) session_id: String,
    pub(crate) agent: AgentKind,
    /// `None` only while the driver is being spawned.
    pub(crate) driver: Option<Arc<dyn Driver>>,
    /// Claude's fused status (hooks + transcript). Unused for Codex, whose driver runs its
    /// own engine and reports `DriverEvent::Status`.
    pub(crate) engine: StatusEngine,
    pub(crate) turn_id: Option<String>,
    pub(crate) native_id: Option<String>,
    pub(crate) transcript_path: Option<String>,
    pub(crate) boot: BootState,
    pub(crate) detail: Option<String>,
    /// Messages held until the running turn ends (front = next).
    pub(crate) queue: VecDeque<String>,
    /// Permission id -> driver key (Codex request id).
    pub(crate) perm_keys: HashMap<String, String>,
    /// Claude messages typed mid-turn, waiting for the transcript to say steered or queued.
    pub(crate) steers: Vec<String>,
    /// Recent compacted main-chain transcript lines (for steer/queue classification).
    pub(crate) transcript: Vec<Value>,
}

impl Live {
    pub(crate) fn new(session_id: &str, agent: AgentKind, native_id: Option<String>) -> Live {
        Live {
            session_id: session_id.to_string(),
            agent,
            driver: None,
            engine: StatusEngine::new(),
            turn_id: None,
            native_id,
            transcript_path: None,
            boot: BootState::Booting,
            detail: None,
            queue: VecDeque::new(),
            perm_keys: HashMap::new(),
            steers: Vec::new(),
            transcript: Vec::new(),
        }
    }
}

pub(crate) struct State {
    pub(crate) store: Store,
    pub(crate) live: HashMap<String, Live>,
    pub(crate) last_seq: u64,
    pub(crate) subscribers: Vec<mpsc::Sender<Event>>,
}

pub struct Hub {
    pub(crate) state: Mutex<State>,
    pub(crate) paths: Paths,
    pub(crate) worktrees: WorktreeManager,
    pub(crate) factory: Box<dyn DriverFactory>,
    pub(crate) events: mpsc::Sender<(String, DriverEvent)>,
    pub(crate) boot_timeout: Duration,
}

/// Highest event seq in the store (0 when empty).
fn last_seq(store: &Store) -> Result<u64> {
    let mut seq = 0;
    loop {
        let page = store.events_since(seq, 10_000)?;
        match page.last() {
            Some(e) => seq = e.seq,
            None => return Ok(seq),
        }
    }
}

/// First line of the goal, at most 60 characters.
pub fn title_from_goal(goal: &str) -> String {
    let line = goal.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("untitled");
    if line.chars().count() <= 60 {
        return line.to_string();
    }
    let cut: String = line.chars().take(59).collect();
    format!("{}…", cut.trim_end())
}

impl State {
    /// A task by full id, by branch (unique among non-archived tasks), or by a unique id
    /// suffix of at least 4 characters.
    pub(crate) fn resolve(&self, reference: &str) -> Result<Task> {
        let r = reference.trim();
        if r.is_empty() {
            return Err(HubError::Invalid("empty task reference".into()));
        }
        if let Ok(t) = self.store.get_task(r) {
            return Ok(t);
        }
        let tasks = self.store.list_tasks(true)?;
        let by_branch: Vec<&Task> =
            tasks.iter().filter(|t| t.lifecycle != Lifecycle::Archived && t.branch == r).collect();
        if by_branch.len() == 1 {
            return Ok(by_branch[0].clone());
        }
        if by_branch.len() > 1 {
            return Err(HubError::Conflict(format!("branch {r} names several tasks; use the task id")));
        }
        if r.len() >= 4 {
            let by_suffix: Vec<&Task> = tasks.iter().filter(|t| t.id.ends_with(r)).collect();
            match by_suffix.len() {
                1 => return Ok(by_suffix[0].clone()),
                0 => {}
                _ => return Err(HubError::Conflict(format!("id suffix {r} is ambiguous"))),
            }
        }
        Err(HubError::NotFound(format!("no task {r}")))
    }

    pub(crate) fn view(&self, task: Task) -> Result<TaskView> {
        let pending = self.store.pending_permissions(&task.id)?.len();
        let live = self.live.get(&task.id);
        let trust = usize::from(live.is_some_and(|l| l.boot == BootState::TrustPrompt));
        Ok(TaskView {
            live: live.is_some(),
            boot: live.map(|l| l.boot),
            needs_you: pending + trust,
            queued: live.map_or(0, |l| l.queue.len()),
            detail: live.and_then(|l| l.detail.clone()),
            task,
        })
    }

    pub(crate) fn views(&self, archived: bool) -> Result<Vec<TaskView>> {
        self.store.list_tasks(archived)?.into_iter().map(|t| self.view(t)).collect()
    }

    /// Persists `to` as the task status unless it already is, or the move is not allowed
    /// (e.g. `Ended -> Working` from a late event).
    pub(crate) fn set_status(&mut self, task_id: &str, to: Status, cause: &str) -> Result<()> {
        let cur = self.store.get_task(task_id)?.status;
        if cur == to || !cur.can_transition_to(to) {
            return Ok(());
        }
        self.store.set_status(task_id, cur, to, cause)?;
        Ok(())
    }

    pub(crate) fn event(&mut self, task_id: Option<&str>, kind: &str, payload: Value) -> Result<()> {
        self.store.append_event(task_id, kind, &payload)?;
        Ok(())
    }

    /// Sends every event newer than `last_seq` to the subscribers, dropping closed ones.
    pub(crate) fn publish(&mut self) {
        loop {
            let Ok(events) = self.store.events_since(self.last_seq, 1000) else {
                return;
            };
            let Some(last) = events.last() else {
                return;
            };
            self.last_seq = last.seq;
            self.subscribers.retain(|s| events.iter().all(|e| s.send(e.clone()).is_ok()));
        }
    }

    pub(crate) fn task_of_session(&self, session_id: &str) -> Option<String> {
        self.live.iter().find(|(_, l)| l.session_id == session_id).map(|(t, _)| t.clone())
    }
}

impl Hub {
    /// The hub and the receiving end of the driver-event channel (give it to
    /// [`Hub::spawn_pump`], or drain it with [`Hub::drain`] in tests).
    pub fn new(store: Store, paths: Paths, factory: Box<dyn DriverFactory>) -> Result<(Hub, EventRx)> {
        let (tx, rx) = mpsc::channel();
        let last_seq = last_seq(&store)?;
        let hub = Hub {
            state: Mutex::new(State { store, live: HashMap::new(), last_seq, subscribers: Vec::new() }),
            worktrees: WorktreeManager::new(&paths.worktrees()),
            paths,
            factory,
            events: tx,
            boot_timeout: Duration::from_secs(45),
        };
        Ok((hub, rx))
    }

    /// How long `session.start` / `session.trust` wait for the agent to leave `Booting`.
    pub fn set_boot_timeout(&mut self, timeout: Duration) {
        self.boot_timeout = timeout;
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn create_task(&self, p: CreateTask) -> Result<TaskView> {
        if p.goal.trim().is_empty() {
            return Err(HubError::Invalid("the goal is empty".into()));
        }
        let repo = std::fs::canonicalize(&p.repo).map_err(|e| HubError::Invalid(format!("repo {}: {e}", p.repo)))?;
        // git is slow: no lock held while the worktree is created.
        let wt = self.worktrees.create(&repo, &p.branch, p.base.as_deref())?;
        let title = p.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| title_from_goal(&p.goal));
        let new = NewTask {
            title,
            repo_path: repo.to_string_lossy().into_owned(),
            worktree_path: wt.path.to_string_lossy().into_owned(),
            branch: p.branch,
            agent: p.agent,
            goal: p.goal,
        };
        let mut st = self.lock();
        match st.store.create_task(new) {
            Ok(task) => {
                let v = st.view(task)?;
                st.publish();
                Ok(v)
            }
            Err(e) => {
                drop(st);
                let _ = self.worktrees.remove(&repo, &wt.path);
                Err(e.into())
            }
        }
    }

    pub fn list_tasks(&self, archived: bool) -> Result<Vec<TaskView>> {
        self.lock().views(archived)
    }

    pub fn get_task(&self, reference: &str) -> Result<TaskView> {
        let st = self.lock();
        let t = st.resolve(reference)?;
        st.view(t)
    }

    /// Removes the worktree (git refuses a dirty one) and archives the task. The branch is
    /// kept. Refused while a session is live.
    pub fn archive(&self, reference: &str) -> Result<TaskView> {
        let task = {
            let st = self.lock();
            let t = st.resolve(reference)?;
            if st.live.contains_key(&t.id) {
                return Err(HubError::Conflict(format!("task {} has a live session: stop it first", t.branch)));
            }
            if t.lifecycle == Lifecycle::Archived {
                return Err(HubError::Conflict(format!("task {} is already archived", t.branch)));
            }
            t
        };
        self.worktrees.remove(Path::new(&task.repo_path), Path::new(&task.worktree_path))?;
        let mut st = self.lock();
        let t = st.store.transition_lifecycle(&task.id, Lifecycle::Archived)?;
        let v = st.view(t)?;
        st.publish();
        Ok(v)
    }

    /// The current snapshot plus a channel of every event after it.
    pub fn subscribe(&self) -> Result<(Snapshot, mpsc::Receiver<Event>)> {
        let mut st = self.lock();
        st.publish();
        let (tx, rx) = mpsc::channel();
        st.subscribers.push(tx);
        let snapshot = Snapshot { boot_id: st.store.boot_id().to_string(), seq: st.last_seq, tasks: st.views(false)? };
        Ok((snapshot, rx))
    }

    pub fn status(&self) -> DaemonStatus {
        let st = self.lock();
        DaemonStatus {
            pid: std::process::id(),
            boot_id: st.store.boot_id().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            live_sessions: st.live.len(),
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Expected warnings (Task 7 starts using these items and builds with none):
```
warning: associated function `new` is never used
warning: fields `factory` and `events` are never read
warning: fields `rx`, `factory`, and `dir` are never read
warning: method `kinds` is never used
warning: methods `set_status`, `event`, and `task_of_session` are never used
warning: multiple fields are never read
```

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): hub with task CRUD over worktrees, validated status writes, snapshots

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 7: Hub sessions: start/resume, boot and trust, hooks, driver events, queue pump

**Files:**
- Modify: `crates/mirante-daemon/src/hub/mod.rs`
- Create: `crates/mirante-daemon/src/hub/session.rs`

Everything that moves a session:

- **Start / resume** (spec §2.6, FINDINGS S4). The per-task lock is the store's (`start_session` fails while a session is live). Resume reuses the latest conversation and passes its native id (Claude session id / Codex thread id) to the driver; a resume without a recorded native id starts fresh and says so (`resumed: false`). The spawn runs without the lock; a failed spawn ends the session at once, so no lock is ever orphaned (spec §8).
- **Boot and trust** (FINDINGS rule 5). `start_session` waits until the agent leaves `Booting` and returns the boot state plus the screen tail. A `TrustPrompt` is answered only through `trust`, i.e. by the user; the daemon never answers it on its own.
- **Hooks** (Claude). `SessionStart` records the native session id; the first `transcript_path` starts the driver's tailer at the end of an existing file (a resumed session appends to the same file; old turns must not replay into the fresh engine, Plan 2a risk note); `PermissionRequest` persists a permission card; every hook feeds the session's `StatusEngine` (Plan 2a, public API only: `apply`, `status`, `turn_open`, `Input::PermissionAnswered`).
- **Status side effects.** Turn records follow `turn_open()`. Leaving `NeedsYou` without Mirante answering means the user answered in the attached terminal or the agent moved on: a `PostToolUse` cause settles the pending card as allowed, anything else (turn end on the deny path, interrupt) as denied. Reaching `Idle` classifies Claude steers from the transcript (FINDINGS rule 2) and delivers the next queued message (`kick`), never into a dialog.
- **Exit.** The session ends, its pending cards expire (the dialog died with the process), undelivered queued messages are reported, status `Ended`.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/hub/mod.rs`:
```rust
//! The hub: all daemon state behind one mutex. Every public method locks, mutates through
//! the `Store` (which persists each transition with its audit event), then publishes the
//! new events to subscribers. Submodules add the session lifecycle (`session`) and
//! messaging / permissions (`messages`).
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

use mirante_core::model::{AgentKind, Event, Lifecycle, NewTask, Status, Task};
use mirante_core::status::StatusEngine;
use mirante_core::store::Store;
use mirante_core::worktree::WorktreeManager;
use mirante_core::CoreError;
use mirante_ipc::rpc::{code, RpcError};
use mirante_ipc::types::{BootState, CreateTask, DaemonStatus, Snapshot, TaskView};
use mirante_ipc::Paths;
use serde_json::Value;

use crate::driver::{Driver, DriverError, DriverEvent, DriverFactory};

pub mod session;

pub type Result<T> = std::result::Result<T, HubError>;
pub type EventRx = mpsc::Receiver<(String, DriverEvent)>;

#[derive(Debug)]
pub enum HubError {
    Core(CoreError),
    Driver(DriverError),
    NotFound(String),
    Conflict(String),
    /// Refused because a dialog waits for the user.
    NeedsYou(String),
    Invalid(String),
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HubError::Core(e) => write!(f, "{e}"),
            HubError::Driver(e) => write!(f, "{e}"),
            HubError::NotFound(m) | HubError::Conflict(m) | HubError::NeedsYou(m) | HubError::Invalid(m) => {
                write!(f, "{m}")
            }
        }
    }
}

impl std::error::Error for HubError {}

impl From<CoreError> for HubError {
    fn from(e: CoreError) -> Self {
        HubError::Core(e)
    }
}

impl From<DriverError> for HubError {
    fn from(e: DriverError) -> Self {
        HubError::Driver(e)
    }
}

impl From<HubError> for RpcError {
    fn from(e: HubError) -> Self {
        let c = match &e {
            HubError::Core(CoreError::NotFound { .. }) | HubError::NotFound(_) => code::NOT_FOUND,
            HubError::Core(CoreError::InvalidBranch { .. }) | HubError::Invalid(_) => code::INVALID_PARAMS,
            HubError::Core(
                CoreError::TaskBusy(_)
                | CoreError::InvalidTransition { .. }
                | CoreError::StaleStatus { .. }
                | CoreError::SessionEnded(_)
                | CoreError::Git { .. }
                | CoreError::OutsideRoot { .. },
            )
            | HubError::Conflict(_)
            | HubError::Driver(DriverError::NoDialog(_)) => code::CONFLICT,
            HubError::Driver(DriverError::Unsupported(_)) => code::UNSUPPORTED,
            HubError::Driver(DriverError::Failed(_)) | HubError::Core(CoreError::ClaudeNotFound(_)) => code::DRIVER,
            HubError::NeedsYou(_) => code::NEEDS_YOU,
            HubError::Core(_) => code::INTERNAL,
        };
        RpcError::new(c, e.to_string())
    }
}

/// One live session: the agent process (through its driver) plus the state the hub keeps
/// about it. Removed when the session ends.
pub(crate) struct Live {
    pub(crate) session_id: String,
    pub(crate) agent: AgentKind,
    /// `None` only while the driver is being spawned.
    pub(crate) driver: Option<Arc<dyn Driver>>,
    /// Claude's fused status (hooks + transcript). Unused for Codex, whose driver runs its
    /// own engine and reports `DriverEvent::Status`.
    pub(crate) engine: StatusEngine,
    pub(crate) turn_id: Option<String>,
    pub(crate) native_id: Option<String>,
    pub(crate) transcript_path: Option<String>,
    pub(crate) boot: BootState,
    pub(crate) detail: Option<String>,
    /// Messages held until the running turn ends (front = next).
    pub(crate) queue: VecDeque<String>,
    /// Permission id -> driver key (Codex request id).
    pub(crate) perm_keys: HashMap<String, String>,
    /// Claude messages typed mid-turn, waiting for the transcript to say steered or queued.
    pub(crate) steers: Vec<String>,
    /// Recent compacted main-chain transcript lines (for steer/queue classification).
    pub(crate) transcript: Vec<Value>,
}

impl Live {
    pub(crate) fn new(session_id: &str, agent: AgentKind, native_id: Option<String>) -> Live {
        Live {
            session_id: session_id.to_string(),
            agent,
            driver: None,
            engine: StatusEngine::new(),
            turn_id: None,
            native_id,
            transcript_path: None,
            boot: BootState::Booting,
            detail: None,
            queue: VecDeque::new(),
            perm_keys: HashMap::new(),
            steers: Vec::new(),
            transcript: Vec::new(),
        }
    }
}

pub(crate) struct State {
    pub(crate) store: Store,
    pub(crate) live: HashMap<String, Live>,
    pub(crate) last_seq: u64,
    pub(crate) subscribers: Vec<mpsc::Sender<Event>>,
}

pub struct Hub {
    pub(crate) state: Mutex<State>,
    pub(crate) paths: Paths,
    pub(crate) worktrees: WorktreeManager,
    pub(crate) factory: Box<dyn DriverFactory>,
    pub(crate) events: mpsc::Sender<(String, DriverEvent)>,
    pub(crate) boot_timeout: Duration,
}

/// Highest event seq in the store (0 when empty).
fn last_seq(store: &Store) -> Result<u64> {
    let mut seq = 0;
    loop {
        let page = store.events_since(seq, 10_000)?;
        match page.last() {
            Some(e) => seq = e.seq,
            None => return Ok(seq),
        }
    }
}

/// First line of the goal, at most 60 characters.
pub fn title_from_goal(goal: &str) -> String {
    let line = goal.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("untitled");
    if line.chars().count() <= 60 {
        return line.to_string();
    }
    let cut: String = line.chars().take(59).collect();
    format!("{}…", cut.trim_end())
}

impl State {
    /// A task by full id, by branch (unique among non-archived tasks), or by a unique id
    /// suffix of at least 4 characters.
    pub(crate) fn resolve(&self, reference: &str) -> Result<Task> {
        let r = reference.trim();
        if r.is_empty() {
            return Err(HubError::Invalid("empty task reference".into()));
        }
        if let Ok(t) = self.store.get_task(r) {
            return Ok(t);
        }
        let tasks = self.store.list_tasks(true)?;
        let by_branch: Vec<&Task> =
            tasks.iter().filter(|t| t.lifecycle != Lifecycle::Archived && t.branch == r).collect();
        if by_branch.len() == 1 {
            return Ok(by_branch[0].clone());
        }
        if by_branch.len() > 1 {
            return Err(HubError::Conflict(format!("branch {r} names several tasks; use the task id")));
        }
        if r.len() >= 4 {
            let by_suffix: Vec<&Task> = tasks.iter().filter(|t| t.id.ends_with(r)).collect();
            match by_suffix.len() {
                1 => return Ok(by_suffix[0].clone()),
                0 => {}
                _ => return Err(HubError::Conflict(format!("id suffix {r} is ambiguous"))),
            }
        }
        Err(HubError::NotFound(format!("no task {r}")))
    }

    pub(crate) fn view(&self, task: Task) -> Result<TaskView> {
        let pending = self.store.pending_permissions(&task.id)?.len();
        let live = self.live.get(&task.id);
        let trust = usize::from(live.is_some_and(|l| l.boot == BootState::TrustPrompt));
        Ok(TaskView {
            live: live.is_some(),
            boot: live.map(|l| l.boot),
            needs_you: pending + trust,
            queued: live.map_or(0, |l| l.queue.len()),
            detail: live.and_then(|l| l.detail.clone()),
            task,
        })
    }

    pub(crate) fn views(&self, archived: bool) -> Result<Vec<TaskView>> {
        self.store.list_tasks(archived)?.into_iter().map(|t| self.view(t)).collect()
    }

    /// Persists `to` as the task status unless it already is, or the move is not allowed
    /// (e.g. `Ended -> Working` from a late event).
    pub(crate) fn set_status(&mut self, task_id: &str, to: Status, cause: &str) -> Result<()> {
        let cur = self.store.get_task(task_id)?.status;
        if cur == to || !cur.can_transition_to(to) {
            return Ok(());
        }
        self.store.set_status(task_id, cur, to, cause)?;
        Ok(())
    }

    pub(crate) fn event(&mut self, task_id: Option<&str>, kind: &str, payload: Value) -> Result<()> {
        self.store.append_event(task_id, kind, &payload)?;
        Ok(())
    }

    /// Sends every event newer than `last_seq` to the subscribers, dropping closed ones.
    pub(crate) fn publish(&mut self) {
        loop {
            let Ok(events) = self.store.events_since(self.last_seq, 1000) else {
                return;
            };
            let Some(last) = events.last() else {
                return;
            };
            self.last_seq = last.seq;
            self.subscribers.retain(|s| events.iter().all(|e| s.send(e.clone()).is_ok()));
        }
    }

    pub(crate) fn task_of_session(&self, session_id: &str) -> Option<String> {
        self.live.iter().find(|(_, l)| l.session_id == session_id).map(|(t, _)| t.clone())
    }
}

impl Hub {
    /// The hub and the receiving end of the driver-event channel (give it to
    /// [`Hub::spawn_pump`], or drain it with [`Hub::drain`] in tests).
    pub fn new(store: Store, paths: Paths, factory: Box<dyn DriverFactory>) -> Result<(Hub, EventRx)> {
        let (tx, rx) = mpsc::channel();
        let last_seq = last_seq(&store)?;
        let hub = Hub {
            state: Mutex::new(State { store, live: HashMap::new(), last_seq, subscribers: Vec::new() }),
            worktrees: WorktreeManager::new(&paths.worktrees()),
            paths,
            factory,
            events: tx,
            boot_timeout: Duration::from_secs(45),
        };
        Ok((hub, rx))
    }

    /// How long `session.start` / `session.trust` wait for the agent to leave `Booting`.
    pub fn set_boot_timeout(&mut self, timeout: Duration) {
        self.boot_timeout = timeout;
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn create_task(&self, p: CreateTask) -> Result<TaskView> {
        if p.goal.trim().is_empty() {
            return Err(HubError::Invalid("the goal is empty".into()));
        }
        let repo = std::fs::canonicalize(&p.repo).map_err(|e| HubError::Invalid(format!("repo {}: {e}", p.repo)))?;
        // git is slow: no lock held while the worktree is created.
        let wt = self.worktrees.create(&repo, &p.branch, p.base.as_deref())?;
        let title = p.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| title_from_goal(&p.goal));
        let new = NewTask {
            title,
            repo_path: repo.to_string_lossy().into_owned(),
            worktree_path: wt.path.to_string_lossy().into_owned(),
            branch: p.branch,
            agent: p.agent,
            goal: p.goal,
        };
        let mut st = self.lock();
        match st.store.create_task(new) {
            Ok(task) => {
                let v = st.view(task)?;
                st.publish();
                Ok(v)
            }
            Err(e) => {
                drop(st);
                let _ = self.worktrees.remove(&repo, &wt.path);
                Err(e.into())
            }
        }
    }

    pub fn list_tasks(&self, archived: bool) -> Result<Vec<TaskView>> {
        self.lock().views(archived)
    }

    pub fn get_task(&self, reference: &str) -> Result<TaskView> {
        let st = self.lock();
        let t = st.resolve(reference)?;
        st.view(t)
    }

    /// Removes the worktree (git refuses a dirty one) and archives the task. The branch is
    /// kept. Refused while a session is live.
    pub fn archive(&self, reference: &str) -> Result<TaskView> {
        let task = {
            let st = self.lock();
            let t = st.resolve(reference)?;
            if st.live.contains_key(&t.id) {
                return Err(HubError::Conflict(format!("task {} has a live session: stop it first", t.branch)));
            }
            if t.lifecycle == Lifecycle::Archived {
                return Err(HubError::Conflict(format!("task {} is already archived", t.branch)));
            }
            t
        };
        self.worktrees.remove(Path::new(&task.repo_path), Path::new(&task.worktree_path))?;
        let mut st = self.lock();
        let t = st.store.transition_lifecycle(&task.id, Lifecycle::Archived)?;
        let v = st.view(t)?;
        st.publish();
        Ok(v)
    }

    /// The current snapshot plus a channel of every event after it.
    pub fn subscribe(&self) -> Result<(Snapshot, mpsc::Receiver<Event>)> {
        let mut st = self.lock();
        st.publish();
        let (tx, rx) = mpsc::channel();
        st.subscribers.push(tx);
        let snapshot = Snapshot { boot_id: st.store.boot_id().to_string(), seq: st.last_seq, tasks: st.views(false)? };
        Ok((snapshot, rx))
    }

    pub fn status(&self) -> DaemonStatus {
        let st = self.lock();
        DaemonStatus {
            pid: std::process::id(),
            boot_id: st.store.boot_id().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            live_sessions: st.live.len(),
        }
    }
}

#[cfg(test)]
pub(crate) mod testkit {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;

    use mirante_core::model::AgentKind;
    use mirante_core::store::Store;
    use mirante_ipc::test_dir::TestDir;
    use mirante_ipc::types::{CreateTask, TaskView};
    use mirante_ipc::Paths;

    use super::{EventRx, Hub};
    use crate::driver::fake::FakeFactory;

    pub struct Kit {
        pub hub: Arc<Hub>,
        pub rx: EventRx,
        pub factory: Arc<FakeFactory>,
        pub repo: PathBuf,
        pub dir: TestDir,
    }

    /// A git repo whose only branch is `trunk`, with one commit.
    pub fn git_repo(root: &std::path::Path) -> PathBuf {
        let repo = root.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "trunk"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(repo.join("README"), "hi\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        repo
    }

    impl Kit {
        pub fn new() -> Kit {
            Kit::with_store(|_| Store::open_in_memory().unwrap())
        }

        pub fn with_store(open: impl FnOnce(&Paths) -> Store) -> Kit {
            let dir = TestDir::new();
            let paths = Paths::new(&dir.path().join("home"));
            paths.ensure_home().unwrap();
            let repo = git_repo(dir.path());
            let factory = Arc::new(FakeFactory::default());
            let (mut hub, rx) = Hub::new(open(&paths), paths, Box::new(factory.clone())).unwrap();
            hub.set_boot_timeout(std::time::Duration::ZERO);
            Kit { hub: Arc::new(hub), rx, factory, repo, dir }
        }

        pub fn create(&self, agent: AgentKind, branch: &str) -> TaskView {
            self.hub
                .create_task(CreateTask {
                    repo: self.repo.to_string_lossy().into_owned(),
                    agent,
                    branch: branch.into(),
                    base: None,
                    goal: format!("goal of {branch}"),
                    title: None,
                })
                .unwrap()
        }

        pub fn kinds(&self, task_id: &str) -> Vec<String> {
            self.hub.lock().store.task_events(task_id).unwrap().into_iter().map(|e| e.kind).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::Kit;
    use super::*;
    use mirante_ipc::types::CreateTask;

    #[test]
    fn create_makes_a_worktree_on_the_users_branch_and_a_task() {
        let k = Kit::new();
        let v = k.create(AgentKind::Claude, "feat/login");
        assert_eq!(v.task.branch, "feat/login");
        assert_eq!(v.task.title, "goal of feat/login");
        assert!(Path::new(&v.task.worktree_path).join("README").exists());
        assert!(v.task.worktree_path.ends_with("worktrees/app/feat/login"), "{}", v.task.worktree_path);
        assert!(!v.live);
        assert_eq!(v.task.status, Status::Idle);
    }

    #[test]
    fn create_rejects_an_empty_goal_and_rolls_back_the_worktree_on_failure() {
        let k = Kit::new();
        let bad = CreateTask {
            repo: k.repo.to_string_lossy().into_owned(),
            agent: AgentKind::Codex,
            branch: "b1".into(),
            base: None,
            goal: "  ".into(),
            title: None,
        };
        assert!(matches!(k.hub.create_task(bad), Err(HubError::Invalid(_))));
        let missing = CreateTask {
            repo: "/definitely/not/here".into(),
            agent: AgentKind::Codex,
            branch: "b1".into(),
            base: None,
            goal: "g".into(),
            title: None,
        };
        assert!(matches!(k.hub.create_task(missing), Err(HubError::Invalid(_))));
    }

    #[test]
    fn tasks_resolve_by_id_branch_or_unique_suffix() {
        let k = Kit::new();
        let a = k.create(AgentKind::Claude, "alpha").task;
        let b = k.create(AgentKind::Codex, "beta").task;
        assert_eq!(k.hub.get_task(&a.id).unwrap().task.id, a.id);
        assert_eq!(k.hub.get_task("beta").unwrap().task.id, b.id);
        assert_eq!(k.hub.get_task(&b.id[b.id.len() - 6..]).unwrap().task.id, b.id);
        assert!(matches!(k.hub.get_task("nope"), Err(HubError::NotFound(_))));
        assert!(matches!(k.hub.get_task(""), Err(HubError::Invalid(_))));
        assert_eq!(k.hub.list_tasks(false).unwrap().len(), 2);
    }

    #[test]
    fn archive_removes_the_worktree_keeps_the_branch_and_hides_the_task() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "gone").task;
        let v = k.hub.archive("gone").unwrap();
        assert_eq!(v.task.lifecycle, Lifecycle::Archived);
        assert!(!Path::new(&t.worktree_path).exists());
        let branches = std::process::Command::new("git").arg("-C").arg(&k.repo).args(["branch"]).output().unwrap();
        assert!(String::from_utf8_lossy(&branches.stdout).contains("gone"));
        assert!(k.hub.list_tasks(false).unwrap().is_empty());
        assert_eq!(k.hub.list_tasks(true).unwrap().len(), 1);
        assert!(matches!(k.hub.archive(&t.id), Err(HubError::Conflict(_))));
    }

    #[test]
    fn subscribers_get_every_event_after_the_snapshot() {
        let k = Kit::new();
        k.create(AgentKind::Claude, "one");
        let (snap, rx) = k.hub.subscribe().unwrap();
        assert_eq!(snap.tasks.len(), 1);
        k.create(AgentKind::Claude, "two");
        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(ev.kind, "task_created");
        assert_eq!(ev.seq, snap.seq + 1);
        assert_eq!(ev.boot_id, snap.boot_id);
    }

    #[test]
    fn title_is_the_first_goal_line_capped() {
        assert_eq!(title_from_goal("\n  fix the login bug \nmore"), "fix the login bug");
        let long = "x".repeat(80);
        let t = title_from_goal(&long);
        assert_eq!(t.chars().count(), 60);
        assert!(t.ends_with('…'));
        assert_eq!(title_from_goal(""), "untitled");
    }

    #[test]
    fn errors_map_to_typed_rpc_codes() {
        let e: RpcError = HubError::Core(CoreError::TaskBusy("t".into())).into();
        assert_eq!(e.code, code::CONFLICT);
        let e: RpcError = HubError::Driver(DriverError::Unsupported("trust")).into();
        assert_eq!(e.code, code::UNSUPPORTED);
        let e: RpcError = HubError::NeedsYou("dialog".into()).into();
        assert_eq!(e.code, code::NEEDS_YOU);
        let e: RpcError = HubError::Core(CoreError::NotFound { kind: "task", id: "x".into() }).into();
        assert_eq!(e.code, code::NOT_FOUND);
    }
}
```

`crates/mirante-daemon/src/hub/session.rs` (only the test module for now):
```rust
#[cfg(test)]
impl super::testkit::Kit {
    /// Created, started and booted (`Ready`), with no prompt queued.
    pub fn started(&self, agent: AgentKind, branch: &str) -> TaskView {
        let t = self.create(agent, branch);
        self.hub.start_session(StartSession { task: t.task.id.clone(), prompt: None }, false).unwrap();
        self.emit(DriverEvent::Boot(BootState::Ready));
        self.hub.get_task(&t.task.id).unwrap()
    }

    pub fn drain(&self) {
        self.hub.drain(&self.rx);
    }

    pub fn emit(&self, event: DriverEvent) {
        self.factory.emit(event);
        self.drain();
    }

    pub fn hook(&self, task_id: &str, payload: Value) {
        self.hub
            .apply_hook(mirante_ipc::types::HookParams { task: task_id.into(), payload: payload.to_string(), ts_ms: 0 })
            .unwrap();
    }

    pub fn calls(&self) -> Vec<String> {
        self.factory.driver.calls()
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use mirante_core::model::PermissionStatus;
    use serde_json::json;

    fn status(k: &Kit, task: &str) -> Status {
        k.hub.get_task(task).unwrap().task.status
    }

    #[test]
    fn start_takes_the_per_task_lock_and_boot_ready_delivers_the_first_prompt() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: Some("do it".into()) }, false).unwrap();
        assert_eq!(r.boot, BootState::Booting);
        assert!(!r.resumed);
        let again = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false);
        assert!(matches!(again, Err(HubError::Conflict(_))));
        assert_eq!(k.hub.get_task(&t.id).unwrap().queued, 1);
        k.emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(k.calls(), vec!["send:do it".to_string()]);
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!((v.live, v.boot, v.queued, v.task.lifecycle), (true, Some(BootState::Ready), 0, Lifecycle::Active));
    }

    #[test]
    fn hooks_drive_status_turns_and_the_native_id() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "session_id": "nat-1", "source": "startup"}));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi"}));
        assert_eq!(status(&k, &t.id), Status::Working);
        k.hook(&t.id, json!({"hook_event_name": "Stop"}));
        assert_eq!(status(&k, &t.id), Status::Idle);
        let st = k.hub.lock();
        let conv = st.store.latest_conversation(&t.id).unwrap().unwrap();
        assert_eq!(conv.native_session_id.as_deref(), Some("nat-1"));
        let session = st.store.live_session(&t.id).unwrap().unwrap();
        let turns = st.store.turns(&session.id).unwrap();
        assert_eq!(turns.len(), 1);
        assert!(turns[0].ended_at.is_some());
    }

    #[test]
    fn hook_permission_request_is_persisted_and_settled_when_the_tool_runs() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "touch x"}));
        k.hook(
            &t.id,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": {"command": "touch x"}}),
        );
        assert_eq!(status(&k, &t.id), Status::NeedsYou);
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!(v.needs_you, 1);
        let pending = k.hub.lock().store.pending_permissions(&t.id).unwrap();
        assert_eq!((pending[0].tool.as_str(), pending[0].detail.as_str()), ("Bash", "touch x"));
        // The user answered "Yes" inside `mirante attach`: the tool runs.
        k.hook(&t.id, json!({"hook_event_name": "PostToolUse", "tool_name": "Bash"}));
        assert_eq!(status(&k, &t.id), Status::Working);
        assert!(k.hub.lock().store.pending_permissions(&t.id).unwrap().is_empty());
        assert!(k.kinds(&t.id).contains(&"permission_settled".to_string()));
    }

    #[test]
    fn deny_in_the_terminal_is_settled_by_the_transcript_turn_end() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "touch x"}));
        k.emit(DriverEvent::Transcript(
            json!({"type": "user", "origin": {"kind": "human"}, "message": {"content": "touch x"}}),
        ));
        k.hook(
            &t.id,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": {"command": "touch x"}}),
        );
        k.emit(DriverEvent::Transcript(json!({"type": "system", "subtype": "turn_duration"})));
        assert_eq!(status(&k, &t.id), Status::Idle);
        let ev = k.hub.lock().store.task_events(&t.id).unwrap();
        let settled = ev.iter().find(|e| e.kind == "permission_settled").unwrap();
        assert_eq!(settled.payload["answer"], json!(PermissionStatus::Denied));
    }

    #[test]
    fn transcript_path_starts_the_tailer_at_the_end_of_an_existing_file() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let path = k.dir.path().join("t.jsonl");
        std::fs::write(&path, "{\"old\":1}\n").unwrap();
        let p = path.to_string_lossy().into_owned();
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "source": "resume", "transcript_path": p}));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "transcript_path": p}));
        let tails: Vec<String> = k.calls().into_iter().filter(|c| c.starts_with("tail:")).collect();
        assert_eq!(tails, vec![format!("tail:{p}:10")]);
        k.emit(DriverEvent::TranscriptOffset { path: p.clone(), offset: 99 });
        let s = k.hub.lock().store.live_session(&t.id).unwrap().unwrap();
        assert_eq!((s.transcript_path.as_deref(), s.transcript_offset), (Some(p.as_str()), 99));
    }

    #[test]
    fn hooks_for_unknown_or_ended_sessions_are_logged_not_applied() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        assert_eq!(status(&k, &t.id), Status::Idle);
        assert!(k.kinds(&t.id).contains(&"hook_orphan".to_string()));
        let bad = k.hub.apply_hook(HookParams { task: t.id.clone(), payload: "nope".into(), ts_ms: 0 });
        assert!(matches!(bad, Err(HubError::Invalid(_))));
    }

    #[test]
    fn process_exit_ends_the_session_expires_permissions_and_allows_resume() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "session_id": "nat-9", "source": "startup"}));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        k.emit(DriverEvent::Exited { code: Some(0) });
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!((v.task.status, v.live, v.needs_you), (Status::Ended, false, 0));
        assert!(k.kinds(&t.id).contains(&"permission_expired".to_string()));
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, true).unwrap();
        assert!(r.resumed);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned().flatten().as_deref(), Some("nat-9"));
        assert_eq!(status(&k, &t.id), Status::Idle);
    }

    #[test]
    fn resume_without_a_native_id_starts_fresh_and_says_so() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.emit(DriverEvent::Exited { code: None });
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, true).unwrap();
        assert!(!r.resumed);
    }

    #[test]
    fn spawn_failure_releases_the_lock_and_ends_the_session() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "a").task;
        k.factory.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).is_err());
        assert_eq!(status(&k, &t.id), Status::Ended);
        k.factory.fail.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).is_ok());
    }

    #[test]
    fn trust_is_only_answered_when_the_prompt_is_open() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
        assert!(matches!(k.hub.trust(TrustAnswer { task: t.id.clone(), accept: true }), Err(HubError::Conflict(_))));
        k.emit(DriverEvent::Boot(BootState::TrustPrompt));
        assert_eq!(k.hub.get_task(&t.id).unwrap().needs_you, 1);
        let r = k.hub.trust(TrustAnswer { task: t.id.clone(), accept: true }).unwrap();
        assert_eq!(r.boot, BootState::Booting);
        assert_eq!(k.calls(), vec!["trust:true".to_string()]);
        assert!(k.kinds(&t.id).contains(&"trust_answered".to_string()));
    }

    #[test]
    fn stop_and_shutdown_end_sessions_through_the_driver() {
        let k = Kit::new();
        let a = k.started(AgentKind::Claude, "a").task;
        let b = k.started(AgentKind::Codex, "b").task;
        let v = k.hub.stop("a").unwrap();
        assert_eq!((v.task.status, v.live), (Status::Ended, false));
        assert!(matches!(k.hub.stop(&a.id), Err(HubError::Conflict(_))));
        k.hub.shutdown();
        assert_eq!(status(&k, &b.id), Status::Ended);
        assert_eq!(k.calls().iter().filter(|c| *c == "stop").count(), 2);
        assert_eq!(k.hub.status().live_sessions, 0);
    }

    #[test]
    fn codex_status_events_drive_status_turns_and_detail() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::NativeId("thr-1".into()));
        k.emit(DriverEvent::Status {
            to: Status::Working,
            cause: "codex:turn/started".into(),
            turn_open: true,
            detail: None,
        });
        let detail = Some("turn done, 1 command still finishing".to_string());
        k.emit(DriverEvent::Status {
            to: Status::Working,
            cause: "codex:turn/completed".into(),
            turn_open: false,
            detail: detail.clone(),
        });
        let v = k.hub.get_task(&t.id).unwrap();
        assert_eq!((v.task.status, v.detail), (Status::Working, detail));
        k.emit(DriverEvent::Status {
            to: Status::Idle,
            cause: "codex:item/completed".into(),
            turn_open: false,
            detail: None,
        });
        assert_eq!(status(&k, &t.id), Status::Idle);
        let st = k.hub.lock();
        assert_eq!(st.store.latest_conversation(&t.id).unwrap().unwrap().native_session_id.as_deref(), Some("thr-1"));
        let s = st.store.live_session(&t.id).unwrap().unwrap();
        assert_eq!(st.store.turns(&s.id).unwrap().len(), 1);
    }

    #[test]
    fn events_of_an_ended_session_are_ignored() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        let old = k.factory.last_sink();
        k.emit(DriverEvent::Exited { code: Some(1) });
        old.emit(DriverEvent::Status { to: Status::Working, cause: "late".into(), turn_open: true, detail: None });
        k.drain();
        assert_eq!(status(&k, &t.id), Status::Ended);
    }

    #[test]
    fn attach_goes_through_the_live_driver() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let (_d, a) = k.hub.attach(AttachParams { task: t.id.clone(), rows: 30, cols: 100 }).unwrap();
        assert_eq!(a.initial, b"screen");
        assert_eq!(a.output.recv().unwrap(), b"live");
        assert!(k.calls().contains(&"attach:30x100".to_string()));
    }

    #[test]
    fn compact_keeps_what_classification_reads() {
        assert!(compact(&json!({"type": "user", "isSidechain": true})).is_none());
        assert!(compact(&json!({"type": "progress"})).is_none());
        let a =
            compact(&json!({"type": "assistant", "message": {"stop_reason": "end_turn", "content": "big"}})).unwrap();
        assert_eq!(a, json!({"type": "assistant", "message": {"stop_reason": "end_turn"}}));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error[E0425]: cannot find type `AgentKind` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/hub/session.rs`:
```rust
//! Session lifecycle: start / resume (one live session per task), boot and trust, stop,
//! and every input that moves a session's status: hook events (Claude, via `hook.event`)
//! and driver events (transcript lines, Codex status, approvals, exit).
use std::sync::Arc;
use std::time::{Duration, Instant};

use mirante_core::delivery::{classify_message, Delivery};
use mirante_core::hook::HookEvent;
use mirante_core::model::{AgentKind, Lifecycle, PermissionStatus, Status};
use mirante_core::status::{is_sidechain, Input};
use mirante_ipc::types::{
    AttachParams, BootState, HookParams, SendMode, StartResult, StartSession, TaskView, TrustAnswer,
};
use serde_json::{json, Value};

use super::{EventRx, Hub, HubError, Live, Result, State};
use crate::driver::{Attachment, Driver, DriverEvent, Sink, SpawnRequest};

/// (task id, session id, driver) of a live session.
type LiveRef = (String, String, Option<Arc<dyn Driver>>);

/// Most compacted transcript lines kept per session for steer/queue classification.
const TRANSCRIPT_WINDOW: usize = 500;

/// The parts of a main-chain transcript line that steer/queue classification reads
/// (`mirante_core::delivery`): queue operations, prompts and turn ends. Assistant content is
/// dropped to bound memory. Sidechain lines are skipped.
pub(crate) fn compact(line: &Value) -> Option<Value> {
    if is_sidechain(line) {
        return None;
    }
    match line["type"].as_str()? {
        "queue-operation" | "user" | "system" => Some(line.clone()),
        "assistant" => {
            Some(json!({ "type": "assistant", "message": { "stop_reason": line["message"]["stop_reason"] } }))
        }
        _ => None,
    }
}

impl State {
    /// Delivers the next queued message when the session is ready and idle.
    pub(crate) fn kick(&mut self, task_id: &str) -> Result<()> {
        let status = self.store.get_task(task_id)?.status;
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        if l.boot != BootState::Ready || status != Status::Idle || l.queue.is_empty() {
            return Ok(());
        }
        let Some(driver) = l.driver.clone() else {
            return Ok(());
        };
        let text = l.queue.pop_front().expect("not empty");
        match driver.send(&text) {
            Ok(()) => self.event(Some(task_id), "message_sent", json!({ "mode": SendMode::Queue, "text": text })),
            Err(e) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    l.queue.push_front(text);
                }
                self.event(Some(task_id), "message_failed", json!({ "error": e.to_string() }))
            }
        }
    }

    /// For each Claude message typed mid-turn, records whether the transcript shows it
    /// steered (absorbed at a tool boundary) or queued (delivered after the turn).
    pub(crate) fn classify_steers(&mut self, task_id: &str) -> Result<()> {
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        let mut verdicts = Vec::new();
        let lines = &l.transcript;
        l.steers.retain(|text| match classify_message(lines, text) {
            Delivery::Pending => true,
            d => {
                verdicts.push((text.clone(), d));
                false
            }
        });
        for (text, delivery) in verdicts {
            self.event(Some(task_id), "message_delivery", json!({ "text": text, "delivery": delivery }))?;
        }
        Ok(())
    }

    /// Ends the live session `session_id` of `task_id` (no-op if it is not the live one):
    /// closes its turn and DB session, expires its pending permission requests (the dialog
    /// died with the process), reports undelivered queued messages, and sets `Ended`.
    pub(crate) fn finish_session(&mut self, task_id: &str, session_id: &str, cause: &str) -> Result<()> {
        if self.live.get(task_id).is_none_or(|l| l.session_id != session_id) {
            return Ok(());
        }
        let live = self.live.remove(task_id).expect("checked above");
        self.store.end_session(session_id)?;
        for p in self.store.pending_permissions(task_id)? {
            if p.session_id == session_id {
                self.store.answer_permission(&p.id, PermissionStatus::Denied)?;
                self.event(Some(task_id), "permission_expired", json!({ "permission_id": p.id, "cause": cause }))?;
            }
        }
        if !live.queue.is_empty() {
            self.event(Some(task_id), "queue_dropped", json!({ "messages": live.queue }))?;
        }
        self.set_status(task_id, Status::Ended, cause)
    }

    /// Feeds one input to a Claude session's `StatusEngine` and applies the result.
    pub(crate) fn feed_claude(&mut self, task_id: &str, input: &Input) -> Result<()> {
        let Some(live) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        let t = live.engine.apply(input);
        let turn_open = live.engine.turn_open();
        self.after_input(task_id, t.map(|t| (t.from, t.to, t.cause)), turn_open)
    }

    /// Persists a status change and the turn records, then runs what a change implies:
    /// leaving `NeedsYou` settles the pending requests the agent moved past; reaching
    /// `Idle` classifies Claude steers and delivers the next queued message.
    pub(crate) fn after_input(
        &mut self,
        task_id: &str,
        transition: Option<(Status, Status, String)>,
        turn_open: bool,
    ) -> Result<()> {
        if let Some((_, to, cause)) = &transition {
            self.set_status(task_id, *to, cause)?;
        }
        self.sync_turn(task_id, turn_open)?;
        if let Some((from, to, cause)) = transition {
            if from == Status::NeedsYou && to != Status::NeedsYou {
                self.settle_permissions(task_id, &cause)?;
            }
            if to == Status::Idle {
                self.classify_steers(task_id)?;
                self.kick(task_id)?;
            }
        }
        Ok(())
    }

    fn sync_turn(&mut self, task_id: &str, open: bool) -> Result<()> {
        let State { store, live, .. } = self;
        let Some(l) = live.get_mut(task_id) else {
            return Ok(());
        };
        match (open, &l.turn_id) {
            (true, None) => l.turn_id = Some(store.start_turn(&l.session_id)?.id),
            (false, Some(id)) => {
                store.end_turn(id)?;
                l.turn_id = None;
            }
            _ => {}
        }
        Ok(())
    }

    /// The status left `NeedsYou` without Mirante answering: the user answered in the
    /// attached terminal, or the agent moved on. A `PostToolUse` means the tool ran
    /// (allowed); anything else (turn end, interrupt) means it did not (denied).
    fn settle_permissions(&mut self, task_id: &str, cause: &str) -> Result<()> {
        let Some(session_id) = self.live.get(task_id).map(|l| l.session_id.clone()) else {
            return Ok(());
        };
        let answer = if cause == "hook:PostToolUse" { PermissionStatus::AllowedOnce } else { PermissionStatus::Denied };
        for p in self.store.pending_permissions(task_id)? {
            if p.session_id != session_id {
                continue;
            }
            self.store.answer_permission(&p.id, answer)?;
            if let Some(l) = self.live.get_mut(task_id) {
                l.perm_keys.remove(&p.id);
            }
            self.event(
                Some(task_id),
                "permission_settled",
                json!({ "permission_id": p.id, "answer": answer, "cause": cause }),
            )?;
        }
        Ok(())
    }

    fn handle(&mut self, task_id: &str, session_id: &str, event: DriverEvent) -> Result<()> {
        match event {
            DriverEvent::Transcript(line) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    if let Some(c) = compact(&line) {
                        l.transcript.push(c);
                        let excess = l.transcript.len().saturating_sub(TRANSCRIPT_WINDOW);
                        l.transcript.drain(..excess);
                    }
                }
                self.feed_claude(task_id, &Input::Transcript(line))
            }
            DriverEvent::TranscriptOffset { path, offset } => {
                Ok(self.store.set_transcript(session_id, &path, offset)?)
            }
            DriverEvent::Status { to, cause, turn_open, detail } => {
                let from = self.store.get_task(task_id)?.status;
                if let Some(l) = self.live.get_mut(task_id) {
                    l.detail = detail;
                }
                let t = (from != to).then_some((from, to, cause));
                self.after_input(task_id, t, turn_open)
            }
            DriverEvent::PermissionRequested { key, tool, detail } => {
                let p = self.store.create_permission_request(session_id, &tool, &detail)?;
                if let Some(l) = self.live.get_mut(task_id) {
                    l.perm_keys.insert(p.id, key);
                }
                Ok(())
            }
            DriverEvent::NativeId(id) => self.set_native_id(task_id, session_id, &id),
            DriverEvent::Boot(state) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    l.boot = state;
                }
                self.event(Some(task_id), "boot", json!({ "session_id": session_id, "state": state }))?;
                self.kick(task_id)
            }
            DriverEvent::Step(v) => self.event(Some(task_id), "step", v),
            DriverEvent::Exited { code } => {
                self.event(Some(task_id), "process_exited", json!({ "session_id": session_id, "code": code }))?;
                self.finish_session(task_id, session_id, "mirante:process_exited")
            }
        }
    }

    fn set_native_id(&mut self, task_id: &str, session_id: &str, native: &str) -> Result<()> {
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        if l.native_id.as_deref() == Some(native) {
            return Ok(());
        }
        l.native_id = Some(native.to_string());
        let conversation_id = self.store.get_session(session_id)?.conversation_id;
        Ok(self.store.set_native_session_id(&conversation_id, native)?)
    }
}

impl Hub {
    /// Starts (or, with `resume`, resumes) the task's agent. Fails with a conflict while a
    /// session is live (the per-task lock). Waits until the agent leaves `Booting`.
    pub fn start_session(&self, req: StartSession, resume: bool) -> Result<StartResult> {
        let (task, session_id, native) = {
            let mut st = self.lock();
            let task = st.resolve(&req.task)?;
            if task.lifecycle == Lifecycle::Archived {
                return Err(HubError::Conflict(format!("task {} is archived", task.branch)));
            }
            if st.live.contains_key(&task.id) {
                return Err(HubError::Conflict(format!("task {} already has a live session", task.branch)));
            }
            // Resume continues the latest conversation; a plain start opens a fresh one
            // unless the latest never got a native id (nothing to lose).
            let latest = st.store.latest_conversation(&task.id)?;
            let conversation = match latest {
                Some(c) if resume || c.native_session_id.is_none() => c,
                _ => st.store.create_conversation(&task.id)?,
            };
            let native = if resume { conversation.native_session_id.clone() } else { None };
            let session = st.store.start_session(&task.id, &conversation.id)?;
            st.set_status(&task.id, Status::Idle, "mirante:session_started")?;
            let mut live = Live::new(&session.id, task.agent, native.clone());
            if let Some(p) = req.prompt.filter(|p| !p.trim().is_empty()) {
                live.queue.push_back(p);
            }
            st.live.insert(task.id.clone(), live);
            st.event(
                Some(&task.id),
                "session_launch",
                json!({ "session_id": session.id, "resume": native, "resume_requested": resume }),
            )?;
            st.publish();
            (task, session.id, native)
        };
        let sink = Sink::new(&session_id, self.events.clone());
        let req = SpawnRequest {
            task: task.clone(),
            session_id: session_id.clone(),
            resume: native.clone(),
            paths: self.paths.clone(),
        };
        match self.factory.spawn(req, sink) {
            Ok(driver) => {
                let mut st = self.lock();
                if let Some(l) = st.live.get_mut(&task.id).filter(|l| l.session_id == session_id) {
                    l.driver = Some(driver);
                }
                st.kick(&task.id)?;
                st.publish();
            }
            Err(e) => {
                let mut st = self.lock();
                st.finish_session(&task.id, &session_id, "mirante:spawn_failed")?;
                st.publish();
                return Err(e.into());
            }
        }
        self.wait_boot(&task.id, &session_id, native.is_some())
    }

    /// Polls (without holding the lock) until the session leaves `Booting`, ends, or the
    /// boot timeout passes.
    fn wait_boot(&self, task_id: &str, session_id: &str, resumed: bool) -> Result<StartResult> {
        let deadline = Instant::now() + self.boot_timeout;
        loop {
            let (boot, driver) = {
                let st = self.lock();
                match st.live.get(task_id).filter(|l| l.session_id == session_id) {
                    Some(l) => (l.boot, l.driver.clone()),
                    None => (BootState::Exited, None),
                }
            };
            if boot != BootState::Booting || Instant::now() >= deadline {
                let screen = driver.map(|d| d.screen_tail()).unwrap_or_default();
                return Ok(StartResult { task: task_id.to_string(), boot, resumed, screen });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The user's answer to Claude's workspace-trust dialog (never answered by Mirante on
    /// its own). Accept: boot continues. Decline: the session ends.
    pub fn trust(&self, answer: TrustAnswer) -> Result<StartResult> {
        let (task_id, session_id, driver, resumed) = {
            let mut st = self.lock();
            let task = st.resolve(&answer.task)?;
            let l = st.live.get_mut(&task.id).ok_or_else(|| HubError::Conflict("no live session".into()))?;
            if l.boot != BootState::TrustPrompt {
                return Err(HubError::Conflict("no trust prompt is open".into()));
            }
            let driver = l.driver.clone().ok_or_else(|| HubError::Conflict("agent still starting".into()))?;
            // Before the driver acts: its watcher may report Ready right after.
            l.boot = BootState::Booting;
            let out = (task.id.clone(), l.session_id.clone(), driver, l.native_id.is_some());
            st.event(Some(&task.id), "trust_answered", json!({ "accept": answer.accept }))?;
            st.publish();
            out
        };
        if let Err(e) = driver.trust(answer.accept) {
            let mut st = self.lock();
            if let Some(l) = st.live.get_mut(&task_id) {
                l.boot = BootState::TrustPrompt;
            }
            return Err(e.into());
        }
        self.wait_boot(&task_id, &session_id, resumed)
    }

    /// Ends the live session (graceful, then kill). The conversation stays resumable.
    pub fn stop(&self, reference: &str) -> Result<TaskView> {
        let (task_id, session_id, driver) = self.live_driver(reference)?;
        if let Some(d) = driver {
            d.stop();
        }
        let mut st = self.lock();
        st.finish_session(&task_id, &session_id, "mirante:stopped")?;
        st.publish();
        let t = st.store.get_task(&task_id)?;
        st.view(t)
    }

    pub fn interrupt(&self, reference: &str) -> Result<()> {
        let (task_id, _, driver) = self.live_driver(reference)?;
        driver.ok_or_else(|| HubError::Conflict("agent still starting".into()))?.interrupt()?;
        let mut st = self.lock();
        st.event(Some(&task_id), "interrupt_sent", json!({}))?;
        st.publish();
        Ok(())
    }

    pub fn attach(&self, p: AttachParams) -> Result<(Arc<dyn Driver>, Attachment)> {
        let (task_id, _, driver) = self.live_driver(&p.task)?;
        let driver = driver.ok_or_else(|| HubError::Conflict("agent still starting".into()))?;
        let a = driver.attach(p.rows.max(10), p.cols.max(40))?;
        let mut st = self.lock();
        st.event(Some(&task_id), "attached", json!({ "attachment": a.id }))?;
        st.publish();
        Ok((driver, a))
    }

    fn live_driver(&self, reference: &str) -> Result<LiveRef> {
        let st = self.lock();
        let task = st.resolve(reference)?;
        let l = st
            .live
            .get(&task.id)
            .ok_or_else(|| HubError::Conflict(format!("task {} has no live session", task.branch)))?;
        Ok((task.id.clone(), l.session_id.clone(), l.driver.clone()))
    }

    /// Stops every live session (daemon shutdown).
    pub fn shutdown(&self) {
        let live: Vec<LiveRef> = {
            let st = self.lock();
            st.live.iter().map(|(t, l)| (t.clone(), l.session_id.clone(), l.driver.clone())).collect()
        };
        for (_, _, d) in &live {
            if let Some(d) = d {
                d.stop();
            }
        }
        let mut st = self.lock();
        for (task_id, session_id, _) in live {
            let _ = st.finish_session(&task_id, &session_id, "mirante:daemon_shutdown");
        }
        st.publish();
    }

    /// One Claude hook payload (from `mirante-hook`). Records the native session id and the
    /// transcript path (starting the tailer), persists permission requests, and feeds the
    /// status engine. A payload for a task without a live Claude session is only logged.
    pub fn apply_hook(&self, p: HookParams) -> Result<()> {
        let mut st = self.lock();
        let ev = match HookEvent::parse(&p.payload) {
            Ok(ev) => ev,
            Err(e) => {
                st.event(Some(&p.task), "hook_unparseable", json!({ "error": e.to_string() }))?;
                st.publish();
                return Err(HubError::Invalid(format!("hook payload: {e}")));
            }
        };
        let live = st.live.get(&p.task).filter(|l| l.agent == AgentKind::Claude);
        let Some(live) = live else {
            st.event(Some(&p.task), "hook_orphan", json!({ "hook": ev.hook_event_name }))?;
            st.publish();
            return Ok(());
        };
        let session_id = live.session_id.clone();
        if ev.hook_event_name == "SessionStart" {
            if let Some(sid) = &ev.session_id {
                st.set_native_id(&p.task, &session_id, sid)?;
            }
        }
        if let Some(path) = ev.transcript_path.clone() {
            st.follow_transcript(&p.task, &session_id, &path)?;
        }
        if ev.hook_event_name == "PermissionRequest" {
            let tool = ev.tool_name.clone().unwrap_or_else(|| "unknown".into());
            st.store.create_permission_request(&session_id, &tool, &ev.permission_detail())?;
        }
        st.feed_claude(&p.task, &Input::Hook(ev))?;
        st.publish();
        Ok(())
    }

    /// Applies one driver event. Events of a session that is no longer live are ignored.
    pub fn handle_driver_event(&self, session_id: &str, event: DriverEvent) {
        let mut st = self.lock();
        let Some(task_id) = st.task_of_session(session_id) else {
            return;
        };
        if let Err(e) = st.handle(&task_id, session_id, event) {
            eprintln!("mirante-daemon: task {task_id}: {e}");
            let _ = st.event(Some(&task_id), "daemon_error", json!({ "error": e.to_string() }));
        }
        st.publish();
    }

    /// Applies every queued driver event now (tests; the daemon uses the pump thread).
    pub fn drain(&self, rx: &EventRx) {
        while let Ok((session_id, event)) = rx.try_recv() {
            self.handle_driver_event(&session_id, event);
        }
    }

    pub fn spawn_pump(self: &Arc<Self>, rx: EventRx) -> std::thread::JoinHandle<()> {
        let hub = Arc::clone(self);
        std::thread::spawn(move || {
            for (session_id, event) in rx {
                hub.handle_driver_event(&session_id, event);
            }
        })
    }
}

impl State {
    /// Starts tailing a transcript the first time a hook names it. A file that already
    /// exists (a resumed session appends to the same file) is tailed from its end, so old
    /// turns are not replayed into the fresh engine; the persisted offset wins when the
    /// session already tailed this path.
    fn follow_transcript(&mut self, task_id: &str, session_id: &str, path: &str) -> Result<()> {
        let Some(l) = self.live.get_mut(task_id) else {
            return Ok(());
        };
        if l.transcript_path.as_deref() == Some(path) {
            return Ok(());
        }
        l.transcript_path = Some(path.to_string());
        let driver = l.driver.clone();
        let session = self.store.get_session(session_id)?;
        let offset = if session.transcript_path.as_deref() == Some(path) {
            session.transcript_offset
        } else {
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        };
        self.store.set_transcript(session_id, path, offset)?;
        if let Some(d) = driver {
            d.tail_transcript(std::path::Path::new(path), offset);
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): session lifecycle, hook fusion, permission settling and queue pump

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 8: Hub messaging and permission answers

**Files:**
- Modify: `crates/mirante-daemon/src/hub/mod.rs`
- Create: `crates/mirante-daemon/src/hub/messages.rs`

Spec §2.4–2.5.

- **Steer** hands the message to the agent now (Claude: typed, absorbed at the next tool boundary; Codex: `turn/steer`). It is refused with `NEEDS_YOU` while a dialog is open: typed text would answer a permission dialog, which would be an approval nobody gave.
- **Queue** holds the message in the daemon until the running turn ends; on an idle agent with an empty queue it goes out at once. Before the agent is ready, both modes queue.
- **Answers** are persisted only after the driver acted (Claude types the exact one-time option for the dialog that matches the request; Codex answers the JSON-RPC request), so a failed answer leaves the card pending. Claude shows one dialog at a time, so its cards are answered oldest first.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/hub/mod.rs`:
```rust
//! The hub: all daemon state behind one mutex. Every public method locks, mutates through
//! the `Store` (which persists each transition with its audit event), then publishes the
//! new events to subscribers. Submodules add the session lifecycle (`session`) and
//! messaging / permissions (`messages`).
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

use mirante_core::model::{AgentKind, Event, Lifecycle, NewTask, Status, Task};
use mirante_core::status::StatusEngine;
use mirante_core::store::Store;
use mirante_core::worktree::WorktreeManager;
use mirante_core::CoreError;
use mirante_ipc::rpc::{code, RpcError};
use mirante_ipc::types::{BootState, CreateTask, DaemonStatus, Snapshot, TaskView};
use mirante_ipc::Paths;
use serde_json::Value;

use crate::driver::{Driver, DriverError, DriverEvent, DriverFactory};

pub mod messages;
pub mod session;

pub type Result<T> = std::result::Result<T, HubError>;
pub type EventRx = mpsc::Receiver<(String, DriverEvent)>;

#[derive(Debug)]
pub enum HubError {
    Core(CoreError),
    Driver(DriverError),
    NotFound(String),
    Conflict(String),
    /// Refused because a dialog waits for the user.
    NeedsYou(String),
    Invalid(String),
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HubError::Core(e) => write!(f, "{e}"),
            HubError::Driver(e) => write!(f, "{e}"),
            HubError::NotFound(m) | HubError::Conflict(m) | HubError::NeedsYou(m) | HubError::Invalid(m) => {
                write!(f, "{m}")
            }
        }
    }
}

impl std::error::Error for HubError {}

impl From<CoreError> for HubError {
    fn from(e: CoreError) -> Self {
        HubError::Core(e)
    }
}

impl From<DriverError> for HubError {
    fn from(e: DriverError) -> Self {
        HubError::Driver(e)
    }
}

impl From<HubError> for RpcError {
    fn from(e: HubError) -> Self {
        let c = match &e {
            HubError::Core(CoreError::NotFound { .. }) | HubError::NotFound(_) => code::NOT_FOUND,
            HubError::Core(CoreError::InvalidBranch { .. }) | HubError::Invalid(_) => code::INVALID_PARAMS,
            HubError::Core(
                CoreError::TaskBusy(_)
                | CoreError::InvalidTransition { .. }
                | CoreError::StaleStatus { .. }
                | CoreError::SessionEnded(_)
                | CoreError::Git { .. }
                | CoreError::OutsideRoot { .. },
            )
            | HubError::Conflict(_)
            | HubError::Driver(DriverError::NoDialog(_)) => code::CONFLICT,
            HubError::Driver(DriverError::Unsupported(_)) => code::UNSUPPORTED,
            HubError::Driver(DriverError::Failed(_)) | HubError::Core(CoreError::ClaudeNotFound(_)) => code::DRIVER,
            HubError::NeedsYou(_) => code::NEEDS_YOU,
            HubError::Core(_) => code::INTERNAL,
        };
        RpcError::new(c, e.to_string())
    }
}

/// One live session: the agent process (through its driver) plus the state the hub keeps
/// about it. Removed when the session ends.
pub(crate) struct Live {
    pub(crate) session_id: String,
    pub(crate) agent: AgentKind,
    /// `None` only while the driver is being spawned.
    pub(crate) driver: Option<Arc<dyn Driver>>,
    /// Claude's fused status (hooks + transcript). Unused for Codex, whose driver runs its
    /// own engine and reports `DriverEvent::Status`.
    pub(crate) engine: StatusEngine,
    pub(crate) turn_id: Option<String>,
    pub(crate) native_id: Option<String>,
    pub(crate) transcript_path: Option<String>,
    pub(crate) boot: BootState,
    pub(crate) detail: Option<String>,
    /// Messages held until the running turn ends (front = next).
    pub(crate) queue: VecDeque<String>,
    /// Permission id -> driver key (Codex request id).
    pub(crate) perm_keys: HashMap<String, String>,
    /// Claude messages typed mid-turn, waiting for the transcript to say steered or queued.
    pub(crate) steers: Vec<String>,
    /// Recent compacted main-chain transcript lines (for steer/queue classification).
    pub(crate) transcript: Vec<Value>,
}

impl Live {
    pub(crate) fn new(session_id: &str, agent: AgentKind, native_id: Option<String>) -> Live {
        Live {
            session_id: session_id.to_string(),
            agent,
            driver: None,
            engine: StatusEngine::new(),
            turn_id: None,
            native_id,
            transcript_path: None,
            boot: BootState::Booting,
            detail: None,
            queue: VecDeque::new(),
            perm_keys: HashMap::new(),
            steers: Vec::new(),
            transcript: Vec::new(),
        }
    }
}

pub(crate) struct State {
    pub(crate) store: Store,
    pub(crate) live: HashMap<String, Live>,
    pub(crate) last_seq: u64,
    pub(crate) subscribers: Vec<mpsc::Sender<Event>>,
}

pub struct Hub {
    pub(crate) state: Mutex<State>,
    pub(crate) paths: Paths,
    pub(crate) worktrees: WorktreeManager,
    pub(crate) factory: Box<dyn DriverFactory>,
    pub(crate) events: mpsc::Sender<(String, DriverEvent)>,
    pub(crate) boot_timeout: Duration,
}

/// Highest event seq in the store (0 when empty).
fn last_seq(store: &Store) -> Result<u64> {
    let mut seq = 0;
    loop {
        let page = store.events_since(seq, 10_000)?;
        match page.last() {
            Some(e) => seq = e.seq,
            None => return Ok(seq),
        }
    }
}

/// First line of the goal, at most 60 characters.
pub fn title_from_goal(goal: &str) -> String {
    let line = goal.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("untitled");
    if line.chars().count() <= 60 {
        return line.to_string();
    }
    let cut: String = line.chars().take(59).collect();
    format!("{}…", cut.trim_end())
}

impl State {
    /// A task by full id, by branch (unique among non-archived tasks), or by a unique id
    /// suffix of at least 4 characters.
    pub(crate) fn resolve(&self, reference: &str) -> Result<Task> {
        let r = reference.trim();
        if r.is_empty() {
            return Err(HubError::Invalid("empty task reference".into()));
        }
        if let Ok(t) = self.store.get_task(r) {
            return Ok(t);
        }
        let tasks = self.store.list_tasks(true)?;
        let by_branch: Vec<&Task> =
            tasks.iter().filter(|t| t.lifecycle != Lifecycle::Archived && t.branch == r).collect();
        if by_branch.len() == 1 {
            return Ok(by_branch[0].clone());
        }
        if by_branch.len() > 1 {
            return Err(HubError::Conflict(format!("branch {r} names several tasks; use the task id")));
        }
        if r.len() >= 4 {
            let by_suffix: Vec<&Task> = tasks.iter().filter(|t| t.id.ends_with(r)).collect();
            match by_suffix.len() {
                1 => return Ok(by_suffix[0].clone()),
                0 => {}
                _ => return Err(HubError::Conflict(format!("id suffix {r} is ambiguous"))),
            }
        }
        Err(HubError::NotFound(format!("no task {r}")))
    }

    pub(crate) fn view(&self, task: Task) -> Result<TaskView> {
        let pending = self.store.pending_permissions(&task.id)?.len();
        let live = self.live.get(&task.id);
        let trust = usize::from(live.is_some_and(|l| l.boot == BootState::TrustPrompt));
        Ok(TaskView {
            live: live.is_some(),
            boot: live.map(|l| l.boot),
            needs_you: pending + trust,
            queued: live.map_or(0, |l| l.queue.len()),
            detail: live.and_then(|l| l.detail.clone()),
            task,
        })
    }

    pub(crate) fn views(&self, archived: bool) -> Result<Vec<TaskView>> {
        self.store.list_tasks(archived)?.into_iter().map(|t| self.view(t)).collect()
    }

    /// Persists `to` as the task status unless it already is, or the move is not allowed
    /// (e.g. `Ended -> Working` from a late event).
    pub(crate) fn set_status(&mut self, task_id: &str, to: Status, cause: &str) -> Result<()> {
        let cur = self.store.get_task(task_id)?.status;
        if cur == to || !cur.can_transition_to(to) {
            return Ok(());
        }
        self.store.set_status(task_id, cur, to, cause)?;
        Ok(())
    }

    pub(crate) fn event(&mut self, task_id: Option<&str>, kind: &str, payload: Value) -> Result<()> {
        self.store.append_event(task_id, kind, &payload)?;
        Ok(())
    }

    /// Sends every event newer than `last_seq` to the subscribers, dropping closed ones.
    pub(crate) fn publish(&mut self) {
        loop {
            let Ok(events) = self.store.events_since(self.last_seq, 1000) else {
                return;
            };
            let Some(last) = events.last() else {
                return;
            };
            self.last_seq = last.seq;
            self.subscribers.retain(|s| events.iter().all(|e| s.send(e.clone()).is_ok()));
        }
    }

    pub(crate) fn task_of_session(&self, session_id: &str) -> Option<String> {
        self.live.iter().find(|(_, l)| l.session_id == session_id).map(|(t, _)| t.clone())
    }
}

impl Hub {
    /// The hub and the receiving end of the driver-event channel (give it to
    /// [`Hub::spawn_pump`], or drain it with [`Hub::drain`] in tests).
    pub fn new(store: Store, paths: Paths, factory: Box<dyn DriverFactory>) -> Result<(Hub, EventRx)> {
        let (tx, rx) = mpsc::channel();
        let last_seq = last_seq(&store)?;
        let hub = Hub {
            state: Mutex::new(State { store, live: HashMap::new(), last_seq, subscribers: Vec::new() }),
            worktrees: WorktreeManager::new(&paths.worktrees()),
            paths,
            factory,
            events: tx,
            boot_timeout: Duration::from_secs(45),
        };
        Ok((hub, rx))
    }

    /// How long `session.start` / `session.trust` wait for the agent to leave `Booting`.
    pub fn set_boot_timeout(&mut self, timeout: Duration) {
        self.boot_timeout = timeout;
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn create_task(&self, p: CreateTask) -> Result<TaskView> {
        if p.goal.trim().is_empty() {
            return Err(HubError::Invalid("the goal is empty".into()));
        }
        let repo = std::fs::canonicalize(&p.repo).map_err(|e| HubError::Invalid(format!("repo {}: {e}", p.repo)))?;
        // git is slow: no lock held while the worktree is created.
        let wt = self.worktrees.create(&repo, &p.branch, p.base.as_deref())?;
        let title = p.title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| title_from_goal(&p.goal));
        let new = NewTask {
            title,
            repo_path: repo.to_string_lossy().into_owned(),
            worktree_path: wt.path.to_string_lossy().into_owned(),
            branch: p.branch,
            agent: p.agent,
            goal: p.goal,
        };
        let mut st = self.lock();
        match st.store.create_task(new) {
            Ok(task) => {
                let v = st.view(task)?;
                st.publish();
                Ok(v)
            }
            Err(e) => {
                drop(st);
                let _ = self.worktrees.remove(&repo, &wt.path);
                Err(e.into())
            }
        }
    }

    pub fn list_tasks(&self, archived: bool) -> Result<Vec<TaskView>> {
        self.lock().views(archived)
    }

    pub fn get_task(&self, reference: &str) -> Result<TaskView> {
        let st = self.lock();
        let t = st.resolve(reference)?;
        st.view(t)
    }

    /// Removes the worktree (git refuses a dirty one) and archives the task. The branch is
    /// kept. Refused while a session is live.
    pub fn archive(&self, reference: &str) -> Result<TaskView> {
        let task = {
            let st = self.lock();
            let t = st.resolve(reference)?;
            if st.live.contains_key(&t.id) {
                return Err(HubError::Conflict(format!("task {} has a live session: stop it first", t.branch)));
            }
            if t.lifecycle == Lifecycle::Archived {
                return Err(HubError::Conflict(format!("task {} is already archived", t.branch)));
            }
            t
        };
        self.worktrees.remove(Path::new(&task.repo_path), Path::new(&task.worktree_path))?;
        let mut st = self.lock();
        let t = st.store.transition_lifecycle(&task.id, Lifecycle::Archived)?;
        let v = st.view(t)?;
        st.publish();
        Ok(v)
    }

    /// The current snapshot plus a channel of every event after it.
    pub fn subscribe(&self) -> Result<(Snapshot, mpsc::Receiver<Event>)> {
        let mut st = self.lock();
        st.publish();
        let (tx, rx) = mpsc::channel();
        st.subscribers.push(tx);
        let snapshot = Snapshot { boot_id: st.store.boot_id().to_string(), seq: st.last_seq, tasks: st.views(false)? };
        Ok((snapshot, rx))
    }

    pub fn status(&self) -> DaemonStatus {
        let st = self.lock();
        DaemonStatus {
            pid: std::process::id(),
            boot_id: st.store.boot_id().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            live_sessions: st.live.len(),
        }
    }
}

#[cfg(test)]
pub(crate) mod testkit {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;

    use mirante_core::model::AgentKind;
    use mirante_core::store::Store;
    use mirante_ipc::test_dir::TestDir;
    use mirante_ipc::types::{CreateTask, TaskView};
    use mirante_ipc::Paths;

    use super::{EventRx, Hub};
    use crate::driver::fake::FakeFactory;

    pub struct Kit {
        pub hub: Arc<Hub>,
        pub rx: EventRx,
        pub factory: Arc<FakeFactory>,
        pub repo: PathBuf,
        pub dir: TestDir,
    }

    /// A git repo whose only branch is `trunk`, with one commit.
    pub fn git_repo(root: &std::path::Path) -> PathBuf {
        let repo = root.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "trunk"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(repo.join("README"), "hi\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        repo
    }

    impl Kit {
        pub fn new() -> Kit {
            Kit::with_store(|_| Store::open_in_memory().unwrap())
        }

        pub fn with_store(open: impl FnOnce(&Paths) -> Store) -> Kit {
            let dir = TestDir::new();
            let paths = Paths::new(&dir.path().join("home"));
            paths.ensure_home().unwrap();
            let repo = git_repo(dir.path());
            let factory = Arc::new(FakeFactory::default());
            let (mut hub, rx) = Hub::new(open(&paths), paths, Box::new(factory.clone())).unwrap();
            hub.set_boot_timeout(std::time::Duration::ZERO);
            Kit { hub: Arc::new(hub), rx, factory, repo, dir }
        }

        pub fn create(&self, agent: AgentKind, branch: &str) -> TaskView {
            self.hub
                .create_task(CreateTask {
                    repo: self.repo.to_string_lossy().into_owned(),
                    agent,
                    branch: branch.into(),
                    base: None,
                    goal: format!("goal of {branch}"),
                    title: None,
                })
                .unwrap()
        }

        pub fn kinds(&self, task_id: &str) -> Vec<String> {
            self.hub.lock().store.task_events(task_id).unwrap().into_iter().map(|e| e.kind).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::Kit;
    use super::*;
    use mirante_ipc::types::CreateTask;

    #[test]
    fn create_makes_a_worktree_on_the_users_branch_and_a_task() {
        let k = Kit::new();
        let v = k.create(AgentKind::Claude, "feat/login");
        assert_eq!(v.task.branch, "feat/login");
        assert_eq!(v.task.title, "goal of feat/login");
        assert!(Path::new(&v.task.worktree_path).join("README").exists());
        assert!(v.task.worktree_path.ends_with("worktrees/app/feat/login"), "{}", v.task.worktree_path);
        assert!(!v.live);
        assert_eq!(v.task.status, Status::Idle);
    }

    #[test]
    fn create_rejects_an_empty_goal_and_rolls_back_the_worktree_on_failure() {
        let k = Kit::new();
        let bad = CreateTask {
            repo: k.repo.to_string_lossy().into_owned(),
            agent: AgentKind::Codex,
            branch: "b1".into(),
            base: None,
            goal: "  ".into(),
            title: None,
        };
        assert!(matches!(k.hub.create_task(bad), Err(HubError::Invalid(_))));
        let missing = CreateTask {
            repo: "/definitely/not/here".into(),
            agent: AgentKind::Codex,
            branch: "b1".into(),
            base: None,
            goal: "g".into(),
            title: None,
        };
        assert!(matches!(k.hub.create_task(missing), Err(HubError::Invalid(_))));
    }

    #[test]
    fn tasks_resolve_by_id_branch_or_unique_suffix() {
        let k = Kit::new();
        let a = k.create(AgentKind::Claude, "alpha").task;
        let b = k.create(AgentKind::Codex, "beta").task;
        assert_eq!(k.hub.get_task(&a.id).unwrap().task.id, a.id);
        assert_eq!(k.hub.get_task("beta").unwrap().task.id, b.id);
        assert_eq!(k.hub.get_task(&b.id[b.id.len() - 6..]).unwrap().task.id, b.id);
        assert!(matches!(k.hub.get_task("nope"), Err(HubError::NotFound(_))));
        assert!(matches!(k.hub.get_task(""), Err(HubError::Invalid(_))));
        assert_eq!(k.hub.list_tasks(false).unwrap().len(), 2);
    }

    #[test]
    fn archive_removes_the_worktree_keeps_the_branch_and_hides_the_task() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "gone").task;
        let v = k.hub.archive("gone").unwrap();
        assert_eq!(v.task.lifecycle, Lifecycle::Archived);
        assert!(!Path::new(&t.worktree_path).exists());
        let branches = std::process::Command::new("git").arg("-C").arg(&k.repo).args(["branch"]).output().unwrap();
        assert!(String::from_utf8_lossy(&branches.stdout).contains("gone"));
        assert!(k.hub.list_tasks(false).unwrap().is_empty());
        assert_eq!(k.hub.list_tasks(true).unwrap().len(), 1);
        assert!(matches!(k.hub.archive(&t.id), Err(HubError::Conflict(_))));
    }

    #[test]
    fn subscribers_get_every_event_after_the_snapshot() {
        let k = Kit::new();
        k.create(AgentKind::Claude, "one");
        let (snap, rx) = k.hub.subscribe().unwrap();
        assert_eq!(snap.tasks.len(), 1);
        k.create(AgentKind::Claude, "two");
        let ev = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(ev.kind, "task_created");
        assert_eq!(ev.seq, snap.seq + 1);
        assert_eq!(ev.boot_id, snap.boot_id);
    }

    #[test]
    fn title_is_the_first_goal_line_capped() {
        assert_eq!(title_from_goal("\n  fix the login bug \nmore"), "fix the login bug");
        let long = "x".repeat(80);
        let t = title_from_goal(&long);
        assert_eq!(t.chars().count(), 60);
        assert!(t.ends_with('…'));
        assert_eq!(title_from_goal(""), "untitled");
    }

    #[test]
    fn errors_map_to_typed_rpc_codes() {
        let e: RpcError = HubError::Core(CoreError::TaskBusy("t".into())).into();
        assert_eq!(e.code, code::CONFLICT);
        let e: RpcError = HubError::Driver(DriverError::Unsupported("trust")).into();
        assert_eq!(e.code, code::UNSUPPORTED);
        let e: RpcError = HubError::NeedsYou("dialog".into()).into();
        assert_eq!(e.code, code::NEEDS_YOU);
        let e: RpcError = HubError::Core(CoreError::NotFound { kind: "task", id: "x".into() }).into();
        assert_eq!(e.code, code::NOT_FOUND);
    }
}
```

`crates/mirante-daemon/src/hub/messages.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use crate::driver::DriverEvent;
    use serde_json::Value;
    use std::sync::atomic::Ordering;

    fn send(k: &Kit, task: &str, text: &str, mode: SendMode) -> Result<SendResult> {
        k.hub.send(SendMessage { task: task.into(), text: text.into(), mode })
    }

    #[test]
    fn steer_goes_out_now_and_queue_waits_for_the_turn_end() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        assert_eq!(send(&k, &t.id, "use tabs", SendMode::Steer).unwrap().delivered, SendMode::Steer);
        let r = send(&k, &t.id, "then run tests", SendMode::Queue).unwrap();
        assert_eq!((r.delivered, r.queued), (SendMode::Queue, 1));
        assert_eq!(k.calls(), vec!["send:use tabs".to_string()]);
        k.hook(&t.id, json!({"hook_event_name": "Stop"}));
        assert_eq!(k.calls(), vec!["send:use tabs".to_string(), "send:then run tests".to_string()]);
        assert_eq!(k.hub.get_task(&t.id).unwrap().queued, 0);
    }

    #[test]
    fn queue_on_an_idle_agent_is_delivered_at_once() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        assert_eq!(send(&k, &t.id, "go", SendMode::Queue).unwrap().delivered, SendMode::Steer);
        assert_eq!(k.calls(), vec!["send:go".to_string()]);
    }

    #[test]
    fn steer_is_refused_while_a_dialog_is_open_but_queue_is_accepted() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        assert!(matches!(send(&k, &t.id, "1", SendMode::Steer), Err(HubError::NeedsYou(_))));
        assert_eq!(send(&k, &t.id, "later", SendMode::Queue).unwrap().delivered, SendMode::Queue);
        let c = k.started(AgentKind::Codex, "c").task;
        k.factory.driver.dialog.store(true, Ordering::SeqCst);
        assert!(matches!(send(&k, &c.id, "x", SendMode::Steer), Err(HubError::NeedsYou(_))));
        assert!(k.calls().is_empty());
    }

    #[test]
    fn messages_before_ready_are_queued_and_without_a_session_refused() {
        let k = Kit::new();
        let t = k.create(AgentKind::Claude, "a").task;
        assert!(matches!(send(&k, &t.id, "x", SendMode::Steer), Err(HubError::Conflict(_))));
        k.hub.start_session(mirante_ipc::types::StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
        assert_eq!(send(&k, &t.id, "early", SendMode::Steer).unwrap().delivered, SendMode::Queue);
        assert!(matches!(send(&k, &t.id, " ", SendMode::Steer), Err(HubError::Invalid(_))));
        k.emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(k.calls(), vec!["send:early".to_string()]);
    }

    #[test]
    fn steer_verdict_comes_from_the_transcript_at_the_turn_end() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let line = |v: Value| k.emit(DriverEvent::Transcript(v));
        line(json!({"type": "user", "origin": {"kind": "human"}, "message": {"content": "five steps"}}));
        send(&k, &t.id, "stop, say BANANA", SendMode::Steer).unwrap();
        line(json!({"type": "queue-operation", "operation": "enqueue", "content": "stop, say BANANA"}));
        line(json!({"type": "queue-operation", "operation": "remove", "reason": "absorbed_mid_turn"}));
        line(
            json!({"type": "assistant", "message": {"stop_reason": "end_turn", "content": [{"type": "text", "text": "BANANA"}]}}),
        );
        line(json!({"type": "system", "subtype": "turn_duration"}));
        let ev = k.hub.lock().store.task_events(&t.id).unwrap();
        let d = ev.iter().find(|e| e.kind == "message_delivery").expect("verdict");
        assert_eq!(d.payload["delivery"], json!("steered"));
    }

    #[test]
    fn claude_permission_answer_types_the_choice_then_persists_once() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(
            &t.id,
            json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": {"command": "touch x"}}),
        );
        let p = k.hub.permissions().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].branch, "a");
        let id = p[0].request.id.clone();
        let a = k.hub.answer_permission(AnswerPermission { id: id[id.len() - 6..].into(), allow: true }).unwrap();
        assert_eq!(a.status, PermissionStatus::AllowedOnce);
        assert_eq!(k.calls(), vec!["answer:touch x:true".to_string()]);
        assert_eq!(k.hub.get_task(&t.id).unwrap().task.status, Status::Working);
        assert!(matches!(k.hub.answer_permission(AnswerPermission { id, allow: false }), Err(HubError::NotFound(_))));
    }

    #[test]
    fn a_failed_driver_answer_persists_nothing() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        k.factory.driver.fail_answers.store(true, Ordering::SeqCst);
        let id = k.hub.permissions().unwrap()[0].request.id.clone();
        assert!(matches!(
            k.hub.answer_permission(AnswerPermission { id, allow: true }),
            Err(HubError::Driver(crate::driver::DriverError::NoDialog(_)))
        ));
        assert_eq!(k.hub.permissions().unwrap().len(), 1);
    }

    #[test]
    fn codex_approval_is_answered_with_its_request_key() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::PermissionRequested {
            key: "7".into(),
            tool: "command".into(),
            detail: "rm -rf build".into(),
        });
        let id = k.hub.permissions().unwrap()[0].request.id.clone();
        k.hub.answer_permission(AnswerPermission { id, allow: false }).unwrap();
        assert_eq!(k.calls(), vec!["answer:7:false".to_string()]);
        assert!(k.hub.lock().live[&t.id].perm_keys.is_empty());
    }

    #[test]
    fn older_claude_dialog_must_be_answered_first() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Edit", "agent_id": "a-1"}));
        let p = k.hub.permissions().unwrap();
        let newer = p[1].request.id.clone();
        assert!(matches!(
            k.hub.answer_permission(AnswerPermission { id: newer, allow: true }),
            Err(HubError::Conflict(_))
        ));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error: cannot find macro `json` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/hub/messages.rs`:
```rust
//! Talking to a running agent: steer now or queue for the turn end, and permission
//! answers (the queue pump and the steer verdict live in `session`).
use mirante_core::model::{AgentKind, Lifecycle, PermissionRequest, PermissionStatus, Status};
use mirante_core::status::Input;
use mirante_ipc::types::{AnswerPermission, BootState, PermissionView, SendMessage, SendMode, SendResult};
use serde_json::json;

use super::{Hub, HubError, Result, State};

impl State {
    /// A pending request by full id or unique id suffix (at least 4 characters).
    fn find_permission(&self, id: &str) -> Result<PermissionRequest> {
        let id = id.trim();
        let mut found = Vec::new();
        for t in self.store.list_tasks(false)? {
            for p in self.store.pending_permissions(&t.id)? {
                if p.id == id || (id.len() >= 4 && p.id.ends_with(id)) {
                    found.push(p);
                }
            }
        }
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => Err(HubError::NotFound(format!("no pending permission request {id}"))),
            _ => Err(HubError::Conflict(format!("permission id {id} is ambiguous"))),
        }
    }
}

impl Hub {
    /// Steer: hand the message to the agent now (starts a turn if idle). Refused while a
    /// dialog is open, because typed text would answer it. Queue: hold it until the running
    /// turn ends (delivered at once when the agent is already idle with an empty queue).
    /// Before the agent is ready, both modes queue.
    pub fn send(&self, m: SendMessage) -> Result<SendResult> {
        if m.text.trim().is_empty() {
            return Err(HubError::Invalid("empty message".into()));
        }
        let mut st = self.lock();
        let task = st.resolve(&m.task)?;
        let l = st.live.get_mut(&task.id).ok_or_else(|| {
            HubError::Conflict(format!("task {} has no live session: mirante resume {}", task.branch, task.branch))
        })?;
        let ready = l.boot == BootState::Ready && l.driver.is_some();
        let now = match m.mode {
            SendMode::Steer => {
                let dialog = l.driver.as_ref().is_some_and(|d| d.dialog_open());
                if dialog || (l.agent == AgentKind::Claude && task.status == Status::NeedsYou) {
                    return Err(HubError::NeedsYou(format!(
                        "task {} waits on a dialog: answer it first (mirante perms / mirante attach)",
                        task.branch
                    )));
                }
                ready
            }
            SendMode::Queue => ready && task.status == Status::Idle && l.queue.is_empty(),
        };
        if !now {
            l.queue.push_back(m.text.clone());
            let queued = l.queue.len();
            st.event(Some(&task.id), "message_queued", json!({ "text": m.text, "position": queued }))?;
            st.publish();
            return Ok(SendResult { delivered: SendMode::Queue, queued });
        }
        l.driver.clone().expect("ready implies a driver").send(&m.text)?;
        if l.agent == AgentKind::Claude && task.status == Status::Working {
            l.steers.push(m.text.clone());
        }
        let queued = l.queue.len();
        st.event(Some(&task.id), "message_sent", json!({ "mode": m.mode, "text": m.text }))?;
        st.publish();
        Ok(SendResult { delivered: SendMode::Steer, queued })
    }

    /// Pending permission requests of all non-archived tasks, oldest first per task.
    pub fn permissions(&self) -> Result<Vec<PermissionView>> {
        let st = self.lock();
        let mut out = Vec::new();
        for t in st.store.list_tasks(false)? {
            if t.lifecycle == Lifecycle::Archived {
                continue;
            }
            for p in st.store.pending_permissions(&t.id)? {
                out.push(PermissionView { request: p, task_title: t.title.clone(), branch: t.branch.clone() });
            }
        }
        Ok(out)
    }

    /// Answers a pending request once: the driver acts first (Claude: types the exact
    /// one-time option; Codex: answers the JSON-RPC request); only then is the answer
    /// persisted. Claude dialogs are answered in order, oldest first.
    pub fn answer_permission(&self, a: AnswerPermission) -> Result<PermissionRequest> {
        let mut st = self.lock();
        let p = st.find_permission(&a.id)?;
        let l = st
            .live
            .get(&p.task_id)
            .filter(|l| l.session_id == p.session_id)
            .ok_or_else(|| HubError::Conflict("the session that asked has ended".into()))?;
        let driver = l.driver.clone().ok_or_else(|| HubError::Conflict("agent still starting".into()))?;
        let agent = l.agent;
        let key = match agent {
            AgentKind::Claude => {
                let oldest =
                    st.store.pending_permissions(&p.task_id)?.into_iter().find(|q| q.session_id == p.session_id);
                if oldest.as_ref().is_some_and(|o| o.id != p.id) {
                    return Err(HubError::Conflict(format!(
                        "answer the older request {} first (Claude shows one dialog at a time)",
                        oldest.map(|o| o.id).unwrap_or_default()
                    )));
                }
                p.detail.clone()
            }
            AgentKind::Codex => l.perm_keys.get(&p.id).cloned().unwrap_or_default(),
        };
        driver.answer_permission(&key, a.allow)?;
        let status = if a.allow { PermissionStatus::AllowedOnce } else { PermissionStatus::Denied };
        let answered = st.store.answer_permission(&p.id, status)?;
        if let Some(l) = st.live.get_mut(&p.task_id) {
            l.perm_keys.remove(&p.id);
        }
        if agent == AgentKind::Claude {
            st.feed_claude(&p.task_id, &Input::PermissionAnswered { allowed: a.allow })?;
        }
        st.publish();
        Ok(answered)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): steer/queue messaging and once-only permission answers

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 9: Recovery at daemon start

**Files:**
- Modify: `crates/mirante-daemon/src/lib.rs`
- Create: `crates/mirante-daemon/src/recover.rs`

Agent processes do not outlive the daemon: Claude's PTY closes and Codex's stdin hits EOF when the daemon dies. So at start every session still live in the store is ended, its pending cards expire, and the task goes `Ended`, ready for `mirante resume` (spec §2.6, metric "context recreated by hand: 0"). Then the hook spool is ingested: a `SessionStart` spooled while the daemon was down still records the native session id. The spool is renamed to `hook-spool.jsonl.ingested` so it is read once.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/lib.rs`:
```rust
//! The Mirante daemon: the single owner of state. It keeps the `Store`, one live session per
//! task (agent process + status engine), serves the Unix socket, and turns hook events,
//! transcript lines and Codex notifications into persisted status, turns and permissions.
pub mod driver;
pub mod hub;
pub mod recover;
```

`crates/mirante-daemon/src/recover.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::DriverEvent;
    use crate::hub::testkit::Kit;
    use mirante_core::model::AgentKind;
    use mirante_core::store::Store;
    use mirante_ipc::types::StartSession;

    #[test]
    fn restart_ends_live_sessions_expires_permissions_and_keeps_resume_possible() {
        let k = Kit::with_store(|p| Store::open(&p.database()).unwrap());
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "SessionStart", "session_id": "nat-1", "source": "startup"}));
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        let idle = k.create(AgentKind::Codex, "never-started").task;
        let paths = k.hub.paths().clone();
        // Simulate a crash: a second hub over the same database, nothing shut down.
        let (mut hub2, _rx) =
            Hub::new(Store::open(&paths.database()).unwrap(), paths, Box::new(k.factory.clone())).unwrap();
        hub2.set_boot_timeout(std::time::Duration::ZERO);
        let r = hub2.recover().unwrap();
        assert_eq!((r.sessions_ended, r.permissions_expired), (1, 1));
        let v = hub2.get_task(&t.id).unwrap();
        assert_eq!((v.task.status, v.live, v.needs_you), (Status::Ended, false, 0));
        assert_eq!(hub2.get_task(&idle.id).unwrap().task.status, Status::Idle);
        let res = hub2.start_session(StartSession { task: t.id.clone(), prompt: None }, true).unwrap();
        assert!(res.resumed);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned().flatten().as_deref(), Some("nat-1"));
    }

    #[test]
    fn spooled_session_start_records_the_native_id_and_the_spool_is_consumed() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.emit(DriverEvent::Exited { code: None });
        let paths = k.hub.paths().clone();
        for payload in [r#"{"hook_event_name":"SessionStart","session_id":"nat-7","source":"startup"}"#, "garbage"] {
            spool_line(&paths, &t.id, payload);
        }
        let r = k.hub.recover().unwrap();
        assert_eq!(r.spooled_hooks, 2);
        let conv = k.hub.lock().store.latest_conversation(&t.id).unwrap().unwrap();
        assert_eq!(conv.native_session_id.as_deref(), Some("nat-7"));
        assert!(!paths.spool().exists());
        assert!(k.kinds(&t.id).contains(&"hook_spooled".to_string()));
        assert_eq!(k.hub.recover().unwrap().spooled_hooks, 0);
    }

    fn spool_line(paths: &mirante_ipc::Paths, task: &str, payload: &str) {
        use std::io::Write;
        let p = HookParams { task: task.into(), payload: payload.into(), ts_ms: 1 };
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(paths.spool()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&p).unwrap()).unwrap();
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error: cannot find macro `json` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/recover.rs`:
```rust
//! Daemon start: agent processes do not outlive the daemon (their PTY / pipes close with
//! it), so every session still live in the store is ended, its pending permission requests
//! expire, and the task becomes `Ended`, ready for `mirante resume`. Then hook payloads
//! spooled while the daemon was down are ingested: a `SessionStart` among them still
//! records the native session id, so a session that started while the daemon was down
//! can be resumed.
use mirante_core::hook::HookEvent;
use mirante_core::model::{PermissionStatus, Status};
use mirante_ipc::types::HookParams;
use serde_json::json;

use crate::hub::{Hub, Result};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Recovery {
    pub sessions_ended: usize,
    pub permissions_expired: usize,
    pub spooled_hooks: usize,
}

impl Hub {
    pub fn recover(&self) -> Result<Recovery> {
        let mut report = Recovery::default();
        let mut st = self.lock();
        for task in st.store.list_tasks(false)? {
            let live = st.store.live_session(&task.id)?;
            if let Some(s) = &live {
                st.store.end_session(&s.id)?;
                report.sessions_ended += 1;
                for p in st.store.pending_permissions(&task.id)? {
                    if p.session_id == s.id {
                        st.store.answer_permission(&p.id, PermissionStatus::Denied)?;
                        st.event(
                            Some(&task.id),
                            "permission_expired",
                            json!({ "permission_id": p.id, "cause": "mirante:daemon_restart" }),
                        )?;
                        report.permissions_expired += 1;
                    }
                }
            }
            let status = st.store.get_task(&task.id)?.status;
            if live.is_some() || matches!(status, Status::Working | Status::NeedsYou) {
                st.set_status(&task.id, Status::Ended, "mirante:daemon_restart")?;
            }
        }
        let spool = self.paths.spool();
        if let Ok(text) = std::fs::read_to_string(&spool) {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                report.spooled_hooks += 1;
                let Ok(p) = serde_json::from_str::<HookParams>(line) else {
                    st.event(None, "hook_spool_unparseable", json!({}))?;
                    continue;
                };
                let Ok(ev) = HookEvent::parse(&p.payload) else {
                    st.event(Some(&p.task), "hook_spool_unparseable", json!({}))?;
                    continue;
                };
                st.event(Some(&p.task), "hook_spooled", json!({ "hook": ev.hook_event_name, "ts_ms": p.ts_ms }))?;
                if ev.hook_event_name != "SessionStart" {
                    continue;
                }
                let (Some(native), Ok(Some(conv))) = (ev.session_id.as_deref(), st.store.latest_conversation(&p.task))
                else {
                    continue;
                };
                if conv.native_session_id.as_deref() != Some(native) {
                    st.store.set_native_session_id(&conv.id, native)?;
                }
            }
            std::fs::rename(&spool, spool.with_extension("jsonl.ingested")).map_err(mirante_core::CoreError::from)?;
        }
        st.publish();
        Ok(report)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): end stale sessions and ingest the hook spool at start

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 10: Socket server, `mirante-daemon` binary

**Files:**
- Modify: `crates/mirante-daemon/src/lib.rs`
- Modify: `crates/mirante-daemon/src/driver/mod.rs`
- Create: `crates/mirante-daemon/src/driver/real.rs`
- Create: `crates/mirante-daemon/src/server.rs`
- Create: `crates/mirante-daemon/src/main.rs`

Spec §4–5: `~/.mirante/daemon.sock`, mode 0600 (inside the 0700 home), and each connection's peer uid must equal the daemon's (`getpeereid` on macOS, `SO_PEERCRED` on Linux). One thread per connection.

- `events.subscribe` answers with the snapshot, then streams `event` notifications (each a stored `Event` with `seq` and `boot_id`).
- `attach` answers, then the connection carries raw bytes both ways: the agent's output to the client; the client's keystrokes to the agent (Claude) or nowhere (Codex, read-only). Either side closing ends it; the daemon detaches the client.
- `daemon.shutdown` stops every session gracefully, then calls back (the binary removes the socket and exits).

The binary takes an exclusive `flock` on `~/.mirante/daemon.lock` (one daemon per home; a stale socket file is replaced only while holding it), opens the store, recovers, starts the pump and serves. `RealFactory` is a typed `Unsupported` for both agents until Tasks 14 and 16.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/lib.rs`:
```rust
//! The Mirante daemon: the single owner of state. It keeps the `Store`, one live session per
//! task (agent process + status engine), serves the Unix socket, and turns hook events,
//! transcript lines and Codex notifications into persisted status, turns and permissions.
pub mod driver;
pub mod hub;
pub mod recover;
pub mod server;
```

`crates/mirante-daemon/src/driver/mod.rs`:
```rust
//! The agent driver interface. Every agent sits behind [`Driver`]; an operation the agent
//! cannot do is a typed [`DriverError::Unsupported`], never a silent failure. Drivers are
//! non-blocking (typing and protocol calls run on the driver's own threads) except
//! [`Driver::stop`], so the hub may call them while holding its lock.
//!
//! Drivers report back through a [`Sink`] (a channel drained by the hub's pump thread),
//! never by calling the hub directly, so a driver thread can never deadlock on the hub.
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc};

use mirante_core::model::{Status, Task};
use mirante_ipc::types::BootState;
use mirante_ipc::Paths;
use serde_json::Value;

#[cfg(test)]
pub mod fake;
pub mod real;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The agent cannot do this (e.g. a trust prompt on Codex).
    Unsupported(&'static str),
    /// The dialog the operation targets is not on screen / not pending.
    NoDialog(String),
    Failed(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Unsupported(what) => write!(f, "not supported by this agent: {what}"),
            DriverError::NoDialog(m) => write!(f, "no matching dialog: {m}"),
            DriverError::Failed(m) => write!(f, "agent driver failed: {m}"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Failed(e.to_string())
    }
}

/// What a driver tells the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// Claude: one main-transcript line, for the hub's `StatusEngine`.
    Transcript(Value),
    /// Claude: where the transcript tailer is (persisted, not audited).
    TranscriptOffset {
        path: String,
        offset: u64,
    },
    /// Codex: a status computed by the driver's own engine. `turn_open` drives the turn
    /// records; `detail` is shown next to the status.
    Status {
        to: Status,
        cause: String,
        turn_open: bool,
        detail: Option<String>,
    },
    /// Codex: an approval request. `key` is what `answer_permission` gets back.
    PermissionRequested {
        key: String,
        tool: String,
        detail: String,
    },
    /// Codex: the thread id, used for `thread/resume`.
    NativeId(String),
    Boot(BootState),
    /// A step for the feed and audit trail (command finished, agent message, ...).
    Step(Value),
    Exited {
        code: Option<i32>,
    },
}

/// Channel end a driver emits into. Every event is tagged with the driver's session id so
/// late events from an ended session are ignored.
#[derive(Clone)]
pub struct Sink {
    session_id: String,
    tx: mpsc::Sender<(String, DriverEvent)>,
}

impl Sink {
    pub fn new(session_id: &str, tx: mpsc::Sender<(String, DriverEvent)>) -> Sink {
        Sink { session_id: session_id.to_string(), tx }
    }

    pub fn emit(&self, event: DriverEvent) {
        let _ = self.tx.send((self.session_id.clone(), event));
    }
}

/// A live view of the agent's terminal (Claude) or event stream (Codex).
pub struct Attachment {
    pub id: u64,
    /// Bytes that repaint the current screen (or recent stream) before live output.
    pub initial: Vec<u8>,
    pub output: mpsc::Receiver<Vec<u8>>,
    /// Whether typed bytes reach the agent (`Driver::input`).
    pub input: bool,
}

/// Fan-out of output bytes to attached clients; dead receivers are dropped on send.
#[derive(Default)]
pub struct Fanout {
    next: u64,
    subs: HashMap<u64, mpsc::Sender<Vec<u8>>>,
}

impl Fanout {
    pub fn subscribe(&mut self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        self.next += 1;
        let (tx, rx) = mpsc::channel();
        self.subs.insert(self.next, tx);
        (self.next, rx)
    }

    pub fn remove(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.subs.retain(|_, tx| tx.send(bytes.to_vec()).is_ok());
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }
}

pub trait Driver: Send + Sync {
    /// Delivers a message now: types it into the TUI (Claude) or steers / starts a turn
    /// (Codex). Returns once the message is handed to the driver's worker.
    fn send(&self, text: &str) -> Result<(), DriverError>;
    /// Esc (Claude) or `turn/interrupt` (Codex).
    fn interrupt(&self) -> Result<(), DriverError>;
    /// Answers the permission dialog / approval request named by `key` (Codex: the request
    /// id; Claude: the request detail, checked against the dialog on screen). Selects only
    /// a one-time "Yes" or "No", never an "always" or "auto" option.
    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError>;
    /// Answers Claude's workspace-trust dialog. `false` ends the session.
    fn trust(&self, accept: bool) -> Result<(), DriverError>;
    /// True while a dialog would capture typed text (a message must not be typed then).
    fn dialog_open(&self) -> bool;
    /// Claude: start (or switch) tailing the session transcript at `offset`.
    fn tail_transcript(&self, path: &Path, offset: u64);
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError>;
    fn input(&self, attachment: u64, bytes: &[u8]) -> Result<(), DriverError>;
    fn detach(&self, attachment: u64);
    /// Last lines of the screen (Claude) or stream (Codex), for the CLI.
    fn screen_tail(&self) -> String;
    /// Ends the agent gracefully, then kills it. Blocks until the process is gone; the
    /// driver still emits `Exited`.
    fn stop(&self);
}

pub struct SpawnRequest {
    pub task: Task,
    pub session_id: String,
    /// Native id to resume (Claude session id / Codex thread id).
    pub resume: Option<String>,
    pub paths: Paths,
}

pub trait DriverFactory: Send + Sync {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_drops_dead_receivers() {
        let mut f = Fanout::default();
        let (a, rx_a) = f.subscribe();
        let (_b, rx_b) = f.subscribe();
        f.send(b"x");
        assert_eq!(rx_a.recv().unwrap(), b"x");
        drop(rx_b);
        f.send(b"y");
        assert_eq!(rx_a.recv().unwrap(), b"y");
        f.remove(a);
        assert!(f.is_empty());
    }

    #[test]
    fn sink_tags_events_with_the_session() {
        let (tx, rx) = mpsc::channel();
        Sink::new("s1", tx).emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(rx.recv().unwrap(), ("s1".to_string(), DriverEvent::Boot(BootState::Ready)));
    }
}
```

`crates/mirante-daemon/src/driver/real.rs`:
```rust
//! The production driver factory. The Claude and Codex drivers are wired in by Tasks 14
//! and 16; until then starting a session is a typed `Unsupported` error.
use std::sync::Arc;

use mirante_core::model::AgentKind;

use super::{Driver, DriverError, DriverFactory, Sink, SpawnRequest};

#[derive(Debug, Default, Clone)]
pub struct RealFactory;

impl DriverFactory for RealFactory {
    fn spawn(&self, req: SpawnRequest, _sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        match req.task.agent {
            AgentKind::Claude => Err(DriverError::Unsupported("the Claude driver is not built yet")),
            AgentKind::Codex => Err(DriverError::Unsupported("the Codex driver is not built yet")),
        }
    }
}
```

`crates/mirante-daemon/src/main.rs`:
```rust
//! `mirante-daemon`: one per `MIRANTE_HOME` (flock), owns the store, serves the socket.
use std::fs::File;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use mirante_core::store::Store;
use mirante_daemon::driver::real::RealFactory;
use mirante_daemon::hub::Hub;
use mirante_daemon::server;
use mirante_ipc::Paths;

fn lock(path: &Path) -> io::Result<File> {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(path)?;
    // SAFETY: valid fd owned by `f`, which the caller keeps alive for the process lifetime.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::new(io::ErrorKind::WouldBlock, "another mirante-daemon is running for this home"));
    }
    Ok(f)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let paths = Paths::from_env()?;
    paths.ensure_home()?;
    let _lock = lock(&paths.lock())?;
    let store = Store::open(&paths.database())?;
    let (hub, rx) = Hub::new(store, paths.clone(), Box::new(RealFactory::default()))?;
    let hub = Arc::new(hub);
    let report = hub.recover()?;
    eprintln!("mirante-daemon: recovered {report:?}");
    hub.spawn_pump(rx);
    let listener = server::bind(&paths.socket())?;
    eprintln!("mirante-daemon: pid {} listening on {}", std::process::id(), paths.socket().display());
    let socket = paths.socket();
    server::serve(
        hub,
        listener,
        Arc::new(move || {
            let _ = std::fs::remove_file(&socket);
            std::process::exit(0);
        }),
    )?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mirante-daemon: {e}");
            ExitCode::FAILURE
        }
    }
}
```

`crates/mirante-daemon/src/server.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testkit::Kit;
    use mirante_core::model::AgentKind;
    use mirante_ipc::types::{CreateTask, Snapshot, TaskView};
    use mirante_ipc::{Client, ClientError};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn serve_kit(k: &Kit) -> (std::path::PathBuf, Arc<AtomicBool>) {
        let sock = k.dir.path().join("d.sock");
        let listener = bind(&sock).unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let hub = k.hub.clone();
        std::thread::spawn(move || serve(hub, listener, Arc::new(move || f.store(true, Ordering::SeqCst))));
        (sock, flag)
    }

    #[test]
    fn socket_is_private_and_the_peer_is_us() {
        let k = Kit::new();
        let (sock, _) = serve_kit(&k);
        assert_eq!(std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&a).unwrap(), my_uid());
    }

    #[test]
    fn requests_round_trip_with_typed_errors() {
        let k = Kit::new();
        let (sock, _) = serve_kit(&k);
        let mut c = Client::connect(&sock).unwrap();
        let created: TaskView = c
            .call(
                method::TASK_CREATE,
                CreateTask {
                    repo: k.repo.to_string_lossy().into_owned(),
                    agent: AgentKind::Codex,
                    branch: "rpc".into(),
                    base: None,
                    goal: "g".into(),
                    title: Some("Via RPC".into()),
                },
            )
            .unwrap();
        assert_eq!(created.task.title, "Via RPC");
        let list: Vec<TaskView> = c.call(method::TASK_LIST, Value::Null).unwrap();
        assert_eq!(list.len(), 1);
        let got: TaskView = c.call(method::TASK_GET, json!({"task": "rpc"})).unwrap();
        assert_eq!(got.task.id, created.task.id);
        let code_of = |r: Result<Value, ClientError>| match r {
            Err(ClientError::Rpc(e)) => e.code,
            other => panic!("{other:?}"),
        };
        assert_eq!(code_of(c.call(method::TASK_GET, json!({"task": "zzzz"}))), code::NOT_FOUND);
        assert_eq!(code_of(c.call(method::TASK_GET, json!({"nope": 1}))), code::INVALID_PARAMS);
        assert_eq!(code_of(c.call("task.explode", json!({}))), code::METHOD_NOT_FOUND);
        assert_eq!(code_of(c.call(method::SESSION_STOP, json!({"task": "rpc"}))), code::CONFLICT);
    }

    #[test]
    fn subscribe_sends_a_snapshot_then_events() {
        let k = Kit::new();
        let (sock, _) = serve_kit(&k);
        let mut sub = Client::connect(&sock).unwrap();
        let snap: Snapshot = sub.call(method::EVENTS_SUBSCRIBE, json!({})).unwrap();
        assert!(snap.tasks.is_empty());
        k.create(AgentKind::Claude, "x");
        let n = sub.next_notification().unwrap().unwrap();
        assert_eq!(n.method, method::EVENT);
        assert_eq!(n.params["kind"], "task_created");
        assert_eq!(n.params["seq"].as_u64().unwrap(), snap.seq + 1);
    }

    #[test]
    fn attach_streams_raw_bytes_both_ways_until_the_client_closes() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let (sock, _) = serve_kit(&k);
        let mut c = Client::connect(&sock).unwrap();
        let r: AttachResult = c.call(method::ATTACH, json!({"task": t.id, "rows": 24, "cols": 80})).unwrap();
        assert!(r.input);
        let (mut reader, mut writer) = c.into_raw();
        let mut got = [0u8; 10];
        reader.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"screenlive");
        writer.write_all(b"yo").unwrap();
        writer.shutdown(Shutdown::Write).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !k.calls().contains(&"detach:1".to_string()) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let calls = k.calls();
        assert!(calls.contains(&"input:yo".to_string()), "{calls:?}");
        assert!(calls.contains(&"detach:1".to_string()), "{calls:?}");
    }

    #[test]
    fn shutdown_stops_sessions_then_calls_back() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        let (sock, flag) = serve_kit(&k);
        let mut c = Client::connect(&sock).unwrap();
        let _: Value = c.call(method::DAEMON_SHUTDOWN, json!({})).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !flag.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(flag.load(Ordering::SeqCst));
        assert!(!k.hub.get_task(&t.id).unwrap().live);
    }

    #[test]
    fn garbage_lines_get_a_parse_error_and_the_connection_survives() {
        let k = Kit::new();
        let (sock, _) = serve_kit(&k);
        let mut s = UnixStream::connect(&sock).unwrap();
        s.write_all(b"not json\n").unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let resp: Response = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(resp.error.unwrap().code, code::PARSE);
        write_line(&mut s, &Request::new(2, method::DAEMON_STATUS, Value::Null)).unwrap();
        let resp: Response = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(resp.result.unwrap()["pid"], std::process::id());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error: cannot find macro `json` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/server.rs`:
```rust
//! The daemon socket: `~/.mirante/daemon.sock`, mode 0600, and every connection's peer uid
//! must equal the daemon's. One thread per connection; newline-delimited JSON-RPC 2.0.
//! Two methods take over their connection: `events.subscribe` (a snapshot response, then
//! `event` notifications) and `attach` (a response, then raw bytes both ways until either
//! side closes).
use std::io::{self, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;

use mirante_ipc::rpc::{code, read_line, write_line, Notification, Request, Response, RpcError};
use mirante_ipc::types::{method, AttachParams, AttachResult, ListParams, TaskRef};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::hub::{self, Hub};

pub type OnShutdown = Arc<dyn Fn() + Send + Sync>;

/// Binds the socket with mode 0600, replacing a stale file. Call only while holding the
/// daemon lock, so a live daemon's socket is never removed.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// The uid of the process on the other end of a connected Unix socket.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd"))]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: fd is a valid connected socket for the duration of the call.
        if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: cred/len are valid for writes of the declared size.
        let rc = unsafe {
            libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len)
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
}

pub fn my_uid() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

pub fn serve(hub: Arc<Hub>, listener: UnixListener, on_shutdown: OnShutdown) -> io::Result<()> {
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mirante-daemon: accept: {e}");
                continue;
            }
        };
        let (hub, on_shutdown) = (hub.clone(), on_shutdown.clone());
        std::thread::spawn(move || {
            let _ = handle(&hub, stream, &*on_shutdown);
        });
    }
    Ok(())
}

pub fn handle(hub: &Hub, stream: UnixStream, on_shutdown: &(dyn Fn() + Send + Sync)) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    if peer_uid(&stream)? != my_uid() {
        let e = RpcError::new(code::FORBIDDEN, "peer uid does not match the daemon's");
        return write_line(&mut writer, &Response::err(None, e));
    }
    let mut reader = BufReader::new(stream);
    while let Some(line) = read_line(&mut reader)? {
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_line(&mut writer, &Response::err(None, RpcError::new(code::PARSE, e.to_string())))?;
                continue;
            }
        };
        match req.method.as_str() {
            method::EVENTS_SUBSCRIBE => return subscribe(hub, req.id, writer),
            method::ATTACH => return attach(hub, req, reader, writer),
            method::DAEMON_SHUTDOWN => {
                write_line(&mut writer, &Response::ok(req.id, json!({})))?;
                hub.shutdown();
                on_shutdown();
                return Ok(());
            }
            _ => {
                let resp = match dispatch(hub, &req.method, req.params) {
                    Ok(v) => Response::ok(req.id, v),
                    Err(e) => Response::err(Some(req.id), e),
                };
                write_line(&mut writer, &resp)?;
            }
        }
    }
    Ok(())
}

fn params<T: DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    serde_json::from_value(v).map_err(|e| RpcError::new(code::INVALID_PARAMS, e.to_string()))
}

fn out<T: Serialize>(r: hub::Result<T>) -> Result<Value, RpcError> {
    let v = r.map_err(RpcError::from)?;
    serde_json::to_value(v).map_err(|e| RpcError::new(code::INTERNAL, e.to_string()))
}

/// Every request/response method. Unit-testable without a socket.
pub fn dispatch(hub: &Hub, m: &str, p: Value) -> Result<Value, RpcError> {
    match m {
        method::TASK_CREATE => out(hub.create_task(params(p)?)),
        method::TASK_LIST => {
            let l: ListParams = if p.is_null() { ListParams::default() } else { params(p)? };
            out(hub.list_tasks(l.archived))
        }
        method::TASK_GET => out(hub.get_task(&params::<TaskRef>(p)?.task)),
        method::TASK_ARCHIVE => out(hub.archive(&params::<TaskRef>(p)?.task)),
        method::SESSION_START => out(hub.start_session(params(p)?, false)),
        method::SESSION_RESUME => out(hub.start_session(params(p)?, true)),
        method::SESSION_TRUST => out(hub.trust(params(p)?)),
        method::SESSION_STOP => out(hub.stop(&params::<TaskRef>(p)?.task)),
        method::SESSION_INTERRUPT => out(hub.interrupt(&params::<TaskRef>(p)?.task).map(|()| json!({}))),
        method::MESSAGE_SEND => out(hub.send(params(p)?)),
        method::PERMISSION_LIST => out(hub.permissions()),
        method::PERMISSION_ANSWER => out(hub.answer_permission(params(p)?)),
        method::HOOK_EVENT => out(hub.apply_hook(params(p)?).map(|()| json!({}))),
        method::DAEMON_STATUS => out(Ok(hub.status())),
        other => Err(RpcError::new(code::METHOD_NOT_FOUND, format!("unknown method {other}"))),
    }
}

fn subscribe(hub: &Hub, id: u64, mut writer: UnixStream) -> io::Result<()> {
    let (snapshot, rx) = match hub.subscribe() {
        Ok(x) => x,
        Err(e) => return write_line(&mut writer, &Response::err(Some(id), e.into())),
    };
    write_line(&mut writer, &Response::ok(id, serde_json::to_value(snapshot)?))?;
    for event in rx {
        write_line(&mut writer, &Notification::new(method::EVENT, serde_json::to_value(event)?))?;
    }
    Ok(())
}

fn attach(hub: &Hub, req: Request, mut reader: BufReader<UnixStream>, mut writer: UnixStream) -> io::Result<()> {
    let p: AttachParams = match params(req.params) {
        Ok(p) => p,
        Err(e) => return write_line(&mut writer, &Response::err(Some(req.id), e)),
    };
    let (driver, a) = match hub.attach(p) {
        Ok(x) => x,
        Err(e) => return write_line(&mut writer, &Response::err(Some(req.id), e.into())),
    };
    write_line(&mut writer, &Response::ok(req.id, serde_json::to_value(AttachResult { input: a.input })?))?;
    writer.write_all(&a.initial)?;
    let mut out = writer.try_clone()?;
    let output = a.output;
    let pump = std::thread::spawn(move || {
        for chunk in output {
            if out.write_all(&chunk).is_err() {
                break;
            }
        }
        // Output ended (agent exited) or the client is gone: close both ways.
        let _ = out.shutdown(Shutdown::Both);
    });
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) if a.input => {
                if driver.input(a.id, &buf[..n]).is_err() {
                    break;
                }
            }
            Ok(_) => {}
        }
    }
    driver.detach(a.id);
    let _ = writer.shutdown(Shutdown::Both);
    let _ = pump.join();
    Ok(())
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Smoke-test the binary in a temp home**

```bash
cargo build -p mirante-daemon
H=/tmp/mr-smoke; rm -rf $H
MIRANTE_HOME=$H ./target/debug/mirante-daemon 2>/tmp/mr-smoke.log &
sleep 1
stat -f '%Lp' $H/daemon.sock            # Linux: stat -c '%a'
printf '{"jsonrpc":"2.0","id":1,"method":"daemon.status"}\n' | nc -U -w 1 $H/daemon.sock
MIRANTE_HOME=$H ./target/debug/mirante-daemon; echo "exit $?"
printf '{"jsonrpc":"2.0","id":2,"method":"daemon.shutdown"}\n' | nc -U -w 1 $H/daemon.sock
rm -rf $H /tmp/mr-smoke.log
```

Expected (ids, pids and boot ids differ):
```
600
{"jsonrpc":"2.0","id":1,"result":{"boot_id":"…","live_sessions":0,"pid":…,"version":"0.1.0"}}
mirante-daemon: another mirante-daemon is running for this home
exit 1
{"jsonrpc":"2.0","id":2,"result":{}}
```

- [ ] **Step 6: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): private Unix socket with peer-uid check, subscribe/attach, daemon binary

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 11: CLI library: argument parsing and tables

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/mirante-cli/Cargo.toml`
- Create: `crates/mirante-cli/src/lib.rs`
- Create: `crates/mirante-cli/src/args.rs`
- Create: `crates/mirante-cli/src/render.rs`

The CLI is a thin client; its logic that can be unit-tested lives in a library (`mirante_cli`), the binary comes in Task 12. No argument-parsing dependency: a small parser with flags anywhere, `--flag=value`, and `--` to protect message text that starts with dashes. Tables are plain text; colors only on a terminal (needs_you is bold red). Short ids are the last 8 characters of the UUID.

- [ ] **Step 1: Write the failing tests**

`Cargo.toml`:
```toml
[workspace]
members = [
    "crates/spike-kit",
    "crates/mirante-core",
    "crates/mirante-ipc",
    "crates/mirante-hook",
    "crates/mirante-daemon",
    "crates/mirante-cli",
]
resolver = "2"
```

`crates/mirante-cli/Cargo.toml`:
```toml
[package]
name = "mirante-cli"
version = "0.1.0"
edition = "2021"
publish = false
license = "Apache-2.0"

[dependencies]
mirante-core = { path = "../mirante-core" }
mirante-ipc = { path = "../mirante-ipc" }
serde_json = "1"
libc = "0.2"
```

`crates/mirante-cli/src/lib.rs`:
```rust
//! Library half of the `mirante` CLI (the binary is `src/main.rs`): argument parsing,
//! rendering, terminal helpers and daemon start-up.
pub mod args;
pub mod render;
```

`crates/mirante-cli/src/args.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Cmd, String> {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn new_takes_flags_anywhere_and_joins_the_goal() {
        assert_eq!(
            p("new ~/app --agent codex fix the login --branch feat/login --base=release").unwrap(),
            Cmd::New {
                repo: "~/app".into(),
                agent: AgentKind::Codex,
                branch: "feat/login".into(),
                base: Some("release".into()),
                title: None,
                goal: "fix the login".into(),
                trust: false,
            }
        );
        assert!(matches!(p("new . --agent claude --branch b --trust go").unwrap(), Cmd::New { trust: true, .. }));
    }

    #[test]
    fn new_reports_what_is_missing() {
        assert_eq!(p("new . --branch b go").unwrap_err(), "missing --agent claude|codex");
        assert_eq!(p("new . --agent claude go").unwrap_err(), "missing --branch <name>");
        assert_eq!(p("new . --agent claude --branch b").unwrap_err(), "missing <goal>");
        assert_eq!(p("new . --agent gpt --branch b go").unwrap_err(), "unknown agent gpt (claude or codex)");
        assert_eq!(p("new . --agent").unwrap_err(), "--agent needs a value");
    }

    #[test]
    fn say_defaults_to_steer_and_double_dash_protects_text() {
        assert_eq!(
            p("say feat use tabs").unwrap(),
            Cmd::Say { task: "feat".into(), text: "use tabs".into(), queue: false }
        );
        assert_eq!(
            p("say --queue feat -- --not-a-flag").unwrap(),
            Cmd::Say { task: "feat".into(), text: "--not-a-flag".into(), queue: true }
        );
        assert_eq!(p("say feat").unwrap_err(), "missing <message>");
    }

    #[test]
    fn simple_commands() {
        assert_eq!(p("").unwrap(), Cmd::Help);
        assert_eq!(p("ls --all").unwrap(), Cmd::Ls { all: true });
        assert_eq!(p("allow ab12").unwrap(), Cmd::Answer { id: "ab12".into(), allow: true });
        assert_eq!(p("deny ab12").unwrap(), Cmd::Answer { id: "ab12".into(), allow: false });
        assert_eq!(p("resume feat --trust").unwrap(), Cmd::Resume { task: "feat".into(), trust: true });
        assert_eq!(p("daemon status").unwrap(), Cmd::Daemon(DaemonCmd::Status));
        assert_eq!(p("watch").unwrap(), Cmd::Watch);
        assert!(p("stop").is_err());
        assert!(p("stop a b").is_err());
        assert!(p("ls --bogus").is_err());
        assert!(p("frobnicate").is_err());
    }
}
```

`crates/mirante-cli/src/render.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{AgentKind, Lifecycle, PermissionStatus, Task};

    fn view(status: Status, needs: usize, detail: Option<&str>) -> TaskView {
        TaskView {
            task: Task {
                id: "0190aaaa-bbbb-7ccc-8ddd-eeee12345678".into(),
                title: "Fix the login".into(),
                repo_path: "/r".into(),
                worktree_path: "/w".into(),
                branch: "feat/login".into(),
                agent: AgentKind::Claude,
                goal: "fix it".into(),
                notes: String::new(),
                lifecycle: Lifecycle::Active,
                status,
                created_at: 0,
                updated_at: 0,
            },
            live: true,
            boot: Some(BootState::Ready),
            needs_you: needs,
            queued: 0,
            detail: detail.map(String::from),
        }
    }

    #[test]
    fn short_ids_are_the_unique_suffix() {
        assert_eq!(short("0190aaaa-bbbb-7ccc-8ddd-eeee12345678"), "12345678");
        assert_eq!(short("abc"), "abc");
    }

    #[test]
    fn fit_truncates_and_pads_by_characters() {
        assert_eq!(fit("ação", 6), "ação  ");
        assert_eq!(fit("abcdefgh", 5), "abcd…");
    }

    #[test]
    fn table_shows_status_needs_and_detail() {
        let t = tasks_table(
            &[view(Status::NeedsYou, 2, None), view(Status::Working, 0, Some("turn done, 1 command still finishing"))],
            false,
        );
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("ID"));
        assert!(lines[1].contains("12345678") && lines[1].contains("needs_you") && lines[1].contains(" 2 "));
        assert!(lines[2].contains("[turn done, 1 command still finishing]"));
        assert!(!t.contains('\x1b'));
        assert!(tasks_table(&[view(Status::Idle, 0, None)], true).contains("\x1b[32m"));
    }

    #[test]
    fn perms_table_says_how_to_answer() {
        let p = PermissionView {
            request: PermissionRequest {
                id: "p-0000-abcdef12".into(),
                task_id: "t".into(),
                session_id: "s".into(),
                tool: "Bash".into(),
                detail: "rm -rf build\nsecond line".into(),
                status: PermissionStatus::Pending,
                created_at: 0,
                answered_at: None,
            },
            task_title: "T".into(),
            branch: "feat".into(),
        };
        let t = perms_table(&[p]);
        assert!(t.contains("abcdef12") && t.contains("rm -rf build") && !t.contains("second line"));
        assert!(t.contains("mirante allow <perm>"));
        assert_eq!(perms_table(&[]), "no pending permission requests\n");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-cli`

Expected: FAIL to compile; the first error is
```
error[E0425]: cannot find type `Cmd` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-cli/src/args.rs`:
```rust
//! Command-line parsing (no dependencies). Flags may come before or after positionals;
//! `--` ends flags; `--flag=value` is accepted.
use std::collections::HashMap;

use mirante_core::model::AgentKind;

pub const USAGE: &str = "\
usage: mirante <command>

  new <repo> --agent claude|codex --branch <name> [--base <ref>] [--title <t>] [--trust] <goal...>
                         create a worktree + task and start the agent with <goal>
  ls [--all]             tasks with status and what needs you (--all: include archived)
  show <task>            one task in detail
  watch                  live status board (rings the bell when a task needs you)
  attach <task>          live terminal (Claude) or event stream (Codex); detach with Ctrl-]
  say <task> [--queue] <message...>
                         steer now (default) or queue for the end of the running turn
  perms                  pending permission requests
  allow <perm> | deny <perm>
                         answer one request, once (never \"always\")
  interrupt <task>       stop the current step (Esc / turn/interrupt)
  stop <task>            end the agent process (resumable)
  resume <task> [--trust]
                         restart the agent on its native session (context kept)
  archive <task>         remove the worktree (branch kept) and hide the task
  daemon start|stop|status

<task> is a task id, a unique id suffix (4+ chars) or the branch name.
<perm> is a permission id or a unique suffix.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonCmd {
    Start,
    Stop,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    New {
        repo: String,
        agent: AgentKind,
        branch: String,
        base: Option<String>,
        title: Option<String>,
        goal: String,
        trust: bool,
    },
    Ls {
        all: bool,
    },
    Show {
        task: String,
    },
    Watch,
    Attach {
        task: String,
    },
    Say {
        task: String,
        text: String,
        queue: bool,
    },
    Perms,
    Answer {
        id: String,
        allow: bool,
    },
    Interrupt {
        task: String,
    },
    Stop {
        task: String,
    },
    Resume {
        task: String,
        trust: bool,
    },
    Archive {
        task: String,
    },
    Daemon(DaemonCmd),
    Help,
}

struct Parsed {
    positional: Vec<String>,
    values: HashMap<String, String>,
    bools: Vec<String>,
}

fn split(args: &[String], with_value: &[&str], bools: &[&str]) -> Result<Parsed, String> {
    let mut p = Parsed { positional: Vec::new(), values: HashMap::new(), bools: Vec::new() };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            p.positional.extend(it.by_ref().cloned());
            break;
        }
        if !a.starts_with("--") {
            p.positional.push(a.clone());
            continue;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (a.as_str(), None),
        };
        if with_value.contains(&name) {
            let v = match inline {
                Some(v) => v,
                None => it.next().cloned().ok_or_else(|| format!("{name} needs a value"))?,
            };
            p.values.insert(name.to_string(), v);
        } else if bools.contains(&name) && inline.is_none() {
            p.bools.push(name.to_string());
        } else {
            return Err(format!("unknown option {a}"));
        }
    }
    Ok(p)
}

fn one(p: &Parsed, what: &str) -> Result<String, String> {
    match p.positional.as_slice() {
        [x] => Ok(x.clone()),
        [] => Err(format!("missing <{what}>")),
        _ => Err(format!("expected one <{what}>, got {}", p.positional.len())),
    }
}

/// The task followed by a free text of one or more words.
fn task_and_text(p: &Parsed, what: &str) -> Result<(String, String), String> {
    let (task, words) = p.positional.split_first().ok_or("missing <task>")?;
    let text = words.join(" ");
    if text.trim().is_empty() {
        return Err(format!("missing <{what}>"));
    }
    Ok((task.clone(), text))
}

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let Some((sub, rest)) = args.split_first() else {
        return Ok(Cmd::Help);
    };
    let none: &[&str] = &[];
    match sub.as_str() {
        "new" => {
            let p = split(rest, &["--agent", "--branch", "--base", "--title"], &["--trust"])?;
            let (repo, goal) = task_and_text(&p, "goal").map_err(|e| e.replace("<task>", "<repo>"))?;
            let agent = match p.values.get("--agent").map(String::as_str) {
                Some("claude") => AgentKind::Claude,
                Some("codex") => AgentKind::Codex,
                Some(other) => return Err(format!("unknown agent {other} (claude or codex)")),
                None => return Err("missing --agent claude|codex".into()),
            };
            let branch = p.values.get("--branch").cloned().ok_or("missing --branch <name>")?;
            Ok(Cmd::New {
                repo,
                agent,
                branch,
                base: p.values.get("--base").cloned(),
                title: p.values.get("--title").cloned(),
                goal,
                trust: p.bools.iter().any(|b| b == "--trust"),
            })
        }
        "ls" | "list" => {
            let p = split(rest, none, &["--all"])?;
            if !p.positional.is_empty() {
                return Err("ls takes no arguments".into());
            }
            Ok(Cmd::Ls { all: !p.bools.is_empty() })
        }
        "show" => Ok(Cmd::Show { task: one(&split(rest, none, none)?, "task")? }),
        "watch" => split(rest, none, none)?
            .positional
            .is_empty()
            .then_some(Cmd::Watch)
            .ok_or("watch takes no arguments".into()),
        "attach" => Ok(Cmd::Attach { task: one(&split(rest, none, none)?, "task")? }),
        "say" => {
            let p = split(rest, none, &["--queue"])?;
            let (task, text) = task_and_text(&p, "message")?;
            Ok(Cmd::Say { task, text, queue: !p.bools.is_empty() })
        }
        "perms" => Ok(Cmd::Perms),
        "allow" | "deny" => Ok(Cmd::Answer { id: one(&split(rest, none, none)?, "perm")?, allow: sub == "allow" }),
        "interrupt" => Ok(Cmd::Interrupt { task: one(&split(rest, none, none)?, "task")? }),
        "stop" => Ok(Cmd::Stop { task: one(&split(rest, none, none)?, "task")? }),
        "resume" => {
            let p = split(rest, none, &["--trust"])?;
            Ok(Cmd::Resume { task: one(&p, "task")?, trust: !p.bools.is_empty() })
        }
        "archive" => Ok(Cmd::Archive { task: one(&split(rest, none, none)?, "task")? }),
        "daemon" => match one(&split(rest, none, none)?, "start|stop|status")?.as_str() {
            "start" => Ok(Cmd::Daemon(DaemonCmd::Start)),
            "stop" => Ok(Cmd::Daemon(DaemonCmd::Stop)),
            "status" => Ok(Cmd::Daemon(DaemonCmd::Status)),
            other => Err(format!("unknown daemon command {other}")),
        },
        "help" | "-h" | "--help" => Ok(Cmd::Help),
        other => Err(format!("unknown command {other}")),
    }
}
```

Add above the test module in `crates/mirante-cli/src/render.rs`:
```rust
//! Plain-text tables. Colors only on a terminal (`color` = stdout is a tty).
use mirante_core::model::{PermissionRequest, Status};
use mirante_ipc::types::{BootState, PermissionView, TaskView};

/// Last 8 characters of an id (UUID v7 prefixes are time-based and collide; suffixes do not).
pub fn short(id: &str) -> &str {
    let mut start = id.len().saturating_sub(8);
    while !id.is_char_boundary(start) {
        start += 1;
    }
    &id[start..]
}

/// `s` cut to `width` characters (with `…`) and padded.
pub fn fit(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n > width {
        let cut: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

pub fn status_label(s: Status) -> &'static str {
    s.as_str()
}

fn paint(s: Status, text: &str, color: bool) -> String {
    if !color {
        return text.to_string();
    }
    let code = match s {
        Status::Working => "33",
        Status::NeedsYou => "1;31",
        Status::Idle => "32",
        Status::Ended => "2",
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

fn note(v: &TaskView) -> String {
    match (v.boot, &v.detail) {
        (Some(BootState::TrustPrompt), _) => "trust prompt: mirante resume / attach".into(),
        (Some(BootState::Booting), _) => "starting".into(),
        (_, Some(d)) => d.clone(),
        _ => String::new(),
    }
}

pub fn tasks_table(tasks: &[TaskView], color: bool) -> String {
    if tasks.is_empty() {
        return "no tasks (mirante new <repo> --agent claude|codex --branch <name> <goal>)\n".into();
    }
    let mut out = format!(
        "{}  {}  {}  {}  {}  {}  {}\n",
        fit("ID", 8),
        fit("BRANCH", 22),
        fit("AGENT", 6),
        fit("STATUS", 9),
        fit("NEEDS", 5),
        fit("Q", 2),
        "TITLE"
    );
    for v in tasks {
        let needs = if v.needs_you > 0 { v.needs_you.to_string() } else { String::new() };
        let queued = if v.queued > 0 { v.queued.to_string() } else { String::new() };
        let note = note(v);
        let title = if note.is_empty() { v.task.title.clone() } else { format!("{}  [{note}]", v.task.title) };
        out.push_str(&format!(
            "{}  {}  {}  {}  {}  {}  {}\n",
            fit(short(&v.task.id), 8),
            fit(&v.task.branch, 22),
            fit(v.task.agent.as_str(), 6),
            paint(v.task.status, &fit(status_label(v.task.status), 9), color),
            fit(&needs, 5),
            fit(&queued, 2),
            title
        ));
    }
    out
}

pub fn perms_table(perms: &[PermissionView]) -> String {
    if perms.is_empty() {
        return "no pending permission requests\n".into();
    }
    let mut out = format!("{}  {}  {}  {}\n", fit("PERM", 8), fit("TASK", 22), fit("TOOL", 10), "DETAIL");
    for p in perms {
        out.push_str(&format!(
            "{}  {}  {}  {}\n",
            fit(short(&p.request.id), 8),
            fit(&p.branch, 22),
            fit(&p.request.tool, 10),
            p.request.detail.lines().next().unwrap_or("")
        ));
    }
    out.push_str("answer once: mirante allow <perm> | mirante deny <perm>\n");
    out
}

pub fn task_detail(v: &TaskView, perms: &[PermissionRequest]) -> String {
    let t = &v.task;
    let mut out = format!(
        "{}  {}\n  id:        {}\n  agent:     {}\n  status:    {}{}\n  lifecycle: {}\n  branch:    {}\n  worktree:  {}\n  repo:      {}\n  goal:      {}\n",
        short(&t.id),
        t.title,
        t.id,
        t.agent.as_str(),
        t.status.as_str(),
        if note(v).is_empty() { String::new() } else { format!(" [{}]", note(v)) },
        t.lifecycle.as_str(),
        t.branch,
        t.worktree_path,
        t.repo_path,
        t.goal.lines().next().unwrap_or("")
    );
    out.push_str(&format!("  live:      {}  queued: {}\n", v.live, v.queued));
    for p in perms {
        out.push_str(&format!("  needs you: {} {} {}\n", short(&p.id), p.tool, p.detail));
    }
    out
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-cli`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/mirante-cli
git commit -F - <<'EOF'
feat(cli): argument parser and status/permission tables

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 12: `mirante` binary: commands, auto-start, attach, watch

**Files:**
- Modify: `crates/mirante-cli/Cargo.toml`
- Modify: `crates/mirante-cli/src/lib.rs`
- Create: `crates/mirante-cli/src/term.rs`
- Create: `crates/mirante-cli/src/daemon_ctl.rs`
- Create: `crates/mirante-cli/src/main.rs`

- **Auto-start.** Any command except `daemon status|stop` and `watch` starts the daemon when nothing listens (`mirante-daemon` next to `mirante`, or `MIRANTE_DAEMON_BIN`), in its own session (`setsid`, so closing the terminal does not kill it), logging to `~/.mirante/daemon.log`.
- **`new`** creates the task (worktree on your branch), starts the agent with the goal as its first message, and walks the boot: if Claude shows the trust dialog, the CLI prints it and asks you `[y/N]` (FINDINGS rule 5: shown to the user, never accepted silently). `--trust` is your explicit "yes" given up front.
- **`attach`** puts your terminal in raw mode and passes bytes through; Ctrl-] detaches (the agent keeps running). The PTY is resized to your terminal. For Codex it is a read-only event stream.
- **`watch`** redraws the board on every event (snapshot per event: the stream is only a hint) and rings the bell when a task newly needs you. It reconnects without restarting a stopped daemon.

No unit tests here (the logic is in the library); Task 17's end-to-end test drives this binary.

- [ ] **Step 1: Write the files**

`crates/mirante-cli/Cargo.toml`:
```toml
[package]
name = "mirante-cli"
version = "0.1.0"
edition = "2021"
publish = false
license = "Apache-2.0"

[[bin]]
name = "mirante"
path = "src/main.rs"

[dependencies]
mirante-core = { path = "../mirante-core" }
mirante-ipc = { path = "../mirante-ipc" }
serde_json = "1"
libc = "0.2"
```

`crates/mirante-cli/src/lib.rs`:
```rust
//! Library half of the `mirante` CLI (the binary is `src/main.rs`): argument parsing,
//! rendering, terminal helpers and daemon start-up.
pub mod args;
pub mod daemon_ctl;
pub mod render;
pub mod term;
```

`crates/mirante-cli/src/term.rs`:
```rust
//! Terminal helpers for `attach`: raw mode (restored on drop) and the window size.
use std::io;

pub struct RawMode {
    orig: libc::termios,
}

impl RawMode {
    pub fn enable() -> io::Result<RawMode> {
        // SAFETY: termios is plain data; tcgetattr fills it for fd 0.
        unsafe {
            let mut orig: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut orig) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = orig;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(0, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(RawMode { orig })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restores the attributes read in `enable`.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.orig);
        }
    }
}

pub fn is_tty(fd: i32) -> bool {
    // SAFETY: isatty has no preconditions.
    unsafe { libc::isatty(fd) == 1 }
}

/// (rows, cols) of the terminal on stdout, or 40x120 when unknown.
pub fn size() -> (u16, u16) {
    // SAFETY: winsize is plain data filled by the ioctl.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            (ws.ws_row, ws.ws_col)
        } else {
            (40, 120)
        }
    }
}
```

`crates/mirante-cli/src/daemon_ctl.rs`:
```rust
//! Connecting to the daemon, starting it on first use.
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mirante_ipc::{Client, Paths};

/// `MIRANTE_DAEMON_BIN`, else `mirante-daemon` next to this executable.
pub fn daemon_bin() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("MIRANTE_DAEMON_BIN").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    Ok(std::env::current_exe()?.with_file_name("mirante-daemon"))
}

/// Spawns the daemon in its own session (it survives this terminal), logging to
/// `~/.mirante/daemon.log`, and waits up to 5 s for its socket.
pub fn start_daemon(paths: &Paths) -> io::Result<Client> {
    paths.ensure_home()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(paths.log())?;
    let mut cmd = Command::new(daemon_bin()?);
    cmd.env(mirante_ipc::paths::HOME_ENV, paths.home()).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
    cmd.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Client::connect(&paths.socket()) {
            Ok(c) => return Ok(c),
            Err(e) if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("daemon did not start (see {}): {e}", paths.log().display()),
                ))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// A client, starting the daemon if nothing listens on the socket.
pub fn connect(paths: &Paths) -> io::Result<Client> {
    match Client::connect(&paths.socket()) {
        Ok(c) => Ok(c),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => start_daemon(paths),
        Err(e) => Err(e),
    }
}
```

`crates/mirante-cli/src/main.rs`:
```rust
//! `mirante`: thin client of `mirante-daemon`.
use std::collections::HashMap;
use std::error::Error;
use std::io::{self, BufRead, Read, Write};
use std::net::Shutdown;
use std::process::ExitCode;
use std::time::Duration;

use mirante_core::model::{PermissionRequest, Status};
use mirante_ipc::types::{
    method, AnswerPermission, AttachParams, AttachResult, BootState, CreateTask, DaemonStatus, ListParams,
    PermissionView, SendMessage, SendMode, SendResult, Snapshot, StartResult, StartSession, TaskRef, TaskView,
    TrustAnswer,
};
use mirante_ipc::{Client, Paths};
use serde_json::{json, Value};

use mirante_cli::args::{self, Cmd, DaemonCmd, USAGE};
use mirante_cli::{daemon_ctl, render, term};

type Res = Result<(), Box<dyn Error>>;

/// Ctrl-] detaches from `attach`.
const DETACH: u8 = 0x1d;

fn client(paths: &Paths) -> Result<Client, Box<dyn Error>> {
    let c = daemon_ctl::connect(paths)?;
    // `session.start` may wait up to 45 s for the agent to boot.
    c.set_timeout(Some(Duration::from_secs(90)))?;
    Ok(c)
}

fn ask(question: &str) -> io::Result<bool> {
    print!("{question}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Walks the start result: asks the user about Claude's trust dialog (unless `--trust`
/// said yes up front), reports how the boot ended.
fn boot_flow(c: &mut Client, mut r: StartResult, trust: bool) -> Res {
    let id = render::short(&r.task).to_string();
    loop {
        match r.boot {
            BootState::Ready => {
                println!("{id} ready{}. attach: mirante attach {id}", if r.resumed { " (resumed)" } else { "" });
                return Ok(());
            }
            BootState::Booting => {
                println!("{id} is still starting; check `mirante ls`");
                return Ok(());
            }
            BootState::Exited => return Err(format!("the agent exited during startup:\n{}", r.screen).into()),
            BootState::TrustPrompt => {
                println!("Claude asks whether to trust the worktree folder:\n\n{}\n", r.screen);
                let accept = trust || ask("Trust this folder and continue? [y/N] ")?;
                r = c.call(method::SESSION_TRUST, TrustAnswer { task: r.task.clone(), accept })?;
                if !accept {
                    println!("declined: the session ended (mirante resume {id} to try again)");
                    return Ok(());
                }
            }
        }
    }
}

fn attach(paths: &Paths, task: &str) -> Res {
    if !term::is_tty(0) || !term::is_tty(1) {
        return Err("attach needs a terminal".into());
    }
    let (rows, cols) = term::size();
    let mut c = client(paths)?;
    let r: AttachResult = c.call(method::ATTACH, AttachParams { task: task.into(), rows, cols })?;
    c.set_timeout(None)?;
    eprintln!("attached{}; detach with Ctrl-]", if r.input { "" } else { " (read-only stream)" });
    let (mut reader, writer) = c.into_raw();
    let raw = term::RawMode::enable()?;
    let mut w = writer.try_clone()?;
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = [0u8; 1024];
        loop {
            let n = match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let (chunk, detach) = match buf[..n].iter().position(|b| *b == DETACH) {
                Some(i) => (&buf[..i], true),
                None => (&buf[..n], false),
            };
            if r.input && !chunk.is_empty() && w.write_all(chunk).is_err() {
                break;
            }
            if detach {
                break;
            }
        }
        let _ = w.shutdown(Shutdown::Both);
    });
    let mut out = io::stdout().lock();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                out.write_all(&buf[..n])?;
                out.flush()?;
            }
        }
    }
    drop(raw);
    // Reset attributes and show the cursor in case the agent's TUI left them changed.
    print!("\x1b[0m\x1b[?25h\r\ndetached\r\n");
    Ok(())
}

fn board(tasks: &[TaskView], seen: &mut HashMap<String, Status>, color: bool) {
    // Bell only when a task newly needs the user.
    let bell = tasks
        .iter()
        .any(|v| v.task.status == Status::NeedsYou && seen.get(&v.task.id).is_some_and(|s| *s != Status::NeedsYou));
    seen.clear();
    seen.extend(tasks.iter().map(|v| (v.task.id.clone(), v.task.status)));
    print!("\x1b[2J\x1b[H");
    println!("mirante watch  (Ctrl-C to quit)\n");
    print!("{}", render::tasks_table(tasks, color));
    if bell {
        print!("\x07");
    }
    let _ = io::stdout().flush();
}

fn watch(paths: &Paths) -> Res {
    let color = term::is_tty(1);
    let mut seen = HashMap::new();
    loop {
        // No auto-start here: after `mirante daemon stop` the board waits, it does not
        // bring the daemon back.
        let session = (|| -> Result<(), Box<dyn Error>> {
            let mut sub = Client::connect(&paths.socket())?;
            let snap: Snapshot = sub.call(method::EVENTS_SUBSCRIBE, json!({}))?;
            board(&snap.tasks, &mut seen, color);
            let mut q = Client::connect(&paths.socket())?;
            q.set_timeout(Some(Duration::from_secs(10)))?;
            // The stream is only a hint: every event triggers a fresh snapshot.
            while sub.next_notification()?.is_some() {
                let tasks: Vec<TaskView> = q.call(method::TASK_LIST, ListParams::default())?;
                board(&tasks, &mut seen, color);
            }
            Ok(())
        })();
        if let Err(e) = session {
            eprintln!("mirante watch: {e}; reconnecting");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn run(cmd: Cmd) -> Res {
    let paths = Paths::from_env()?;
    let color = term::is_tty(1);
    match cmd {
        Cmd::Help => println!("{USAGE}"),
        Cmd::Daemon(DaemonCmd::Status) => match Client::connect(&paths.socket()) {
            Ok(mut c) => {
                let s: DaemonStatus = c.call(method::DAEMON_STATUS, json!({}))?;
                println!(
                    "running: pid {} boot {} live sessions {} (v{})",
                    s.pid, s.boot_id, s.live_sessions, s.version
                );
            }
            Err(_) => println!("not running"),
        },
        Cmd::Daemon(DaemonCmd::Start) => {
            if Client::connect(&paths.socket()).is_ok() {
                println!("already running");
            } else {
                daemon_ctl::start_daemon(&paths)?;
                println!("started");
            }
        }
        Cmd::Daemon(DaemonCmd::Stop) => match Client::connect(&paths.socket()) {
            Ok(mut c) => {
                c.set_timeout(Some(Duration::from_secs(60)))?;
                let _: Value = c.call(method::DAEMON_SHUTDOWN, json!({}))?;
                println!("stopped (sessions ended; mirante resume <task> restores them)");
            }
            Err(_) => println!("not running"),
        },
        Cmd::New { repo, agent, branch, base, title, goal, trust } => {
            let repo = std::fs::canonicalize(&repo).map_err(|e| format!("repo {repo}: {e}"))?;
            let mut c = client(&paths)?;
            let v: TaskView = c.call(
                method::TASK_CREATE,
                CreateTask {
                    repo: repo.to_string_lossy().into_owned(),
                    agent,
                    branch,
                    base,
                    goal: goal.clone(),
                    title,
                },
            )?;
            println!("created {} on {} ({})", render::short(&v.task.id), v.task.branch, v.task.worktree_path);
            let r: StartResult =
                c.call(method::SESSION_START, StartSession { task: v.task.id.clone(), prompt: Some(goal) })?;
            boot_flow(&mut c, r, trust)?;
        }
        Cmd::Ls { all } => {
            let tasks: Vec<TaskView> = client(&paths)?.call(method::TASK_LIST, ListParams { archived: all })?;
            print!("{}", render::tasks_table(&tasks, color));
        }
        Cmd::Show { task } => {
            let mut c = client(&paths)?;
            let v: TaskView = c.call(method::TASK_GET, TaskRef { task })?;
            let perms: Vec<PermissionView> = c.call(method::PERMISSION_LIST, json!({}))?;
            let mine: Vec<PermissionRequest> =
                perms.into_iter().filter(|p| p.request.task_id == v.task.id).map(|p| p.request).collect();
            print!("{}", render::task_detail(&v, &mine));
        }
        Cmd::Watch => watch(&paths)?,
        Cmd::Attach { task } => attach(&paths, &task)?,
        Cmd::Say { task, text, queue } => {
            let mode = if queue { SendMode::Queue } else { SendMode::Steer };
            let r: SendResult = client(&paths)?.call(method::MESSAGE_SEND, SendMessage { task, text, mode })?;
            match r.delivered {
                SendMode::Steer => println!("sent now"),
                SendMode::Queue => println!("queued (#{} in line, delivered when the turn ends)", r.queued),
            }
        }
        Cmd::Perms => {
            let perms: Vec<PermissionView> = client(&paths)?.call(method::PERMISSION_LIST, json!({}))?;
            print!("{}", render::perms_table(&perms));
        }
        Cmd::Answer { id, allow } => {
            let p: PermissionRequest =
                client(&paths)?.call(method::PERMISSION_ANSWER, AnswerPermission { id, allow })?;
            println!("{} {}: {}", render::short(&p.id), p.tool, p.status.as_str());
        }
        Cmd::Interrupt { task } => {
            let _: Value = client(&paths)?.call(method::SESSION_INTERRUPT, TaskRef { task })?;
            println!("interrupt sent");
        }
        Cmd::Stop { task } => {
            let v: TaskView = client(&paths)?.call(method::SESSION_STOP, TaskRef { task })?;
            println!("{} {}", render::short(&v.task.id), v.task.status.as_str());
        }
        Cmd::Resume { task, trust } => {
            let mut c = client(&paths)?;
            let r: StartResult = c.call(method::SESSION_RESUME, StartSession { task, prompt: None })?;
            if !r.resumed {
                println!("note: no native session id was recorded; this is a fresh conversation");
            }
            boot_flow(&mut c, r, trust)?;
        }
        Cmd::Archive { task } => {
            let v: TaskView = client(&paths)?.call(method::TASK_ARCHIVE, TaskRef { task })?;
            println!("{} archived (branch {} kept)", render::short(&v.task.id), v.task.branch);
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args::parse(&args) {
        Err(e) => {
            eprintln!("mirante: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Ok(cmd) => match run(cmd) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mirante: {e}");
                ExitCode::FAILURE
            }
        },
    }
}
```

- [ ] **Step 2: Build and run the tests**

Run: `cargo test -p mirante-cli`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 3: Smoke-test the CLI against a temp home**

```bash
cargo build --workspace
export MIRANTE_HOME=/tmp/mr-cli; rm -rf $MIRANTE_HOME
R=/tmp/mr-cli-repo; rm -rf $R; mkdir -p $R && git -C $R init -q -b trunk && git -C $R -c user.email=t@e -c user.name=T commit -q --allow-empty -m init
./target/debug/mirante daemon status
./target/debug/mirante new $R --agent codex --branch smoke "hello"; echo "exit $?"
./target/debug/mirante ls
./target/debug/mirante daemon stop
rm -rf $MIRANTE_HOME $R; unset MIRANTE_HOME
```

Expected (ids, pids and boot ids differ):
```
not running
created <8-char id> on smoke (/tmp/mr-cli/worktrees/mr-cli-repo/smoke)
mirante: not supported by this agent: the Codex driver is not built yet (code 3)
exit 1
ID        BRANCH                  AGENT   STATUS     NEEDS  Q   TITLE
<id>      smoke                   codex   ended                 hello
stopped (sessions ended; mirante resume <task> restores them)
```

- [ ] **Step 4: Commit**

```bash
git add crates/mirante-cli
git commit -F - <<'EOF'
feat(cli): mirante binary with daemon auto-start, attach passthrough and watch board

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

## Phase 2b-ii — Claude driver

### Task 13: Screen reading and the PTY process

**Files:**
- Modify: `crates/mirante-daemon/src/driver/mod.rs`
- Create: `crates/mirante-daemon/src/driver/screen.rs`
- Create: `crates/mirante-daemon/src/driver/pty.rs`

`screen.rs` ports the proven dialog matching from `spike_kit::screen` (fixtures are real Claude Code 2.1.285 grids) and adds the trust-dialog, ready (`manual mode on` footer: Mirante always launches with `--permission-mode manual`) and request-matching checks. FINDINGS rule 6: only the exact labels "Yes" / "No" match, only below the live dialog header; "Yes, and always allow …" and "Yes, and switch to auto mode …" can never be chosen. The screen is used only to act on dialogs; status never comes from it (spec §2.2).

`pty.rs` runs a process in a PTY mirrored into a `vt100` grid. Proven in the spikes: the TUI stalls unless cursor-position queries (`ESC[6n`) are answered; the PTY answers them with the real cursor position while no client is attached (an attached terminal answers on its own; answering twice would inject junk input). Output fans out to attached clients; when output ends every attached stream is closed. Dropping the `PtyProcess` kills and reaps the process.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/driver/mod.rs`:
```rust
//! The agent driver interface. Every agent sits behind [`Driver`]; an operation the agent
//! cannot do is a typed [`DriverError::Unsupported`], never a silent failure. Drivers are
//! non-blocking (typing and protocol calls run on the driver's own threads) except
//! [`Driver::stop`], so the hub may call them while holding its lock.
//!
//! Drivers report back through a [`Sink`] (a channel drained by the hub's pump thread),
//! never by calling the hub directly, so a driver thread can never deadlock on the hub.
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc};

use mirante_core::model::{Status, Task};
use mirante_ipc::types::BootState;
use mirante_ipc::Paths;
use serde_json::Value;

#[cfg(test)]
pub mod fake;
pub mod pty;
pub mod real;
pub mod screen;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The agent cannot do this (e.g. a trust prompt on Codex).
    Unsupported(&'static str),
    /// The dialog the operation targets is not on screen / not pending.
    NoDialog(String),
    Failed(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Unsupported(what) => write!(f, "not supported by this agent: {what}"),
            DriverError::NoDialog(m) => write!(f, "no matching dialog: {m}"),
            DriverError::Failed(m) => write!(f, "agent driver failed: {m}"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Failed(e.to_string())
    }
}

/// What a driver tells the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// Claude: one main-transcript line, for the hub's `StatusEngine`.
    Transcript(Value),
    /// Claude: where the transcript tailer is (persisted, not audited).
    TranscriptOffset {
        path: String,
        offset: u64,
    },
    /// Codex: a status computed by the driver's own engine. `turn_open` drives the turn
    /// records; `detail` is shown next to the status.
    Status {
        to: Status,
        cause: String,
        turn_open: bool,
        detail: Option<String>,
    },
    /// Codex: an approval request. `key` is what `answer_permission` gets back.
    PermissionRequested {
        key: String,
        tool: String,
        detail: String,
    },
    /// Codex: the thread id, used for `thread/resume`.
    NativeId(String),
    Boot(BootState),
    /// A step for the feed and audit trail (command finished, agent message, ...).
    Step(Value),
    Exited {
        code: Option<i32>,
    },
}

/// Channel end a driver emits into. Every event is tagged with the driver's session id so
/// late events from an ended session are ignored.
#[derive(Clone)]
pub struct Sink {
    session_id: String,
    tx: mpsc::Sender<(String, DriverEvent)>,
}

impl Sink {
    pub fn new(session_id: &str, tx: mpsc::Sender<(String, DriverEvent)>) -> Sink {
        Sink { session_id: session_id.to_string(), tx }
    }

    pub fn emit(&self, event: DriverEvent) {
        let _ = self.tx.send((self.session_id.clone(), event));
    }
}

/// A live view of the agent's terminal (Claude) or event stream (Codex).
pub struct Attachment {
    pub id: u64,
    /// Bytes that repaint the current screen (or recent stream) before live output.
    pub initial: Vec<u8>,
    pub output: mpsc::Receiver<Vec<u8>>,
    /// Whether typed bytes reach the agent (`Driver::input`).
    pub input: bool,
}

/// Output chunks queued per attached client. A client that falls this far behind stops
/// getting chunks until it is sent a repaint (memory stays bounded whatever it does).
pub const ATTACH_QUEUE: usize = 256;

struct FanoutSub {
    tx: mpsc::SyncSender<Vec<u8>>,
    /// Its queue overflowed: chunks were dropped, so live bytes would corrupt its screen.
    /// Nothing more is sent until [`Fanout::repaint`] delivers a full screen.
    needs_repaint: bool,
}

/// Fan-out of output bytes to attached clients over bounded queues; dead receivers are
/// dropped on send. The owner of the screen calls [`Fanout::repaint`] whenever
/// [`Fanout::needs_repaint`] is true (e.g. right after `send`).
pub struct Fanout {
    next: u64,
    cap: usize,
    subs: HashMap<u64, FanoutSub>,
}

impl Default for Fanout {
    fn default() -> Self {
        Fanout::with_capacity(ATTACH_QUEUE)
    }
}

impl Fanout {
    /// `cap` chunks queued per client (at least 1).
    pub fn with_capacity(cap: usize) -> Fanout {
        Fanout { next: 0, cap: cap.max(1), subs: HashMap::new() }
    }

    pub fn subscribe(&mut self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        self.next += 1;
        let (tx, rx) = mpsc::sync_channel(self.cap);
        self.subs.insert(self.next, FanoutSub { tx, needs_repaint: false });
        (self.next, rx)
    }

    pub fn remove(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    /// Never blocks: a full queue marks that client for a repaint and drops the chunk.
    pub fn send(&mut self, bytes: &[u8]) {
        self.subs.retain(|_, s| {
            if s.needs_repaint {
                return true;
            }
            match s.tx.try_send(bytes.to_vec()) {
                Ok(()) => true,
                Err(mpsc::TrySendError::Full(_)) => {
                    s.needs_repaint = true;
                    true
                }
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Some client dropped chunks and waits for a full screen.
    pub fn needs_repaint(&self) -> bool {
        self.subs.values().any(|s| s.needs_repaint)
    }

    /// Sends `screen` (bytes that redraw the whole screen) to every client marked by
    /// [`Fanout::send`] whose queue has room again, and resumes live bytes for them.
    pub fn repaint(&mut self, screen: &[u8]) {
        self.subs.retain(|_, s| {
            if !s.needs_repaint {
                return true;
            }
            match s.tx.try_send(screen.to_vec()) {
                Ok(()) => {
                    s.needs_repaint = false;
                    true
                }
                Err(mpsc::TrySendError::Full(_)) => true,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }
}

pub trait Driver: Send + Sync {
    /// Delivers a message now: types it into the TUI (Claude) or steers / starts a turn
    /// (Codex). Returns once the message is handed to the driver's worker.
    fn send(&self, text: &str) -> Result<(), DriverError>;
    /// Esc (Claude) or `turn/interrupt` (Codex).
    fn interrupt(&self) -> Result<(), DriverError>;
    /// Answers the permission dialog / approval request named by `key` (Codex: the request
    /// id; Claude: the request detail, checked against the dialog on screen). Selects only
    /// a one-time "Yes" or "No", never an "always" or "auto" option.
    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError>;
    /// Answers Claude's workspace-trust dialog. `false` ends the session.
    fn trust(&self, accept: bool) -> Result<(), DriverError>;
    /// True while a dialog would capture typed text (a message must not be typed then).
    fn dialog_open(&self) -> bool;
    /// Claude: start (or switch) tailing the session transcript at `offset`.
    fn tail_transcript(&self, path: &Path, offset: u64);
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError>;
    fn input(&self, attachment: u64, bytes: &[u8]) -> Result<(), DriverError>;
    fn detach(&self, attachment: u64);
    /// The attached terminal changed size (Claude: resize the PTY; the agent redraws).
    fn resize(&self, _rows: u16, _cols: u16) -> Result<(), DriverError> {
        Err(DriverError::Unsupported("resize"))
    }
    /// Last lines of the screen (Claude) or stream (Codex), for the CLI.
    fn screen_tail(&self) -> String;
    /// Ends the agent gracefully, then kills it. Blocks until the process is gone; the
    /// driver still emits `Exited`.
    fn stop(&self);
}

pub struct SpawnRequest {
    pub task: Task,
    pub session_id: String,
    /// Native id to resume (Claude session id / Codex thread id).
    pub resume: Option<String>,
    pub paths: Paths,
}

pub trait DriverFactory: Send + Sync {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_drops_dead_receivers() {
        let mut f = Fanout::default();
        let (a, rx_a) = f.subscribe();
        let (_b, rx_b) = f.subscribe();
        f.send(b"x");
        assert_eq!(rx_a.recv().unwrap(), b"x");
        drop(rx_b);
        f.send(b"y");
        assert_eq!(rx_a.recv().unwrap(), b"y");
        f.remove(a);
        assert!(f.is_empty());
    }

    #[test]
    fn a_stalled_client_queue_is_bounded_and_resumes_after_a_repaint() {
        let mut f = Fanout::with_capacity(2);
        let (_id, rx) = f.subscribe();
        for b in [b"1", b"2", b"3", b"4"] {
            f.send(b);
        }
        assert!(f.needs_repaint());
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"1".to_vec(), b"2".to_vec()]);
        // Room again, but live bytes stay held back until the full screen is sent.
        f.send(b"5");
        assert!(rx.try_recv().is_err());
        f.repaint(b"SCREEN");
        assert!(!f.needs_repaint());
        f.send(b"6");
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"SCREEN".to_vec(), b"6".to_vec()]);
    }

    #[test]
    fn sink_tags_events_with_the_session() {
        let (tx, rx) = mpsc::channel();
        Sink::new("s1", tx).emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(rx.recv().unwrap(), ("s1".to_string(), DriverEvent::Boot(BootState::Ready)));
    }
}
```

`crates/mirante-daemon/src/driver/screen.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Real grid excerpts (vt100-rendered, Claude Code 2.1.285, S3 run 1).
    const DIALOG: &str = "\
❯ Use o Bash para rodar exatamente touch s3-allowed.txt e diga se deu certo.
  Creating empty file s3-allowed.txt
  ⎿  $ touch s3-allowed.txt
 Bash command
   touch s3-allowed.txt
   Create empty file s3-allowed.txt
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /home/u/s3 from this project
   3. Yes, and switch to auto mode · auto mode handles these prompts for you
   4. No
 Esc to cancel · Tab to amend";
    const IDLE: &str = "\
❯ Use o Bash para rodar exatamente touch s3-denied.txt e diga se deu certo.
  ⎿  Interrupted · What should Claude do instead?
✻ Brewed for 1s · done 1:47 PM
❯
  ⏸ manual mode on · ← for agents";
    const EDIT_DIALOG: &str = "\
 Edit file
   src/main.rs
 Do you want to make this edit to main.rs?
 ❯ 1. Yes
   2. Yes, allow all edits during this session
   3. No
 Esc to cancel";

    #[test]
    fn exact_one_time_options_only() {
        assert_eq!(option_key(DIALOG, "Yes"), Some('1'));
        assert_eq!(option_key(DIALOG, "No"), Some('4'));
        assert_eq!(option_key(EDIT_DIALOG, "No"), Some('3'));
        assert_eq!(option_key(IDLE, "Yes"), None);
    }

    #[test]
    fn lookalike_options_above_the_dialog_are_ignored() {
        let screen = format!("1. Yes\n4. No\n{EDIT_DIALOG}");
        assert_eq!(option_key(&screen, "No"), Some('3'));
        assert_eq!(option_key("1. Yes\n4. No\nno dialog here", "Yes"), None);
    }

    #[test]
    fn dialogs_and_readiness() {
        assert!(has_permission_prompt(DIALOG) && has_permission_prompt(EDIT_DIALOG));
        assert!(!has_permission_prompt(IDLE));
        assert!(!has_permission_prompt("Do you want to proceed?\nsure"));
        assert!(is_ready(IDLE));
        assert!(!is_ready("Do you trust the files in this folder?\n❯ 1. No, exit\n  2. Yes, I trust this folder"));
        assert!(is_trust_dialog("  2. Yes, I trust this folder"));
    }

    #[test]
    fn dialog_matches_the_request_detail() {
        assert!(dialog_matches(DIALOG, "touch s3-allowed.txt"));
        assert!(!dialog_matches(DIALOG, "rm -rf /"));
        assert!(dialog_matches(EDIT_DIALOG, "/Users/u/.mirante/worktrees/app/feat/src/main.rs"));
        assert!(dialog_matches(DIALOG, ""));
        assert!(!dialog_matches(IDLE, "touch s3-denied.txt"));
        let wrapped = "Bash command\n  cargo test --workspace --all-\n  features -- --nocapture\nDo you want to proceed?\n❯ 1. Yes\n  2. No";
        assert!(dialog_matches(wrapped, "cargo test --workspace --all-features -- --nocapture"));
    }

    #[test]
    fn tail_keeps_the_last_non_blank_lines() {
        assert_eq!(tail("a\n\nb\n  \nc\n", 2), "b\nc");
    }
}
```

`crates/mirante-daemon/src/driver/pty.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sh(script: &str) -> PtySpec {
        PtySpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env_remove: vec![],
            env_set: vec![("TERM".into(), "xterm-256color".into())],
            rows: 24,
            cols: 80,
        }
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn typed_input_reaches_the_process_and_the_grid_mirrors_output() {
        let (tx, rx) = mpsc::channel();
        let p = PtyProcess::spawn(&sh("read line; echo \"got:$line\""), move |c| tx.send(c).unwrap()).unwrap();
        p.write(b"hello\r").unwrap();
        wait_for("echo", || p.grid().contains("got:hello"));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Some(0));
        wait_for("exited flag", || p.exited());
    }

    #[test]
    fn cursor_position_queries_are_answered_while_detached() {
        // `stty raw -echo` so the reply reaches `dd` byte for byte.
        let p = PtyProcess::spawn(
            &sh("stty raw -echo; printf 'ab\\033[6n'; r=$(dd bs=1 count=6 2>/dev/null); printf '\\r\\nreply:%s' \"$r\" | tr '\\033' 'E'"),
            |_| {},
        )
        .unwrap();
        wait_for("reply", || p.grid().contains("reply:E[1;3R"));
    }

    #[test]
    fn attached_clients_get_live_output_and_eof_when_the_process_ends() {
        let p = PtyProcess::spawn(&sh("read x; echo \"bye:$x\""), |_| {}).unwrap();
        let (_, rx) = p.subscribe();
        p.write(b"q\r").unwrap();
        let mut seen = Vec::new();
        while let Ok(chunk) = rx.recv_timeout(Duration::from_secs(5)) {
            seen.extend(chunk);
        }
        assert!(String::from_utf8_lossy(&seen).contains("bye:q"));
    }

    #[test]
    fn dropping_the_process_kills_it_and_resize_reaches_the_grid() {
        let p = PtyProcess::spawn(&sh("sleep 30"), |_| {}).unwrap();
        p.resize(30, 100).unwrap();
        assert_eq!(p.parser.lock().unwrap().screen().size(), (nz(30), nz(100)));
        let started = Instant::now();
        drop(p);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error[E0425]: cannot find type `PtySpec` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/driver/screen.rs`:
```rust
//! Reading Claude Code's TUI (2.1.285) off the emulated screen. Used ONLY for UI actions
//! (which key answers this dialog, is a dialog in the way of typed text, is the trust
//! prompt up, is the input box ready). Status never comes from here: it comes from hooks
//! and the transcript (`mirante_core::status`). Dialog matching ported from
//! `spike_kit::screen`, whose fixtures are real 2.1.285 grids.

/// Index of the last permission dialog: a `Do you want to …?` line followed later by a
/// numbered `1.` option line.
fn dialog_start(lines: &[&str]) -> Option<usize> {
    let is_option_one = |l: &str| l.trim_start().trim_start_matches('❯').trim_start().starts_with("1. ");
    (0..lines.len()).rev().find(|&i| {
        let t = lines[i].trim();
        t.starts_with("Do you want to") && t.ends_with('?') && lines[i + 1..].iter().any(|l| is_option_one(l))
    })
}

pub fn has_permission_prompt(grid: &str) -> bool {
    let lines: Vec<&str> = grid.lines().collect();
    dialog_start(&lines).is_some()
}

/// The digit of the live dialog's option whose label is exactly `label` ("Yes" / "No").
/// Only lines below the dialog header count, and only an exact label matches, so
/// "Yes, and always allow …" and "Yes, and switch to auto mode …" are never chosen.
pub fn option_key(grid: &str, label: &str) -> Option<char> {
    let lines: Vec<&str> = grid.lines().collect();
    let start = dialog_start(&lines)?;
    lines[start + 1..].iter().find_map(|l| {
        let t = l.trim().trim_start_matches('❯').trim();
        let (num, rest) = t.split_once(". ")?;
        let digit = num.parse::<u8>().ok().filter(|d| *d < 10)?;
        (rest.trim() == label).then(|| char::from(b'0' + digit))
    })
}

/// Claude's workspace-trust dialog (new folder). Its default option is "No, exit".
pub fn is_trust_dialog(grid: &str) -> bool {
    grid.contains("trust this folder") || grid.contains("Do you trust")
}

/// The input box is up: Mirante always launches with `--permission-mode manual`, whose
/// footer reads "manual mode on".
pub fn is_ready(grid: &str) -> bool {
    !is_trust_dialog(grid) && grid.contains("manual mode on")
}

/// Does the dialog on screen concern `detail` (a permission request's command or path)?
/// Compared without whitespace, so a command wrapped across lines still matches: the
/// first 30 characters of the detail, or, for an absolute path, its file name (the dialog
/// shows paths relative to the project). An empty detail matches any dialog.
pub fn dialog_matches(grid: &str, detail: &str) -> bool {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let detail = detail.lines().next().unwrap_or("").trim();
    if detail.is_empty() {
        return true;
    }
    let lines: Vec<&str> = grid.lines().collect();
    let Some(start) = dialog_start(&lines) else {
        return false;
    };
    // The tool box sits right above the question: search the last 15 lines before it.
    let from = start.saturating_sub(15);
    let hay = squash(&lines[from..=start].join("\n"));
    let head: String = squash(detail).chars().take(30).collect();
    let name = detail.starts_with('/').then(|| detail.rsplit('/').next().unwrap_or("")).filter(|n| !n.is_empty());
    hay.contains(&head) || name.is_some_and(|n| hay.contains(&squash(n)))
}

/// Last `n` non-blank lines.
pub fn tail(grid: &str, n: usize) -> String {
    let lines: Vec<&str> = grid.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
```

Add above the test module in `crates/mirante-daemon/src/driver/pty.rs`:
```rust
//! An agent process in a pseudo-terminal, mirrored into a vt100 grid. The reader thread
//! answers cursor-position queries (`ESC[6n`) while no client is attached (the TUI stalls
//! without an answer; an attached terminal answers on its own), and fans output out to
//! attached clients. The process is killed when the `PtyProcess` is dropped.
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use super::Fanout;

pub const CPR_QUERY: &[u8] = b"\x1b[6n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtySpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env_remove: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub rows: u16,
    pub cols: u16,
}

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct PtyProcess {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Writer,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    parser: Arc<Mutex<vt100::Parser>>,
    fanout: Arc<Mutex<Fanout>>,
    exited: Arc<AtomicBool>,
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

fn nz(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n.max(1)).expect("max(1) is non-zero")
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl PtyProcess {
    /// Spawns `spec`; `on_exit` runs once, with the exit code, when the process ends.
    pub fn spawn(spec: &PtySpec, on_exit: impl FnOnce(Option<i32>) + Send + 'static) -> io::Result<Arc<PtyProcess>> {
        let pair = native_pty_system().openpty(size(spec.rows, spec.cols)).map_err(other)?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        for k in &spec.env_remove {
            cmd.env_remove(k);
        }
        for (k, v) in &spec.env_set {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).map_err(other)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(other)?;
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(other)?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(nz(spec.rows), nz(spec.cols), 0)));
        let fanout = Arc::new(Mutex::new(Fanout::default()));
        let p = Arc::new(PtyProcess {
            master: Mutex::new(pair.master),
            writer: writer.clone(),
            child: Mutex::new(child),
            parser: parser.clone(),
            fanout: fanout.clone(),
            exited: Arc::new(AtomicBool::new(false)),
        });
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let chunk = &buf[..n];
                let cursor = {
                    let mut parser = lock(&parser);
                    parser.process(chunk);
                    parser.screen().cursor_position()
                };
                let mut f = lock(&fanout);
                if f.is_empty() && chunk.windows(CPR_QUERY.len()).any(|w| w == CPR_QUERY) {
                    let answer = format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1);
                    let mut w = lock(&writer);
                    let _ = w.write_all(answer.as_bytes()).and_then(|()| w.flush());
                }
                f.send(chunk);
                if f.needs_repaint() {
                    // A client fell behind and lost chunks (bounded queue): send it the
                    // whole screen, after which it gets live bytes again.
                    let mut screen = b"\x1b[2J\x1b[H".to_vec();
                    screen.extend_from_slice(lock(&parser).screen().contents_formatted().as_bytes());
                    f.repaint(&screen);
                }
            }
            // End of output: close every attached stream.
            *lock(&fanout) = Fanout::default();
        });
        let weak: Weak<PtyProcess> = Arc::downgrade(&p);
        let exited = p.exited.clone();
        std::thread::spawn(move || {
            let code = loop {
                let Some(p) = weak.upgrade() else {
                    return;
                };
                let status = lock(&p.child).try_wait();
                drop(p);
                match status {
                    Ok(Some(s)) => break Some(s.exit_code() as i32),
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(_) => break None,
                }
            };
            exited.store(true, Ordering::SeqCst);
            on_exit(code);
        });
        Ok(p)
    }

    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut w = lock(&self.writer);
        w.write_all(bytes)?;
        w.flush()
    }

    /// The screen as plain text, one line per row.
    pub fn grid(&self) -> String {
        lock(&self.parser).screen().contents()
    }

    /// Escape sequences that repaint the current screen on a fresh terminal.
    pub fn repaint(&self) -> Vec<u8> {
        let mut out = b"\x1b[2J\x1b[H".to_vec();
        out.extend_from_slice(lock(&self.parser).screen().contents_formatted().as_bytes());
        out
    }

    /// Resizes the PTY (the agent gets SIGWINCH and redraws) and the mirror grid.
    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        lock(&self.master).resize(size(rows, cols)).map_err(other)?;
        lock(&self.parser).set_size(nz(rows), nz(cols));
        Ok(())
    }

    pub fn subscribe(&self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        lock(&self.fanout).subscribe()
    }

    pub fn unsubscribe(&self, id: u64) {
        lock(&self.fanout).remove(id);
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    pub fn kill(&self) {
        let _ = lock(&self.child).kill();
    }

    /// Waits up to `timeout` for the process to exit on its own.
    pub fn wait_exit(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if matches!(lock(&self.child).try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        let mut child = lock(&self.child);
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 50 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): PTY process with vt100 mirror, CPR answers, fan-out; dialog matching

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 14: Claude driver and its launch

**Files:**
- Modify: `crates/mirante-daemon/src/driver/mod.rs`
- Create: `crates/mirante-daemon/src/driver/claude.rs`
- Modify: `crates/mirante-daemon/src/driver/real.rs`

The unmodified interactive `claude` in a PTY (FINDINGS: the mode that fits the terms, steers mid-run and resumes), launched from Plan 2a's `claude_launch::launch_spec`: the real binary (`MIRANTE_CLAUDE_BIN` or PATH without shims), nested-session and cmux variables scrubbed (auth never touched), `--permission-mode manual`, Mirante's hooks added through `--settings` (a 0600 file under `~/.mirante/sessions/<task>/`), `--resume <native id>` on resume, `MIRANTE_TASK_ID` and `MIRANTE_HOME` set so the hook reaches this daemon. Never `--dangerously-skip-permissions`; the allowlist is empty.

- **Boot.** A watcher reads the grid: the trust dialog → `Boot(TrustPrompt)` (once); the input footer with no dialog → `Boot(Ready)`. There is no time-based "ready" fallback: an unknown startup dialog keeps the session `Booting` (the user attaches) instead of receiving typed text.
- **Trust.** Accept = Down, pause, Enter (the default option is "No, exit"; Week-1 proven). Decline = kill.
- **Messages** are typed by a typist thread: one line (newlines collapsed: Enter would submit early), 400 ms pause, Enter. Refused while a dialog is on screen.
- **Interrupt** = Esc. **Permission answers** type the digit of the exact option, only if the dialog on screen matches the request's detail (first 30 non-space characters, or the file name of an absolute path).
- **Transcript tail.** One thread per transcript path (switches on `/clear`), polling `TranscriptTail` and looping while `more` (never while offset < length), emitting lines then the new offset.
- **Stop** = Esc, `/exit`, 3 s grace, kill. Dropping the driver kills the process.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/driver/mod.rs`:
```rust
//! The agent driver interface. Every agent sits behind [`Driver`]; an operation the agent
//! cannot do is a typed [`DriverError::Unsupported`], never a silent failure. Drivers are
//! non-blocking (typing and protocol calls run on the driver's own threads) except
//! [`Driver::stop`], so the hub may call them while holding its lock.
//!
//! Drivers report back through a [`Sink`] (a channel drained by the hub's pump thread),
//! never by calling the hub directly, so a driver thread can never deadlock on the hub.
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc};

use mirante_core::model::{Status, Task};
use mirante_ipc::types::BootState;
use mirante_ipc::Paths;
use serde_json::Value;

pub mod claude;
#[cfg(test)]
pub mod fake;
pub mod pty;
pub mod real;
pub mod screen;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The agent cannot do this (e.g. a trust prompt on Codex).
    Unsupported(&'static str),
    /// The dialog the operation targets is not on screen / not pending.
    NoDialog(String),
    Failed(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Unsupported(what) => write!(f, "not supported by this agent: {what}"),
            DriverError::NoDialog(m) => write!(f, "no matching dialog: {m}"),
            DriverError::Failed(m) => write!(f, "agent driver failed: {m}"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Failed(e.to_string())
    }
}

/// What a driver tells the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// Claude: one main-transcript line, for the hub's `StatusEngine`.
    Transcript(Value),
    /// Claude: where the transcript tailer is (persisted, not audited).
    TranscriptOffset {
        path: String,
        offset: u64,
    },
    /// Codex: a status computed by the driver's own engine. `turn_open` drives the turn
    /// records; `detail` is shown next to the status.
    Status {
        to: Status,
        cause: String,
        turn_open: bool,
        detail: Option<String>,
    },
    /// Codex: an approval request. `key` is what `answer_permission` gets back.
    PermissionRequested {
        key: String,
        tool: String,
        detail: String,
    },
    /// Codex: the thread id, used for `thread/resume`.
    NativeId(String),
    Boot(BootState),
    /// A step for the feed and audit trail (command finished, agent message, ...).
    Step(Value),
    Exited {
        code: Option<i32>,
    },
}

/// Channel end a driver emits into. Every event is tagged with the driver's session id so
/// late events from an ended session are ignored.
#[derive(Clone)]
pub struct Sink {
    session_id: String,
    tx: mpsc::Sender<(String, DriverEvent)>,
}

impl Sink {
    pub fn new(session_id: &str, tx: mpsc::Sender<(String, DriverEvent)>) -> Sink {
        Sink { session_id: session_id.to_string(), tx }
    }

    pub fn emit(&self, event: DriverEvent) {
        let _ = self.tx.send((self.session_id.clone(), event));
    }
}

/// A live view of the agent's terminal (Claude) or event stream (Codex).
pub struct Attachment {
    pub id: u64,
    /// Bytes that repaint the current screen (or recent stream) before live output.
    pub initial: Vec<u8>,
    pub output: mpsc::Receiver<Vec<u8>>,
    /// Whether typed bytes reach the agent (`Driver::input`).
    pub input: bool,
}

/// Output chunks queued per attached client. A client that falls this far behind stops
/// getting chunks until it is sent a repaint (memory stays bounded whatever it does).
pub const ATTACH_QUEUE: usize = 256;

struct FanoutSub {
    tx: mpsc::SyncSender<Vec<u8>>,
    /// Its queue overflowed: chunks were dropped, so live bytes would corrupt its screen.
    /// Nothing more is sent until [`Fanout::repaint`] delivers a full screen.
    needs_repaint: bool,
}

/// Fan-out of output bytes to attached clients over bounded queues; dead receivers are
/// dropped on send. The owner of the screen calls [`Fanout::repaint`] whenever
/// [`Fanout::needs_repaint`] is true (e.g. right after `send`).
pub struct Fanout {
    next: u64,
    cap: usize,
    subs: HashMap<u64, FanoutSub>,
}

impl Default for Fanout {
    fn default() -> Self {
        Fanout::with_capacity(ATTACH_QUEUE)
    }
}

impl Fanout {
    /// `cap` chunks queued per client (at least 1).
    pub fn with_capacity(cap: usize) -> Fanout {
        Fanout { next: 0, cap: cap.max(1), subs: HashMap::new() }
    }

    pub fn subscribe(&mut self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        self.next += 1;
        let (tx, rx) = mpsc::sync_channel(self.cap);
        self.subs.insert(self.next, FanoutSub { tx, needs_repaint: false });
        (self.next, rx)
    }

    pub fn remove(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    /// Never blocks: a full queue marks that client for a repaint and drops the chunk.
    pub fn send(&mut self, bytes: &[u8]) {
        self.subs.retain(|_, s| {
            if s.needs_repaint {
                return true;
            }
            match s.tx.try_send(bytes.to_vec()) {
                Ok(()) => true,
                Err(mpsc::TrySendError::Full(_)) => {
                    s.needs_repaint = true;
                    true
                }
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Some client dropped chunks and waits for a full screen.
    pub fn needs_repaint(&self) -> bool {
        self.subs.values().any(|s| s.needs_repaint)
    }

    /// Sends `screen` (bytes that redraw the whole screen) to every client marked by
    /// [`Fanout::send`] whose queue has room again, and resumes live bytes for them.
    pub fn repaint(&mut self, screen: &[u8]) {
        self.subs.retain(|_, s| {
            if !s.needs_repaint {
                return true;
            }
            match s.tx.try_send(screen.to_vec()) {
                Ok(()) => {
                    s.needs_repaint = false;
                    true
                }
                Err(mpsc::TrySendError::Full(_)) => true,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }
}

pub trait Driver: Send + Sync {
    /// Delivers a message now: types it into the TUI (Claude) or steers / starts a turn
    /// (Codex). Returns once the message is handed to the driver's worker.
    fn send(&self, text: &str) -> Result<(), DriverError>;
    /// Esc (Claude) or `turn/interrupt` (Codex).
    fn interrupt(&self) -> Result<(), DriverError>;
    /// Answers the permission dialog / approval request named by `key` (Codex: the request
    /// id; Claude: the request detail, checked against the dialog on screen). Selects only
    /// a one-time "Yes" or "No", never an "always" or "auto" option.
    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError>;
    /// Answers Claude's workspace-trust dialog. `false` ends the session.
    fn trust(&self, accept: bool) -> Result<(), DriverError>;
    /// True while a dialog would capture typed text (a message must not be typed then).
    fn dialog_open(&self) -> bool;
    /// Claude: start (or switch) tailing the session transcript at `offset`.
    fn tail_transcript(&self, path: &Path, offset: u64);
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError>;
    fn input(&self, attachment: u64, bytes: &[u8]) -> Result<(), DriverError>;
    fn detach(&self, attachment: u64);
    /// The attached terminal changed size (Claude: resize the PTY; the agent redraws).
    fn resize(&self, _rows: u16, _cols: u16) -> Result<(), DriverError> {
        Err(DriverError::Unsupported("resize"))
    }
    /// Last lines of the screen (Claude) or stream (Codex), for the CLI.
    fn screen_tail(&self) -> String;
    /// Ends the agent gracefully, then kills it. Blocks until the process is gone; the
    /// driver still emits `Exited`.
    fn stop(&self);
}

pub struct SpawnRequest {
    pub task: Task,
    pub session_id: String,
    /// Native id to resume (Claude session id / Codex thread id).
    pub resume: Option<String>,
    pub paths: Paths,
}

pub trait DriverFactory: Send + Sync {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_drops_dead_receivers() {
        let mut f = Fanout::default();
        let (a, rx_a) = f.subscribe();
        let (_b, rx_b) = f.subscribe();
        f.send(b"x");
        assert_eq!(rx_a.recv().unwrap(), b"x");
        drop(rx_b);
        f.send(b"y");
        assert_eq!(rx_a.recv().unwrap(), b"y");
        f.remove(a);
        assert!(f.is_empty());
    }

    #[test]
    fn a_stalled_client_queue_is_bounded_and_resumes_after_a_repaint() {
        let mut f = Fanout::with_capacity(2);
        let (_id, rx) = f.subscribe();
        for b in [b"1", b"2", b"3", b"4"] {
            f.send(b);
        }
        assert!(f.needs_repaint());
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"1".to_vec(), b"2".to_vec()]);
        // Room again, but live bytes stay held back until the full screen is sent.
        f.send(b"5");
        assert!(rx.try_recv().is_err());
        f.repaint(b"SCREEN");
        assert!(!f.needs_repaint());
        f.send(b"6");
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"SCREEN".to_vec(), b"6".to_vec()]);
    }

    #[test]
    fn sink_tags_events_with_the_session() {
        let (tx, rx) = mpsc::channel();
        Sink::new("s1", tx).emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(rx.recv().unwrap(), ("s1".to_string(), DriverEvent::Boot(BootState::Ready)));
    }
}
```

`crates/mirante-daemon/src/driver/claude.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn fast() -> ClaudeTiming {
        ClaudeTiming {
            tick: Duration::from_millis(20),
            type_pause: Duration::from_millis(50),
            tail_poll: Duration::from_millis(20),
            stop_grace: Duration::from_secs(2),
            boot_limit: Duration::from_secs(10),
        }
    }

    /// A fake `claude`: a shell script in the PTY (raw mode, no echo).
    fn fake(script: &str) -> (Arc<ClaudeDriver>, mpsc::Receiver<(String, DriverEvent)>) {
        let (tx, rx) = mpsc::channel();
        let spec = PtySpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), format!("stty raw -echo; {script}")],
            cwd: std::env::temp_dir(),
            env_remove: vec![],
            env_set: vec![("TERM".into(), "xterm-256color".into())],
            rows: 30,
            cols: 100,
        };
        (ClaudeDriver::spawn(spec, Sink::new("s1", tx), fast()).unwrap(), rx)
    }

    fn next(rx: &mpsc::Receiver<(String, DriverEvent)>, want: impl Fn(&DriverEvent) -> bool) -> DriverEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let (_, ev) = rx.recv_timeout(left).expect("event in time");
            if want(&ev) {
                return ev;
            }
        }
    }

    fn wait_grid(d: &ClaudeDriver, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !d.pty.grid().contains(text) {
            assert!(Instant::now() < deadline, "screen never showed {text:?}:\n{}", d.pty.grid());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    const READY: &str = "printf '\\033[2J\\033[H> \\r\\n  manual mode on\\r\\n'";

    #[test]
    fn trust_prompt_waits_for_the_user_then_down_enter_accepts() {
        let (d, rx) = fake(
            "printf 'Do you trust the files in this folder?\\r\\n> 1. No, exit\\r\\n  2. Yes, I trust this folder\\r\\n'; \
             k=$(dd bs=1 count=4 2>/dev/null | od -An -c | tr -d ' \\n'); \
             printf '\\033[2J\\033[Hkeys:%s\\r\\n  manual mode on\\r\\n' \"$k\"; sleep 5",
        );
        assert_eq!(next(&rx, |e| matches!(e, DriverEvent::Boot(_))), DriverEvent::Boot(BootState::TrustPrompt));
        assert!(d.dialog_open());
        assert!(matches!(d.send("hi"), Err(DriverError::Failed(_))));
        d.trust(true).unwrap();
        assert_eq!(next(&rx, |e| matches!(e, DriverEvent::Boot(_))), DriverEvent::Boot(BootState::Ready));
        wait_grid(&d, "keys:033[B\\r");
        assert!(matches!(d.trust(true), Err(DriverError::NoDialog(_))));
    }

    #[test]
    fn declining_trust_ends_the_process() {
        let (d, rx) = fake("printf 'Yes, I trust this folder\\r\\n'; sleep 30");
        next(&rx, |e| *e == DriverEvent::Boot(BootState::TrustPrompt));
        d.trust(false).unwrap();
        assert!(matches!(next(&rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { .. }));
    }

    #[test]
    fn messages_are_typed_as_one_line_then_enter() {
        let (d, rx) = fake(&format!(
            "{READY}; l=$(dd bs=1 count=13 2>/dev/null | od -An -c | tr -d ' \\n'); printf 'got:%s' \"$l\"; sleep 5"
        ));
        next(&rx, |e| *e == DriverEvent::Boot(BootState::Ready));
        d.send("use\ntabs  now").unwrap();
        wait_grid(&d, "got:usetabsnow\\r");
    }

    #[test]
    fn permission_answer_types_the_exact_option_for_the_matching_dialog() {
        let dialog = "printf '\\033[2J\\033[H Bash command\\r\\n   touch x\\r\\n Do you want to proceed?\\r\\n ❯ 1. Yes\\r\\n   2. Yes, and always allow access to /w\\r\\n   3. No\\r\\n'";
        let (d, rx) = fake(&format!(
            "{READY}; sleep 1; {dialog}; k=$(dd bs=1 count=1 2>/dev/null); printf '\\r\\npicked:%s' \"$k\"; sleep 5"
        ));
        next(&rx, |e| *e == DriverEvent::Boot(BootState::Ready));
        wait_grid(&d, "Do you want to proceed?");
        assert!(d.dialog_open());
        assert!(matches!(d.answer_permission("rm -rf /", true), Err(DriverError::NoDialog(_))));
        d.answer_permission("touch x", false).unwrap();
        wait_grid(&d, "picked:3");
    }

    #[test]
    fn transcript_lines_and_offsets_are_emitted_from_the_given_offset() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "{\"old\":true}\n{\"type\":\"user\"}\n").unwrap();
        let (d, rx) = fake("sleep 5");
        d.tail_transcript(&path, 13);
        assert_eq!(
            next(&rx, |e| matches!(e, DriverEvent::Transcript(_))),
            DriverEvent::Transcript(json!({"type": "user"}))
        );
        let off = next(&rx, |e| matches!(e, DriverEvent::TranscriptOffset { .. }));
        assert_eq!(off, DriverEvent::TranscriptOffset { path: path.to_string_lossy().into_owned(), offset: 29 });
        d.tail_transcript(&path, 0);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"n\":2}\n"))
            .unwrap();
        assert_eq!(next(&rx, |e| matches!(e, DriverEvent::Transcript(_))), DriverEvent::Transcript(json!({"n": 2})));
    }

    #[test]
    fn stop_types_exit_and_the_driver_reports_the_exit() {
        let (d, rx) = fake("while true; do c=$(dd bs=1 count=1 2>/dev/null); [ \"$c\" = t ] && exit 0; done");
        d.stop();
        assert_eq!(next(&rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { code: Some(0) });
        assert!(d.pty.exited());
    }

    #[test]
    fn attach_repaints_then_streams_and_input_reaches_the_agent() {
        let (d, _rx) = fake("printf 'hello'; k=$(dd bs=1 count=2 2>/dev/null); printf ' typed:%s' \"$k\"; sleep 5");
        wait_grid(&d, "hello");
        let a = d.attach(24, 80).unwrap();
        assert!(String::from_utf8_lossy(&a.initial).contains("hello"));
        d.input(a.id, b"ok").unwrap();
        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains("typed:ok") {
            seen.extend(a.output.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        d.detach(a.id);
    }
}
```

`crates/mirante-daemon/src/driver/real.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;

    #[test]
    fn private_files_are_0600_in_0700_dirs() {
        let d = TestDir::new();
        let p = d.path().join("sessions/t1/claude-settings.json");
        write_private(&p, "{}").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(p.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        write_private(&p, "{\"x\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"x\":1}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error: cannot find macro `json` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/driver/claude.rs`:
```rust
//! Claude Code driver: the unmodified interactive `claude` in a PTY (FINDINGS: the
//! human-attended mode that fits the terms, steers mid-run and resumes). Status comes from
//! hooks (via `mirante-hook` → daemon) fused with this driver's transcript tail; the screen
//! is read only to act on dialogs:
//! - boot: the workspace-trust dialog (default "No, exit") is reported as `TrustPrompt` and
//!   answered only by the user (`trust`); `Ready` needs the input box footer.
//! - messages are typed (text, pause, Enter), never while a dialog is open;
//! - interrupt is Esc; permission answers type the digit of the exact "Yes" / "No" option
//!   of the dialog that matches the request.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use mirante_core::transcript_tail::TranscriptTail;
use mirante_ipc::types::BootState;
use serde_json::json;

use super::pty::{PtyProcess, PtySpec};
use super::{screen, Attachment, Driver, DriverError, DriverEvent, Sink};

const ESC: &[u8] = b"\x1b";
const DOWN: &[u8] = b"\x1b[B";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeTiming {
    /// Screen polling interval during boot.
    pub tick: Duration,
    /// Pause between typed text and Enter (the TUI drops an Enter that arrives too fast).
    pub type_pause: Duration,
    pub tail_poll: Duration,
    /// How long `/exit` gets before the process is killed.
    pub stop_grace: Duration,
    /// The boot watcher gives up after this long (the session stays `Booting`).
    pub boot_limit: Duration,
}

impl Default for ClaudeTiming {
    fn default() -> Self {
        ClaudeTiming {
            tick: Duration::from_millis(200),
            type_pause: Duration::from_millis(400),
            tail_poll: Duration::from_millis(300),
            stop_grace: Duration::from_secs(3),
            boot_limit: Duration::from_secs(600),
        }
    }
}

enum Typing {
    Text(String),
    Keys(Vec<u8>),
    Pause(Duration),
}

pub struct ClaudeDriver {
    pty: Arc<PtyProcess>,
    sink: Sink,
    typist: Mutex<mpsc::Sender<Typing>>,
    timing: ClaudeTiming,
    tail_path: Mutex<Option<PathBuf>>,
    /// Bumped to stop the current tail thread (new path, stop, exit).
    tail_gen: Arc<AtomicU64>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl ClaudeDriver {
    pub fn spawn(spec: PtySpec, sink: Sink, timing: ClaudeTiming) -> Result<Arc<ClaudeDriver>, DriverError> {
        let tail_gen = Arc::new(AtomicU64::new(0));
        let (exit_sink, exit_gen) = (sink.clone(), tail_gen.clone());
        let pty = PtyProcess::spawn(&spec, move |code| {
            exit_gen.fetch_add(1, Ordering::SeqCst);
            exit_sink.emit(DriverEvent::Exited { code });
        })?;
        let (tx, rx) = mpsc::channel::<Typing>();
        let typist_pty = pty.clone();
        let pause = timing.type_pause;
        std::thread::spawn(move || {
            for t in rx {
                let _ = match t {
                    Typing::Text(s) => typist_pty
                        .write(s.as_bytes())
                        .map(|()| std::thread::sleep(pause))
                        .and_then(|()| typist_pty.write(b"\r")),
                    Typing::Keys(k) => typist_pty.write(&k),
                    Typing::Pause(d) => {
                        std::thread::sleep(d);
                        Ok(())
                    }
                };
            }
        });
        let driver = Arc::new(ClaudeDriver {
            pty: pty.clone(),
            sink: sink.clone(),
            typist: Mutex::new(tx),
            timing,
            tail_path: Mutex::new(None),
            tail_gen,
        });
        std::thread::spawn(move || watch_boot(&pty, &sink, timing));
        Ok(driver)
    }

    fn type_(&self, t: Typing) -> Result<(), DriverError> {
        lock(&self.typist).send(t).map_err(|_| DriverError::Failed("typist stopped".into()))
    }
}

/// Reports `TrustPrompt` when the trust dialog shows (once per appearance) and `Ready` once
/// the input box is up with no dialog. Never answers anything itself.
fn watch_boot(pty: &PtyProcess, sink: &Sink, timing: ClaudeTiming) {
    let start = Instant::now();
    let mut trust_shown = false;
    while !pty.exited() && start.elapsed() < timing.boot_limit {
        let grid = pty.grid();
        if screen::is_trust_dialog(&grid) {
            if !trust_shown {
                trust_shown = true;
                sink.emit(DriverEvent::Boot(BootState::TrustPrompt));
            }
        } else if screen::is_ready(&grid) && !screen::has_permission_prompt(&grid) {
            sink.emit(DriverEvent::Boot(BootState::Ready));
            return;
        }
        std::thread::sleep(timing.tick);
    }
}

impl Drop for ClaudeDriver {
    fn drop(&mut self) {
        self.tail_gen.fetch_add(1, Ordering::SeqCst);
        self.pty.kill();
    }
}

impl Driver for ClaudeDriver {
    fn send(&self, text: &str) -> Result<(), DriverError> {
        if self.dialog_open() {
            return Err(DriverError::Failed("a dialog is open on the agent's screen".into()));
        }
        // A newline would submit early: one message is one line.
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        self.type_(Typing::Text(line))
    }

    fn interrupt(&self) -> Result<(), DriverError> {
        self.type_(Typing::Keys(ESC.to_vec()))
    }

    fn answer_permission(&self, detail: &str, allow: bool) -> Result<(), DriverError> {
        let grid = self.pty.grid();
        if !screen::has_permission_prompt(&grid) {
            return Err(DriverError::NoDialog("no permission dialog on screen".into()));
        }
        if !screen::dialog_matches(&grid, detail) {
            return Err(DriverError::NoDialog(
                "the dialog on screen is for another request; answer it in `mirante attach`".into(),
            ));
        }
        let label = if allow { "Yes" } else { "No" };
        let key = screen::option_key(&grid, label)
            .ok_or_else(|| DriverError::NoDialog(format!("the dialog has no plain \"{label}\" option")))?;
        self.type_(Typing::Keys(vec![key as u8]))
    }

    fn trust(&self, accept: bool) -> Result<(), DriverError> {
        if !screen::is_trust_dialog(&self.pty.grid()) {
            return Err(DriverError::NoDialog("no trust dialog on screen".into()));
        }
        if !accept {
            self.pty.kill();
            return Ok(());
        }
        // The default option is "No, exit": move to "Yes, I trust this folder" first.
        self.type_(Typing::Keys(DOWN.to_vec()))?;
        self.type_(Typing::Pause(Duration::from_millis(500)))?;
        self.type_(Typing::Text(String::new()))
    }

    fn dialog_open(&self) -> bool {
        let grid = self.pty.grid();
        screen::has_permission_prompt(&grid) || screen::is_trust_dialog(&grid)
    }

    fn tail_transcript(&self, path: &Path, offset: u64) {
        let mut cur = lock(&self.tail_path);
        if cur.as_deref() == Some(path) {
            return;
        }
        *cur = Some(path.to_path_buf());
        let my_gen = self.tail_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let (gen, sink, poll) = (self.tail_gen.clone(), self.sink.clone(), self.timing.tail_poll);
        let mut tail = TranscriptTail::new(path, offset);
        let path = path.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            let mut last = offset;
            while gen.load(Ordering::SeqCst) == my_gen {
                // Catch up while the batch says more is waiting (never loop on file length:
                // a partial last line keeps the offset behind it).
                loop {
                    match tail.poll() {
                        Ok(batch) => {
                            for line in batch.lines {
                                sink.emit(DriverEvent::Transcript(line));
                            }
                            if tail.offset() != last {
                                last = tail.offset();
                                sink.emit(DriverEvent::TranscriptOffset { path: path.clone(), offset: last });
                            }
                            if !batch.more {
                                break;
                            }
                        }
                        Err(e) => {
                            sink.emit(DriverEvent::Step(json!({ "transcript_error": e.to_string() })));
                            break;
                        }
                    }
                }
                std::thread::sleep(poll);
            }
        });
    }

    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError> {
        self.pty.resize(rows, cols)?;
        let (id, output) = self.pty.subscribe();
        Ok(Attachment { id, initial: self.pty.repaint(), output, input: true })
    }

    fn input(&self, _attachment: u64, bytes: &[u8]) -> Result<(), DriverError> {
        Ok(self.pty.write(bytes)?)
    }

    fn detach(&self, attachment: u64) {
        self.pty.unsubscribe(attachment);
    }

    fn resize(&self, rows: u16, cols: u16) -> Result<(), DriverError> {
        Ok(self.pty.resize(rows, cols)?)
    }

    fn screen_tail(&self) -> String {
        screen::tail(&self.pty.grid(), 20)
    }

    fn stop(&self) {
        self.tail_gen.fetch_add(1, Ordering::SeqCst);
        if !self.pty.exited() {
            // Esc closes a dialog or interrupts the turn, then `/exit` quits cleanly.
            let _ = self.pty.write(ESC);
            std::thread::sleep(Duration::from_millis(300));
            let _ = self.pty.write(b"/exit");
            std::thread::sleep(self.timing.type_pause);
            let _ = self.pty.write(b"\r");
            if !self.pty.wait_exit(self.timing.stop_grace) {
                self.pty.kill();
                self.pty.wait_exit(Duration::from_secs(2));
            }
        }
    }
}
```

Add above the test module in `crates/mirante-daemon/src/driver/real.rs`:
```rust
//! The production driver factory: the real `claude` in a PTY with Mirante's scoped hook
//! settings (the real `codex app-server` is wired in by Task 16). Binaries: `MIRANTE_CLAUDE_BIN` /
//! `MIRANTE_CODEX_BIN`, else PATH without wrapper shims. The hook command:
//! `MIRANTE_HOOK_BIN`, else `mirante-hook` next to the daemon executable.
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mirante_core::claude_launch::{launch_spec, resolve_claude_bin_from_env, settings_json, LaunchInput};
use mirante_core::model::AgentKind;
use mirante_ipc::paths::HOME_ENV;

use super::claude::{ClaudeDriver, ClaudeTiming};
use super::pty::PtySpec;
use super::{Driver, DriverError, DriverFactory, Sink, SpawnRequest};

#[derive(Debug, Clone)]
pub struct RealFactory {
    /// PTY size before any client attaches.
    pub rows: u16,
    pub cols: u16,
}

impl Default for RealFactory {
    fn default() -> Self {
        RealFactory { rows: 40, cols: 120 }
    }
}

pub fn hook_bin() -> Result<PathBuf, DriverError> {
    let p = match std::env::var_os("MIRANTE_HOOK_BIN").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe()?.with_file_name("mirante-hook"),
    };
    let ok = std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if ok {
        Ok(p)
    } else {
        Err(DriverError::Failed(format!("hook binary not found: {}", p.display())))
    }
}

/// Writes `contents` to `path` (mode 0600) inside a 0700 directory.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(path)?;
    f.write_all(contents.as_bytes())
}

fn parent_env() -> Vec<String> {
    std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect()
}

impl RealFactory {
    fn claude(&self, req: &SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        let claude = resolve_claude_bin_from_env().map_err(|e| DriverError::Failed(e.to_string()))?;
        let hook = hook_bin()?;
        let settings = req.paths.session_dir(&req.task.id).join("claude-settings.json");
        write_private(&settings, &settings_json(&hook.to_string_lossy(), &[]).to_string())?;
        let spec = launch_spec(LaunchInput {
            claude_bin: &claude,
            worktree: Path::new(&req.task.worktree_path),
            settings_path: &settings,
            task_id: &req.task.id,
            session_id: &req.session_id,
            resume: req.resume.as_deref(),
            parent_env: parent_env(),
        });
        let mut env_set = spec.env_set;
        // The hook (a child of claude) must reach this daemon, not one under another home.
        env_set.push((HOME_ENV.to_string(), req.paths.home().to_string_lossy().into_owned()));
        env_set.push(("TERM".to_string(), "xterm-256color".to_string()));
        let pty = PtySpec {
            program: spec.program,
            args: spec.args,
            cwd: spec.cwd,
            env_remove: spec.env_remove,
            env_set,
            rows: self.rows,
            cols: self.cols,
        };
        Ok(ClaudeDriver::spawn(pty, sink, ClaudeTiming::default())?)
    }
}

impl DriverFactory for RealFactory {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        match req.task.agent {
            AgentKind::Claude => self.claude(&req, sink),
            AgentKind::Codex => Err(DriverError::Unsupported("the Codex driver is not built yet")),
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 58 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): claude PTY driver with user-answered trust, typed steering, transcript tail

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

## Phase 2b-iii — Codex driver

### Task 15: Codex status engine and JSON-RPC connection

**Files:**
- Modify: `crates/mirante-daemon/src/driver/mod.rs`
- Create: `crates/mirante-daemon/src/driver/codex_status.rs`
- Create: `crates/mirante-daemon/src/driver/codex_rpc.rs`

Codex status comes straight from app-server traffic (FINDINGS: no hook/transcript fusion needed). FINDINGS rule 8: `turn/completed` does not mean every command finished (run 2: `passo-2`'s `item/completed` came 1.3 s after it). The engine tracks open `commandExecution` items by id, keeps `Working` with the detail "turn done, N command(s) still finishing" while any is open after the turn, goes `Idle` only on the last one, and never reopens a turn from an item event. Approval requests mean `NeedsYou` until answered; the turn ending voids them. It reuses Plan 2a's `Transition` and `Status::can_transition_to`.

`RpcConn` speaks line-delimited JSON-RPC over any stream pair (the real app-server's stdio, or a socket pair in tests): requests wait for their own response with a timeout; notifications and server→client requests (approvals) go to a callback; EOF is reported once.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/driver/mod.rs`:
```rust
//! The agent driver interface. Every agent sits behind [`Driver`]; an operation the agent
//! cannot do is a typed [`DriverError::Unsupported`], never a silent failure. Drivers are
//! non-blocking (typing and protocol calls run on the driver's own threads) except
//! [`Driver::stop`], so the hub may call them while holding its lock.
//!
//! Drivers report back through a [`Sink`] (a channel drained by the hub's pump thread),
//! never by calling the hub directly, so a driver thread can never deadlock on the hub.
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc};

use mirante_core::model::{Status, Task};
use mirante_ipc::types::BootState;
use mirante_ipc::Paths;
use serde_json::Value;

pub mod claude;
pub mod codex_rpc;
pub mod codex_status;
#[cfg(test)]
pub mod fake;
pub mod pty;
pub mod real;
pub mod screen;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The agent cannot do this (e.g. a trust prompt on Codex).
    Unsupported(&'static str),
    /// The dialog the operation targets is not on screen / not pending.
    NoDialog(String),
    Failed(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Unsupported(what) => write!(f, "not supported by this agent: {what}"),
            DriverError::NoDialog(m) => write!(f, "no matching dialog: {m}"),
            DriverError::Failed(m) => write!(f, "agent driver failed: {m}"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Failed(e.to_string())
    }
}

/// What a driver tells the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// Claude: one main-transcript line, for the hub's `StatusEngine`.
    Transcript(Value),
    /// Claude: where the transcript tailer is (persisted, not audited).
    TranscriptOffset {
        path: String,
        offset: u64,
    },
    /// Codex: a status computed by the driver's own engine. `turn_open` drives the turn
    /// records; `detail` is shown next to the status.
    Status {
        to: Status,
        cause: String,
        turn_open: bool,
        detail: Option<String>,
    },
    /// Codex: an approval request. `key` is what `answer_permission` gets back.
    PermissionRequested {
        key: String,
        tool: String,
        detail: String,
    },
    /// Codex: the thread id, used for `thread/resume`.
    NativeId(String),
    Boot(BootState),
    /// A step for the feed and audit trail (command finished, agent message, ...).
    Step(Value),
    Exited {
        code: Option<i32>,
    },
}

/// Channel end a driver emits into. Every event is tagged with the driver's session id so
/// late events from an ended session are ignored.
#[derive(Clone)]
pub struct Sink {
    session_id: String,
    tx: mpsc::Sender<(String, DriverEvent)>,
}

impl Sink {
    pub fn new(session_id: &str, tx: mpsc::Sender<(String, DriverEvent)>) -> Sink {
        Sink { session_id: session_id.to_string(), tx }
    }

    pub fn emit(&self, event: DriverEvent) {
        let _ = self.tx.send((self.session_id.clone(), event));
    }
}

/// A live view of the agent's terminal (Claude) or event stream (Codex).
pub struct Attachment {
    pub id: u64,
    /// Bytes that repaint the current screen (or recent stream) before live output.
    pub initial: Vec<u8>,
    pub output: mpsc::Receiver<Vec<u8>>,
    /// Whether typed bytes reach the agent (`Driver::input`).
    pub input: bool,
}

/// Output chunks queued per attached client. A client that falls this far behind stops
/// getting chunks until it is sent a repaint (memory stays bounded whatever it does).
pub const ATTACH_QUEUE: usize = 256;

struct FanoutSub {
    tx: mpsc::SyncSender<Vec<u8>>,
    /// Its queue overflowed: chunks were dropped, so live bytes would corrupt its screen.
    /// Nothing more is sent until [`Fanout::repaint`] delivers a full screen.
    needs_repaint: bool,
}

/// Fan-out of output bytes to attached clients over bounded queues; dead receivers are
/// dropped on send. The owner of the screen calls [`Fanout::repaint`] whenever
/// [`Fanout::needs_repaint`] is true (e.g. right after `send`).
pub struct Fanout {
    next: u64,
    cap: usize,
    subs: HashMap<u64, FanoutSub>,
}

impl Default for Fanout {
    fn default() -> Self {
        Fanout::with_capacity(ATTACH_QUEUE)
    }
}

impl Fanout {
    /// `cap` chunks queued per client (at least 1).
    pub fn with_capacity(cap: usize) -> Fanout {
        Fanout { next: 0, cap: cap.max(1), subs: HashMap::new() }
    }

    pub fn subscribe(&mut self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        self.next += 1;
        let (tx, rx) = mpsc::sync_channel(self.cap);
        self.subs.insert(self.next, FanoutSub { tx, needs_repaint: false });
        (self.next, rx)
    }

    pub fn remove(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    /// Never blocks: a full queue marks that client for a repaint and drops the chunk.
    pub fn send(&mut self, bytes: &[u8]) {
        self.subs.retain(|_, s| {
            if s.needs_repaint {
                return true;
            }
            match s.tx.try_send(bytes.to_vec()) {
                Ok(()) => true,
                Err(mpsc::TrySendError::Full(_)) => {
                    s.needs_repaint = true;
                    true
                }
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Some client dropped chunks and waits for a full screen.
    pub fn needs_repaint(&self) -> bool {
        self.subs.values().any(|s| s.needs_repaint)
    }

    /// Sends `screen` (bytes that redraw the whole screen) to every client marked by
    /// [`Fanout::send`] whose queue has room again, and resumes live bytes for them.
    pub fn repaint(&mut self, screen: &[u8]) {
        self.subs.retain(|_, s| {
            if !s.needs_repaint {
                return true;
            }
            match s.tx.try_send(screen.to_vec()) {
                Ok(()) => {
                    s.needs_repaint = false;
                    true
                }
                Err(mpsc::TrySendError::Full(_)) => true,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }
}

pub trait Driver: Send + Sync {
    /// Delivers a message now: types it into the TUI (Claude) or steers / starts a turn
    /// (Codex). Returns once the message is handed to the driver's worker.
    fn send(&self, text: &str) -> Result<(), DriverError>;
    /// Esc (Claude) or `turn/interrupt` (Codex).
    fn interrupt(&self) -> Result<(), DriverError>;
    /// Answers the permission dialog / approval request named by `key` (Codex: the request
    /// id; Claude: the request detail, checked against the dialog on screen). Selects only
    /// a one-time "Yes" or "No", never an "always" or "auto" option.
    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError>;
    /// Answers Claude's workspace-trust dialog. `false` ends the session.
    fn trust(&self, accept: bool) -> Result<(), DriverError>;
    /// True while a dialog would capture typed text (a message must not be typed then).
    fn dialog_open(&self) -> bool;
    /// Claude: start (or switch) tailing the session transcript at `offset`.
    fn tail_transcript(&self, path: &Path, offset: u64);
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError>;
    fn input(&self, attachment: u64, bytes: &[u8]) -> Result<(), DriverError>;
    fn detach(&self, attachment: u64);
    /// The attached terminal changed size (Claude: resize the PTY; the agent redraws).
    fn resize(&self, _rows: u16, _cols: u16) -> Result<(), DriverError> {
        Err(DriverError::Unsupported("resize"))
    }
    /// Last lines of the screen (Claude) or stream (Codex), for the CLI.
    fn screen_tail(&self) -> String;
    /// Ends the agent gracefully, then kills it. Blocks until the process is gone; the
    /// driver still emits `Exited`.
    fn stop(&self);
}

pub struct SpawnRequest {
    pub task: Task,
    pub session_id: String,
    /// Native id to resume (Claude session id / Codex thread id).
    pub resume: Option<String>,
    pub paths: Paths,
}

pub trait DriverFactory: Send + Sync {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_drops_dead_receivers() {
        let mut f = Fanout::default();
        let (a, rx_a) = f.subscribe();
        let (_b, rx_b) = f.subscribe();
        f.send(b"x");
        assert_eq!(rx_a.recv().unwrap(), b"x");
        drop(rx_b);
        f.send(b"y");
        assert_eq!(rx_a.recv().unwrap(), b"y");
        f.remove(a);
        assert!(f.is_empty());
    }

    #[test]
    fn a_stalled_client_queue_is_bounded_and_resumes_after_a_repaint() {
        let mut f = Fanout::with_capacity(2);
        let (_id, rx) = f.subscribe();
        for b in [b"1", b"2", b"3", b"4"] {
            f.send(b);
        }
        assert!(f.needs_repaint());
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"1".to_vec(), b"2".to_vec()]);
        // Room again, but live bytes stay held back until the full screen is sent.
        f.send(b"5");
        assert!(rx.try_recv().is_err());
        f.repaint(b"SCREEN");
        assert!(!f.needs_repaint());
        f.send(b"6");
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"SCREEN".to_vec(), b"6".to_vec()]);
    }

    #[test]
    fn sink_tags_events_with_the_session() {
        let (tx, rx) = mpsc::channel();
        Sink::new("s1", tx).emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(rx.recv().unwrap(), ("s1".to_string(), DriverEvent::Boot(BootState::Ready)));
    }
}
```

`crates/mirante-daemon/src/driver/codex_status.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn n<'a>(method: &'a str, params: &'a Value) -> CodexInput<'a> {
        CodexInput::Notification { method, params }
    }

    #[test]
    fn plain_turn() {
        let mut e = CodexEngine::new();
        let started = json!({"turn": {"id": "tu1"}});
        assert_eq!(e.apply(&n("turn/started", &started)).unwrap().to, Status::Working);
        assert_eq!(e.turn_id(), Some("tu1"));
        assert_eq!(e.apply(&n("turn/completed", &json!({"turn": {"id": "tu1"}}))).unwrap().to, Status::Idle);
        assert!(!e.turn_open());
    }

    /// FINDINGS rule 8 (S1 Codex run 2): passo-2's item/completed came 1.3 s after
    /// turn/completed. The status must not say idle while it runs, and the late item
    /// must not reopen the turn.
    #[test]
    fn command_finishing_after_turn_completed_keeps_working_with_detail() {
        let mut e = CodexEngine::new();
        e.apply(&n("turn/started", &json!({"turn": {"id": "tu1"}})));
        e.apply(&n("item/started", &json!({"item": {"type": "commandExecution", "id": "c2", "command": "sleep 3"}})));
        assert_eq!(e.apply(&n("turn/completed", &json!({}))), None);
        assert_eq!(e.status(), Status::Working);
        assert_eq!(e.detail().as_deref(), Some("turn done, 1 command(s) still finishing"));
        assert!(!e.turn_open());
        let t = e.apply(&n("item/completed", &json!({"item": {"type": "commandExecution", "id": "c2"}}))).unwrap();
        assert_eq!((t.from, t.to, t.cause.as_str()), (Status::Working, Status::Idle, "codex:item/completed"));
        assert!(!e.turn_open());
        assert_eq!(e.detail(), None);
    }

    #[test]
    fn approvals_need_you_until_answered_and_die_with_the_turn() {
        let mut e = CodexEngine::new();
        e.apply(&n("turn/started", &json!({"turn": {"id": "tu1"}})));
        assert_eq!(e.apply(&CodexInput::ApprovalRequested("5".into())).unwrap().to, Status::NeedsYou);
        assert_eq!(e.apply(&CodexInput::ApprovalAnswered("5".into())).unwrap().to, Status::Working);
        e.apply(&CodexInput::ApprovalRequested("6".into()));
        assert_eq!(e.apply(&n("turn/completed", &json!({}))).unwrap().to, Status::Idle);
    }

    #[test]
    fn other_items_and_unknown_notifications_change_nothing() {
        let mut e = CodexEngine::new();
        assert_eq!(e.apply(&n("item/completed", &json!({"item": {"type": "agentMessage", "id": "m"}}))), None);
        assert_eq!(e.apply(&n("thread/status/changed", &json!({}))), None);
        assert_eq!(e.status(), Status::Idle);
    }

    #[test]
    fn closed_is_ended_and_final() {
        let mut e = CodexEngine::new();
        e.apply(&n("turn/started", &json!({"turn": {"id": "tu1"}})));
        assert_eq!(e.apply(&CodexInput::Closed).unwrap().to, Status::Ended);
        assert_eq!(e.apply(&n("turn/started", &json!({"turn": {"id": "tu2"}}))), None);
    }
}
```

`crates/mirante-daemon/src/driver/codex_rpc.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;

    /// (client conn, server reader, server writer, incoming receiver)
    fn pair() -> (Arc<RpcConn>, BufReader<UnixStream>, UnixStream, mpsc::Receiver<Incoming>) {
        let (client, server) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::channel();
        let conn = RpcConn::start(BufReader::new(client.try_clone().unwrap()), client, move |i| {
            let _ = tx.send(i);
        });
        (conn, BufReader::new(server.try_clone().unwrap()), server, rx)
    }

    fn read(r: &mut BufReader<UnixStream>) -> Value {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn requests_get_their_own_response_and_errors_are_returned() {
        let (conn, mut sr, mut sw, _rx) = pair();
        let c2 = conn.clone();
        let h = std::thread::spawn(move || c2.request("thread/start", json!({"cwd": "/w"}), Duration::from_secs(5)));
        let req = read(&mut sr);
        assert_eq!((req["method"].as_str(), req["params"]["cwd"].as_str()), (Some("thread/start"), Some("/w")));
        writeln!(sw, "{}", json!({"jsonrpc": "2.0", "id": req["id"], "result": {"thread": {"id": "th1"}}})).unwrap();
        assert_eq!(h.join().unwrap().unwrap()["thread"]["id"], "th1");
        let c3 = conn.clone();
        let h = std::thread::spawn(move || c3.request("turn/steer", json!({}), Duration::from_secs(5)));
        let req = read(&mut sr);
        writeln!(
            sw,
            "{}",
            json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32600, "message": "expectedTurnId mismatch"}})
        )
        .unwrap();
        assert!(h.join().unwrap().unwrap_err().contains("expectedTurnId"));
    }

    #[test]
    fn notifications_and_server_requests_reach_the_callback_and_eof_closes() {
        let (conn, mut sr, mut sw, rx) = pair();
        writeln!(sw, "{}", json!({"jsonrpc": "2.0", "method": "turn/started", "params": {"turn": {"id": "t"}}}))
            .unwrap();
        writeln!(sw, "{}", json!({"jsonrpc": "2.0", "id": 9, "method": "item/commandExecution/requestApproval", "params": {"command": "ls"}})).unwrap();
        assert_eq!(
            rx.recv().unwrap(),
            Incoming::Notification { method: "turn/started".into(), params: json!({"turn": {"id": "t"}}) }
        );
        let Incoming::Request { id, method, .. } = rx.recv().unwrap() else { panic!() };
        assert_eq!(method, "item/commandExecution/requestApproval");
        conn.respond(&id, json!({"decision": "accept"})).unwrap();
        assert_eq!(read(&mut sr), json!({"jsonrpc": "2.0", "id": 9, "result": {"decision": "accept"}}));
        drop(sw);
        drop(sr);
        conn.close();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Incoming::Closed);
    }

    #[test]
    fn a_request_without_response_times_out() {
        let (conn, _sr, _sw, _rx) = pair();
        assert!(conn.request("initialize", json!({}), Duration::from_millis(50)).unwrap_err().contains("no response"));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error: cannot find macro `json` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/driver/codex_status.rs`:
```rust
//! Codex status, straight from app-server traffic (no hooks, no transcript):
//! - `turn/started` opens the turn (`Working`); `turn/completed` closes it.
//! - FINDINGS rule 8: `turn/completed` does not mean every command finished. Open
//!   `commandExecution` items are tracked by id; while any is open after the turn, the
//!   status stays `Working` with the detail "turn done, N command(s) still finishing", and
//!   only the last `item/completed` makes it `Idle`. Item events never reopen a turn.
//! - Approval requests (server→client JSON-RPC requests) mean `NeedsYou` until answered;
//!   the turn ending voids them.
use std::collections::HashSet;

use mirante_core::model::Status;
use mirante_core::status::Transition;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum CodexInput<'a> {
    Notification {
        method: &'a str,
        params: &'a Value,
    },
    ApprovalRequested(String),
    ApprovalAnswered(String),
    /// The app-server process or its stdout ended.
    Closed,
}

#[derive(Debug, Clone, Default)]
pub struct CodexEngine {
    status: Option<Status>,
    turn: Option<String>,
    open_commands: HashSet<String>,
    approvals: HashSet<String>,
}

impl CodexEngine {
    pub fn new() -> Self {
        CodexEngine { status: Some(Status::Idle), ..Default::default() }
    }

    pub fn status(&self) -> Status {
        self.status.unwrap_or(Status::Idle)
    }

    pub fn turn_id(&self) -> Option<&str> {
        self.turn.as_deref()
    }

    pub fn turn_open(&self) -> bool {
        self.turn.is_some()
    }

    pub fn background_commands(&self) -> usize {
        self.open_commands.len()
    }

    /// Shown next to the status: why a finished turn is still `Working`.
    pub fn detail(&self) -> Option<String> {
        let n = self.open_commands.len();
        (self.turn.is_none() && n > 0).then(|| format!("turn done, {n} command(s) still finishing"))
    }

    /// Where the status should be now, from the tracked facts.
    fn target(&self) -> Status {
        if !self.approvals.is_empty() {
            Status::NeedsYou
        } else if self.turn.is_some() || !self.open_commands.is_empty() {
            Status::Working
        } else {
            Status::Idle
        }
    }

    fn settle(&mut self, cause: String) -> Option<Transition> {
        let from = self.status();
        let to = self.target();
        if !from.can_transition_to(to) {
            return None;
        }
        self.status = Some(to);
        Some(Transition { from, to, cause })
    }

    pub fn apply(&mut self, input: &CodexInput<'_>) -> Option<Transition> {
        if self.status() == Status::Ended {
            return None;
        }
        match input {
            CodexInput::Notification { method, params } => {
                let item = &params["item"];
                match *method {
                    "turn/started" => {
                        self.turn = params["turn"]["id"].as_str().map(str::to_string).or(Some(String::new()))
                    }
                    "turn/completed" => {
                        self.turn = None;
                        self.approvals.clear();
                    }
                    "item/started" if item["type"] == "commandExecution" => {
                        if let Some(id) = item["id"].as_str() {
                            self.open_commands.insert(id.to_string());
                        }
                    }
                    "item/completed" if item["type"] == "commandExecution" => {
                        if let Some(id) = item["id"].as_str() {
                            self.open_commands.remove(id);
                        }
                    }
                    _ => return None,
                }
                self.settle(format!("codex:{method}"))
            }
            CodexInput::ApprovalRequested(key) => {
                self.approvals.insert(key.clone());
                self.settle("codex:approval_requested".into())
            }
            CodexInput::ApprovalAnswered(key) => {
                self.approvals.remove(key);
                self.settle("mirante:approval_answered".into())
            }
            CodexInput::Closed => {
                self.turn = None;
                self.open_commands.clear();
                self.approvals.clear();
                let from = self.status();
                self.status = Some(Status::Ended);
                Some(Transition { from, to: Status::Ended, cause: "codex:closed".into() })
            }
        }
    }
}
```

Add above the test module in `crates/mirante-daemon/src/driver/codex_rpc.rs`:
```rust
//! JSON-RPC 2.0 client over `codex app-server`'s stdio (one JSON object per line). The
//! reader thread routes responses to waiting requests and hands notifications and
//! server→client requests (approvals) to a callback.
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    /// A request from the server; answer it with [`RpcConn::respond`] using `id` verbatim.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// The server's stdout ended.
    Closed,
}

type Pending = Arc<Mutex<HashMap<u64, mpsc::Sender<Result<Value, Value>>>>>;

pub struct RpcConn {
    writer: Mutex<Box<dyn Write + Send>>,
    next_id: AtomicU64,
    pending: Pending,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl RpcConn {
    pub fn start(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        on_incoming: impl Fn(Incoming) + Send + 'static,
    ) -> Arc<RpcConn> {
        let pending: Pending = Arc::default();
        let conn = Arc::new(RpcConn {
            writer: Mutex::new(Box::new(writer)),
            next_id: AtomicU64::new(1),
            pending: pending.clone(),
        });
        std::thread::spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                let method = v["method"].as_str().map(str::to_string);
                match (method, v.get("id")) {
                    (Some(method), Some(id)) => {
                        on_incoming(Incoming::Request { id: id.clone(), method, params: v["params"].clone() })
                    }
                    (Some(method), None) => on_incoming(Incoming::Notification { method, params: v["params"].clone() }),
                    (None, Some(id)) => {
                        let waiter = id.as_u64().and_then(|id| lock(&pending).remove(&id));
                        if let Some(tx) = waiter {
                            let r = match v.get("error") {
                                Some(e) => Err(e.clone()),
                                None => Ok(v["result"].clone()),
                            };
                            let _ = tx.send(r);
                        }
                    }
                    (None, None) => {}
                }
            }
            lock(&pending).clear();
            on_incoming(Incoming::Closed);
        });
        conn
    }

    fn write(&self, msg: &Value) -> io::Result<()> {
        let mut w = lock(&self.writer);
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        w.write_all(&line)?;
        w.flush()
    }

    /// Sends a request and waits for its result. `Err` holds the JSON-RPC error object or
    /// a transport message.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        lock(&self.pending).insert(id, tx);
        if let Err(e) = self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })) {
            lock(&self.pending).remove(&id);
            return Err(e.to_string());
        }
        match rx.recv_timeout(timeout) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => {
                lock(&self.pending).remove(&id);
                Err(format!("{method}: no response within {timeout:?}"))
            }
        }
    }

    /// A notification; `Value::Null` params are omitted (as `initialized` expects).
    pub fn notify(&self, method: &str, params: Value) -> io::Result<()> {
        let mut msg = json!({ "jsonrpc": "2.0", "method": method });
        if !params.is_null() {
            msg["params"] = params;
        }
        self.write(&msg)
    }

    pub fn respond(&self, id: &Value, result: Value) -> io::Result<()> {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    pub fn respond_error(&self, id: &Value, code: i64, message: &str) -> io::Result<()> {
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
    }

    /// Closes the server's stdin (app-server exits on EOF).
    pub fn close(&self) {
        *lock(&self.writer) = Box::new(io::sink());
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 66 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): codex status engine (late command items) and JSON-RPC connection

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 16: Codex driver and its launch

**Files:**
- Modify: `crates/mirante-daemon/src/driver/mod.rs`
- Create: `crates/mirante-daemon/src/driver/codex.rs`
- Modify: `crates/mirante-daemon/src/driver/real.rs`

Method names and payloads are the ones verified in Task 10 of Week 1 plus the generated schema (`codex app-server generate-json-schema`, Codex 0.167): `initialize` (`clientInfo` required) / `initialized`, `thread/start` / `thread/resume` (`threadId`), `turn/start` (`threadId`, `input`), `turn/steer` (`threadId`, `expectedTurnId`, `input`), `turn/interrupt` (`threadId`, `turnId`).

- **Policy.** `approvalPolicy: "on-request"`, `sandbox: "workspace-write"`, `approvalsReviewer: "user"`. Never `"never"` (that was spike-only), never the automatic reviewer: approvals reach the user.
- **Approvals** (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, legacy `execCommandApproval` / `applyPatchApproval`) become permission cards keyed by the request id and are answered once with a one-time decision: `accept`/`decline`; for permissions, the requested profile with `scope: "turn"` or an empty grant; legacy `approved`/`abort`. Never `acceptForSession`, never policy amendments. MCP elicitations are declined; other server requests get a JSON-RPC "not supported" error.
- **Messages.** A worker thread sends `turn/steer` with the running turn's id, falling back to `turn/start` when the steer is rejected (the turn ended in between); `turn/start` when idle. Rule 9: a steer lands at the next model step, a running command keeps running; "stop now" is `mirante interrupt` (`turn/interrupt`).
- **Attach** is a read-only rendered stream (turns, `$ command`, exit codes, agent messages, approval notices) with 64 KiB of scrollback.
- **Launch hygiene** mirrors Claude: the real binary (`MIRANTE_CODEX_BIN` or PATH without shims), the Claude nested-session set plus every `CMUX_*` variable removed, `OPENAI_*` / `CODEX_*` never touched; stderr goes to `~/.mirante/sessions/<task>/codex-stderr.log`. A failed handshake drops the driver, which kills the process.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/driver/mod.rs`:
```rust
//! The agent driver interface. Every agent sits behind [`Driver`]; an operation the agent
//! cannot do is a typed [`DriverError::Unsupported`], never a silent failure. Drivers are
//! non-blocking (typing and protocol calls run on the driver's own threads) except
//! [`Driver::stop`], so the hub may call them while holding its lock.
//!
//! Drivers report back through a [`Sink`] (a channel drained by the hub's pump thread),
//! never by calling the hub directly, so a driver thread can never deadlock on the hub.
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{mpsc, Arc};

use mirante_core::model::{Status, Task};
use mirante_ipc::types::BootState;
use mirante_ipc::Paths;
use serde_json::Value;

pub mod claude;
pub mod codex;
pub mod codex_rpc;
pub mod codex_status;
#[cfg(test)]
pub mod fake;
pub mod pty;
pub mod real;
pub mod screen;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The agent cannot do this (e.g. a trust prompt on Codex).
    Unsupported(&'static str),
    /// The dialog the operation targets is not on screen / not pending.
    NoDialog(String),
    Failed(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Unsupported(what) => write!(f, "not supported by this agent: {what}"),
            DriverError::NoDialog(m) => write!(f, "no matching dialog: {m}"),
            DriverError::Failed(m) => write!(f, "agent driver failed: {m}"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Failed(e.to_string())
    }
}

/// What a driver tells the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// Claude: one main-transcript line, for the hub's `StatusEngine`.
    Transcript(Value),
    /// Claude: where the transcript tailer is (persisted, not audited).
    TranscriptOffset {
        path: String,
        offset: u64,
    },
    /// Codex: a status computed by the driver's own engine. `turn_open` drives the turn
    /// records; `detail` is shown next to the status.
    Status {
        to: Status,
        cause: String,
        turn_open: bool,
        detail: Option<String>,
    },
    /// Codex: an approval request. `key` is what `answer_permission` gets back.
    PermissionRequested {
        key: String,
        tool: String,
        detail: String,
    },
    /// Codex: the thread id, used for `thread/resume`.
    NativeId(String),
    Boot(BootState),
    /// A step for the feed and audit trail (command finished, agent message, ...).
    Step(Value),
    Exited {
        code: Option<i32>,
    },
}

/// Channel end a driver emits into. Every event is tagged with the driver's session id so
/// late events from an ended session are ignored.
#[derive(Clone)]
pub struct Sink {
    session_id: String,
    tx: mpsc::Sender<(String, DriverEvent)>,
}

impl Sink {
    pub fn new(session_id: &str, tx: mpsc::Sender<(String, DriverEvent)>) -> Sink {
        Sink { session_id: session_id.to_string(), tx }
    }

    pub fn emit(&self, event: DriverEvent) {
        let _ = self.tx.send((self.session_id.clone(), event));
    }
}

/// A live view of the agent's terminal (Claude) or event stream (Codex).
pub struct Attachment {
    pub id: u64,
    /// Bytes that repaint the current screen (or recent stream) before live output.
    pub initial: Vec<u8>,
    pub output: mpsc::Receiver<Vec<u8>>,
    /// Whether typed bytes reach the agent (`Driver::input`).
    pub input: bool,
}

/// Output chunks queued per attached client. A client that falls this far behind stops
/// getting chunks until it is sent a repaint (memory stays bounded whatever it does).
pub const ATTACH_QUEUE: usize = 256;

struct FanoutSub {
    tx: mpsc::SyncSender<Vec<u8>>,
    /// Its queue overflowed: chunks were dropped, so live bytes would corrupt its screen.
    /// Nothing more is sent until [`Fanout::repaint`] delivers a full screen.
    needs_repaint: bool,
}

/// Fan-out of output bytes to attached clients over bounded queues; dead receivers are
/// dropped on send. The owner of the screen calls [`Fanout::repaint`] whenever
/// [`Fanout::needs_repaint`] is true (e.g. right after `send`).
pub struct Fanout {
    next: u64,
    cap: usize,
    subs: HashMap<u64, FanoutSub>,
}

impl Default for Fanout {
    fn default() -> Self {
        Fanout::with_capacity(ATTACH_QUEUE)
    }
}

impl Fanout {
    /// `cap` chunks queued per client (at least 1).
    pub fn with_capacity(cap: usize) -> Fanout {
        Fanout { next: 0, cap: cap.max(1), subs: HashMap::new() }
    }

    pub fn subscribe(&mut self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        self.next += 1;
        let (tx, rx) = mpsc::sync_channel(self.cap);
        self.subs.insert(self.next, FanoutSub { tx, needs_repaint: false });
        (self.next, rx)
    }

    pub fn remove(&mut self, id: u64) {
        self.subs.remove(&id);
    }

    /// Never blocks: a full queue marks that client for a repaint and drops the chunk.
    pub fn send(&mut self, bytes: &[u8]) {
        self.subs.retain(|_, s| {
            if s.needs_repaint {
                return true;
            }
            match s.tx.try_send(bytes.to_vec()) {
                Ok(()) => true,
                Err(mpsc::TrySendError::Full(_)) => {
                    s.needs_repaint = true;
                    true
                }
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// Some client dropped chunks and waits for a full screen.
    pub fn needs_repaint(&self) -> bool {
        self.subs.values().any(|s| s.needs_repaint)
    }

    /// Sends `screen` (bytes that redraw the whole screen) to every client marked by
    /// [`Fanout::send`] whose queue has room again, and resumes live bytes for them.
    pub fn repaint(&mut self, screen: &[u8]) {
        self.subs.retain(|_, s| {
            if !s.needs_repaint {
                return true;
            }
            match s.tx.try_send(screen.to_vec()) {
                Ok(()) => {
                    s.needs_repaint = false;
                    true
                }
                Err(mpsc::TrySendError::Full(_)) => true,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    pub fn is_empty(&self) -> bool {
        self.subs.is_empty()
    }
}

pub trait Driver: Send + Sync {
    /// Delivers a message now: types it into the TUI (Claude) or steers / starts a turn
    /// (Codex). Returns once the message is handed to the driver's worker.
    fn send(&self, text: &str) -> Result<(), DriverError>;
    /// Esc (Claude) or `turn/interrupt` (Codex).
    fn interrupt(&self) -> Result<(), DriverError>;
    /// Answers the permission dialog / approval request named by `key` (Codex: the request
    /// id; Claude: the request detail, checked against the dialog on screen). Selects only
    /// a one-time "Yes" or "No", never an "always" or "auto" option.
    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError>;
    /// Answers Claude's workspace-trust dialog. `false` ends the session.
    fn trust(&self, accept: bool) -> Result<(), DriverError>;
    /// True while a dialog would capture typed text (a message must not be typed then).
    fn dialog_open(&self) -> bool;
    /// Claude: start (or switch) tailing the session transcript at `offset`.
    fn tail_transcript(&self, path: &Path, offset: u64);
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError>;
    fn input(&self, attachment: u64, bytes: &[u8]) -> Result<(), DriverError>;
    fn detach(&self, attachment: u64);
    /// The attached terminal changed size (Claude: resize the PTY; the agent redraws).
    fn resize(&self, _rows: u16, _cols: u16) -> Result<(), DriverError> {
        Err(DriverError::Unsupported("resize"))
    }
    /// Last lines of the screen (Claude) or stream (Codex), for the CLI.
    fn screen_tail(&self) -> String;
    /// Ends the agent gracefully, then kills it. Blocks until the process is gone; the
    /// driver still emits `Exited`.
    fn stop(&self);
}

pub struct SpawnRequest {
    pub task: Task,
    pub session_id: String,
    /// Native id to resume (Claude session id / Codex thread id).
    pub resume: Option<String>,
    pub paths: Paths,
}

pub trait DriverFactory: Send + Sync {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout_drops_dead_receivers() {
        let mut f = Fanout::default();
        let (a, rx_a) = f.subscribe();
        let (_b, rx_b) = f.subscribe();
        f.send(b"x");
        assert_eq!(rx_a.recv().unwrap(), b"x");
        drop(rx_b);
        f.send(b"y");
        assert_eq!(rx_a.recv().unwrap(), b"y");
        f.remove(a);
        assert!(f.is_empty());
    }

    #[test]
    fn a_stalled_client_queue_is_bounded_and_resumes_after_a_repaint() {
        let mut f = Fanout::with_capacity(2);
        let (_id, rx) = f.subscribe();
        for b in [b"1", b"2", b"3", b"4"] {
            f.send(b);
        }
        assert!(f.needs_repaint());
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"1".to_vec(), b"2".to_vec()]);
        // Room again, but live bytes stay held back until the full screen is sent.
        f.send(b"5");
        assert!(rx.try_recv().is_err());
        f.repaint(b"SCREEN");
        assert!(!f.needs_repaint());
        f.send(b"6");
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![b"SCREEN".to_vec(), b"6".to_vec()]);
    }

    #[test]
    fn sink_tags_events_with_the_session() {
        let (tx, rx) = mpsc::channel();
        Sink::new("s1", tx).emit(DriverEvent::Boot(BootState::Ready));
        assert_eq!(rx.recv().unwrap(), ("s1".to_string(), DriverEvent::Boot(BootState::Ready)));
    }
}
```

`crates/mirante-daemon/src/driver/codex.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    struct FakeServer {
        r: BufReader<UnixStream>,
        w: UnixStream,
    }

    impl FakeServer {
        fn next(&mut self) -> Value {
            let mut line = String::new();
            self.r.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap_or_else(|_| panic!("bad line {line:?}"))
        }
        fn expect(&mut self, method: &str) -> Value {
            let v = self.next();
            assert_eq!(v["method"], method, "{v}");
            v
        }
        fn send(&mut self, v: Value) {
            writeln!(self.w, "{v}").unwrap();
        }
        fn reply(&mut self, req: &Value, result: Value) {
            self.send(json!({"jsonrpc": "2.0", "id": req["id"], "result": result}));
        }
        fn notify(&mut self, method: &str, params: Value) {
            self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
        }
    }

    /// Connects a driver to a fake app-server; returns after the handshake.
    fn setup(resume: Option<&str>) -> (Arc<CodexDriver>, FakeServer, mpsc::Receiver<(String, DriverEvent)>, Value) {
        let (client, server) = UnixStream::pair().unwrap();
        let mut fake = FakeServer { r: BufReader::new(server.try_clone().unwrap()), w: server };
        let (tx, rx) = mpsc::channel();
        let resume = resume.map(str::to_string);
        let h = std::thread::spawn(move || {
            CodexDriver::connect(
                BufReader::new(client.try_clone().unwrap()),
                client,
                None,
                "/w/tree",
                resume.as_deref(),
                Sink::new("s1", tx),
                Duration::from_secs(5),
            )
        });
        let init = fake.expect("initialize");
        assert_eq!(init["params"]["clientInfo"]["name"], "mirante");
        fake.reply(&init, json!({}));
        let initialized = fake.expect("initialized");
        assert!(initialized.get("params").is_none());
        let thread = fake.next();
        fake.reply(&thread, json!({"thread": {"id": "th-1"}}));
        let d = h.join().unwrap().unwrap();
        (d, fake, rx, thread)
    }

    fn next_event(rx: &mpsc::Receiver<(String, DriverEvent)>, want: impl Fn(&DriverEvent) -> bool) -> DriverEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (_, ev) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("event");
            if want(&ev) {
                return ev;
            }
        }
    }

    #[test]
    fn handshake_starts_a_thread_where_the_user_approves() {
        let (_d, _fake, rx, thread) = setup(None);
        assert_eq!(thread["method"], "thread/start");
        assert_eq!(
            thread["params"],
            json!({"cwd": "/w/tree", "approvalPolicy": "on-request", "sandbox": "workspace-write", "approvalsReviewer": "user"})
        );
        assert_eq!(next_event(&rx, |_| true), DriverEvent::NativeId("th-1".into()));
        assert_eq!(next_event(&rx, |_| true), DriverEvent::Boot(BootState::Ready));
    }

    #[test]
    fn resume_uses_thread_resume_with_the_same_policy() {
        let (_d, _fake, _rx, thread) = setup(Some("th-old"));
        assert_eq!(thread["method"], "thread/resume");
        assert_eq!(thread["params"]["threadId"], "th-old");
        assert_eq!(thread["params"]["approvalPolicy"], "on-request");
        assert_ne!(thread["params"]["approvalPolicy"], "never");
    }

    #[test]
    fn send_starts_a_turn_then_steers_the_running_one_with_its_id() {
        let (d, mut fake, rx, _) = setup(None);
        d.send("five steps").unwrap();
        let start = fake.expect("turn/start");
        assert_eq!(start["params"]["input"][0]["text"], "five steps");
        assert_eq!(start["params"]["threadId"], "th-1");
        fake.reply(&start, json!({"turn": {"id": "tu-1"}}));
        fake.notify("turn/started", json!({"turn": {"id": "tu-1", "status": "inProgress"}}));
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Status { .. }));
        assert_eq!(
            ev,
            DriverEvent::Status {
                to: Status::Working,
                cause: "codex:turn/started".into(),
                turn_open: true,
                detail: None
            }
        );
        d.send("stop, say BANANA").unwrap();
        let steer = fake.expect("turn/steer");
        assert_eq!(steer["params"]["expectedTurnId"], "tu-1");
        fake.reply(&steer, json!({"turnId": "tu-1"}));
        d.interrupt().unwrap();
        let int = fake.expect("turn/interrupt");
        assert_eq!(int["params"], json!({"threadId": "th-1", "turnId": "tu-1"}));
    }

    #[test]
    fn a_rejected_steer_falls_back_to_a_new_turn() {
        let (d, mut fake, _rx, _) = setup(None);
        fake.notify("turn/started", json!({"turn": {"id": "tu-1"}}));
        std::thread::sleep(Duration::from_millis(100));
        d.send("more").unwrap();
        let steer = fake.expect("turn/steer");
        fake.send(json!({"jsonrpc": "2.0", "id": steer["id"], "error": {"code": -32600, "message": "no active turn"}}));
        let start = fake.expect("turn/start");
        assert_eq!(start["params"]["input"][0]["text"], "more");
    }

    #[test]
    fn approvals_become_permission_cards_answered_once_with_one_time_decisions() {
        let (d, mut fake, rx, _) = setup(None);
        fake.notify("turn/started", json!({"turn": {"id": "tu-1"}}));
        fake.send(json!({"jsonrpc": "2.0", "id": 41, "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "i1", "startedAtMs": 1, "command": "rm -rf build"}}));
        let card = next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { .. }));
        assert_eq!(
            card,
            DriverEvent::PermissionRequested {
                key: "41".into(),
                tool: "command".into(),
                detail: "rm -rf build".into()
            }
        );
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Status { to: Status::NeedsYou, .. }));
        assert!(matches!(ev, DriverEvent::Status { turn_open: true, .. }));
        d.answer_permission("41", true).unwrap();
        assert_eq!(fake.next(), json!({"jsonrpc": "2.0", "id": 41, "result": {"decision": "accept"}}));
        assert!(matches!(d.answer_permission("41", true), Err(DriverError::NoDialog(_))));
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Status { .. }));
        assert!(matches!(ev, DriverEvent::Status { to: Status::Working, .. }));
    }

    #[test]
    fn decisions_per_request_kind() {
        let p = json!({"permissions": {"network": {"enabled": true}}});
        assert_eq!(approval_response("item/fileChange/requestApproval", &p, false), json!({"decision": "decline"}));
        assert_eq!(
            approval_response("item/permissions/requestApproval", &p, true),
            json!({"permissions": {"network": {"enabled": true}}, "scope": "turn"})
        );
        assert_eq!(
            approval_response("item/permissions/requestApproval", &p, false),
            json!({"permissions": {}, "scope": "turn"})
        );
        assert_eq!(approval_response("execCommandApproval", &p, true), json!({"decision": "approved"}));
        for allow in [true, false] {
            let d = approval_response("item/commandExecution/requestApproval", &p, allow).to_string();
            assert!(!d.contains("ForSession") && !d.contains("Amendment"), "{d}");
        }
        assert_eq!(approval_card("execCommandApproval", &json!({"command": ["git", "push"]})).1, "git push");
        assert_eq!(
            approval_card("item/fileChange/requestApproval", &json!({"reason": "write outside"})).1,
            "write outside"
        );
    }

    #[test]
    fn unsupported_server_requests_are_refused_and_elicitations_declined() {
        let (_d, mut fake, _rx, _) = setup(None);
        fake.send(json!({"jsonrpc": "2.0", "id": 5, "method": "item/tool/requestUserInput", "params": {}}));
        let r = fake.next();
        assert_eq!((r["id"].as_i64(), r["error"]["code"].as_i64()), (Some(5), Some(-32601)));
        fake.send(json!({"jsonrpc": "2.0", "id": "e1", "method": "mcpServer/elicitation/request", "params": {}}));
        assert_eq!(fake.next(), json!({"jsonrpc": "2.0", "id": "e1", "result": {"action": "decline"}}));
    }

    #[test]
    fn late_command_completion_is_reported_and_the_stream_is_readable() {
        let (d, mut fake, rx, _) = setup(None);
        let a = d.attach(24, 80).unwrap();
        assert!(!a.input);
        fake.notify("turn/started", json!({"turn": {"id": "tu-1"}}));
        fake.notify("item/started", json!({"item": {"type": "commandExecution", "id": "c2", "command": "sleep 3"}}));
        fake.notify("item/completed", json!({"item": {"type": "agentMessage", "id": "m", "text": "BANANA"}}));
        fake.notify("turn/completed", json!({"turn": {"id": "tu-1", "status": "completed"}}));
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Status { turn_open: false, .. }));
        assert_eq!(
            ev,
            DriverEvent::Status {
                to: Status::Working,
                cause: "codex:turn/completed".into(),
                turn_open: false,
                detail: Some("turn done, 1 command(s) still finishing".into())
            }
        );
        fake.notify(
            "item/completed",
            json!({"item": {"type": "commandExecution", "id": "c2", "command": "sleep 3", "exitCode": 0}}),
        );
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Status { .. }));
        assert!(matches!(ev, DriverEvent::Status { to: Status::Idle, .. }));
        let mut seen = String::new();
        while !seen.contains("[exit 0]") {
            seen.push_str(&String::from_utf8_lossy(&a.output.recv_timeout(Duration::from_secs(5)).unwrap()));
        }
        assert!(seen.contains("$ sleep 3") && seen.contains("BANANA") && seen.contains("── turn completed ──"));
        assert!(matches!(d.input(a.id, b"x"), Err(DriverError::Unsupported(_))));
        assert!(matches!(d.trust(true), Err(DriverError::Unsupported(_))));
    }

    #[test]
    fn server_eof_is_an_exit() {
        let (_d, fake, rx, _) = setup(None);
        drop(fake);
        assert_eq!(next_event(&rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { code: None });
    }

    #[test]
    fn binary_resolution_skips_shims_and_env_scrub_never_touches_auth() {
        use std::os::unix::fs::PermissionsExt;
        let d = mirante_ipc::test_dir::TestDir::new();
        let mk = |dir: &Path| {
            std::fs::create_dir_all(dir).unwrap();
            let p = dir.join("codex");
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let shim = d.path().join(".cmuxterm/cmux-cli-shims/x");
        mk(&shim);
        let real = mk(&d.path().join("real"));
        let path = std::env::join_paths([&shim, &d.path().join("real")]).unwrap();
        assert_eq!(resolve_codex_bin(None, path.to_str()).unwrap(), real);
        assert!(resolve_codex_bin(Some("/nope/codex"), None).is_err());
        let keys =
            codex_env_to_scrub(vec!["CMUX_SOCKET".into(), "OPENAI_API_KEY".into(), "CODEX_HOME".into(), "PATH".into()]);
        assert!(keys.contains(&"CMUX_SOCKET".to_string()) && keys.contains(&"CLAUDECODE".to_string()));
        assert!(!keys.iter().any(|k| k.starts_with("OPENAI_") || k.starts_with("CODEX_") || k == "PATH"));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error: cannot find macro `json` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/driver/codex.rs`:
```rust
//! Codex driver: `codex app-server` JSON-RPC over stdio (FINDINGS Task 10, verified names).
//! - Launch hygiene: the real binary (`MIRANTE_CODEX_BIN`, else PATH without shim dirs),
//!   every Claude nested-session and `CMUX_*` variable removed, auth untouched.
//! - `thread/start` / `thread/resume` with `cwd` = the worktree, `approvalPolicy`
//!   "on-request", `sandbox` "workspace-write", `approvalsReviewer` "user": the user
//!   approves; never "never", never an automatic reviewer.
//! - Messages: `turn/steer` (with `expectedTurnId`) while a turn runs, else `turn/start`.
//!   A steer lands at the next model step; a running command keeps running (rule 9), so
//!   "stop now" is `turn/interrupt`.
//! - Approvals arrive as server→client requests and are answered once, with a one-time
//!   decision only ("accept" / "decline"; never "acceptForSession" or policy amendments).
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mirante_core::claude_launch::{env_to_scrub, KNOWN_SHIM_DIRS};
use mirante_core::model::Status;
use mirante_ipc::types::BootState;
use serde_json::{json, Value};

use super::codex_rpc::{Incoming, RpcConn};
use super::codex_status::{CodexEngine, CodexInput};
use super::{Attachment, Driver, DriverError, DriverEvent, Fanout, Sink};

pub const APPROVAL_METHODS: [&str; 5] = [
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "execCommandApproval",
    "applyPatchApproval",
];

const SCROLLBACK: usize = 64 * 1024;

/// `thread/start` (no `resume`) or `thread/resume` parameters.
pub fn thread_params(cwd: &str, resume: Option<&str>) -> Value {
    let mut p = json!({
        "cwd": cwd,
        "approvalPolicy": "on-request",
        "sandbox": "workspace-write",
        "approvalsReviewer": "user",
    });
    if let Some(id) = resume {
        p["threadId"] = json!(id);
    }
    p
}

/// The one-time answer to an approval request (schema: Codex 0.167 app-server).
pub fn approval_response(method: &str, params: &Value, allow: bool) -> Value {
    match method {
        "item/permissions/requestApproval" if allow => json!({ "permissions": params["permissions"], "scope": "turn" }),
        "item/permissions/requestApproval" => json!({ "permissions": {}, "scope": "turn" }),
        "execCommandApproval" | "applyPatchApproval" => json!({ "decision": if allow { "approved" } else { "abort" } }),
        _ => json!({ "decision": if allow { "accept" } else { "decline" } }),
    }
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Array(parts) => {
            let s: Vec<&str> = parts.iter().filter_map(|p| p.as_str().or_else(|| p["text"].as_str())).collect();
            (!s.is_empty()).then(|| s.join(" "))
        }
        _ => None,
    }
}

/// (tool, detail) for the permission card.
pub fn approval_card(method: &str, params: &Value) -> (String, String) {
    let reason = text_of(&params["reason"]);
    let (tool, detail) = match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => ("command", text_of(&params["command"])),
        "item/fileChange/requestApproval" | "applyPatchApproval" => ("file_change", text_of(&params["grantRoot"])),
        _ => ("permissions", Some(params["permissions"].to_string()).filter(|s| s != "null")),
    };
    (tool.to_string(), detail.or(reason).unwrap_or_else(|| "(no detail given)".into()))
}

/// A human-readable line for `mirante attach` (read-only stream).
pub fn render(method: &str, params: &Value) -> Option<String> {
    let item = &params["item"];
    let crlf = |s: &str| s.replace('\n', "\r\n");
    match (method, item["type"].as_str()) {
        ("turn/started", _) => Some("\r\n── turn started ──\r\n".into()),
        ("turn/completed", _) => {
            Some(format!("── turn {} ──\r\n", params["turn"]["status"].as_str().unwrap_or("completed")))
        }
        ("item/started", Some("commandExecution")) => Some(format!("$ {}\r\n", crlf(item["command"].as_str()?))),
        ("item/completed", Some("commandExecution")) => Some(format!("  [exit {}]\r\n", item["exitCode"])),
        ("item/completed", Some("agentMessage")) => Some(format!("{}\r\n", crlf(item["text"].as_str()?))),
        ("item/completed", Some("userMessage")) => Some(format!("> {}\r\n", crlf(&text_of(&item["content"])?))),
        _ => None,
    }
}

/// A feed step for the audit trail.
fn step(method: &str, params: &Value) -> Option<Value> {
    let item = &params["item"];
    match (method, item["type"].as_str()) {
        ("item/completed", Some("commandExecution")) => {
            Some(json!({ "type": "command", "command": item["command"], "exit_code": item["exitCode"] }))
        }
        ("item/completed", Some("agentMessage")) => {
            let text: String = item["text"].as_str().unwrap_or("").chars().take(500).collect();
            Some(json!({ "type": "agent_message", "text": text }))
        }
        _ => None,
    }
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn in_shim_dir(p: &Path) -> bool {
    let s = p.to_string_lossy();
    KNOWN_SHIM_DIRS.iter().any(|d| s.contains(d))
}

/// `override_bin` (`MIRANTE_CODEX_BIN`) when set, else the first executable `codex` in
/// `path_var` that is not (and does not resolve into) a known shim directory.
pub fn resolve_codex_bin(override_bin: Option<&str>, path_var: Option<&str>) -> Result<PathBuf, DriverError> {
    if let Some(bin) = override_bin.filter(|b| !b.is_empty()) {
        let p = PathBuf::from(bin);
        return if is_executable(&p) {
            Ok(p)
        } else {
            Err(DriverError::Failed(format!("MIRANTE_CODEX_BIN={bin} is not an executable file")))
        };
    }
    let path_var = path_var.unwrap_or_default();
    std::env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty() && !in_shim_dir(dir))
        .map(|dir| dir.join("codex"))
        .find(|p| is_executable(p) && std::fs::canonicalize(p).is_ok_and(|real| !in_shim_dir(&real)))
        .ok_or_else(|| DriverError::Failed(format!("no codex outside shim dirs in PATH={path_var}")))
}

/// Variables removed before spawning codex: the Claude nested-session set plus every
/// `CMUX_*` variable present. Never `OPENAI_*` / `CODEX_*` (auth).
pub fn codex_env_to_scrub(present: Vec<String>) -> Vec<String> {
    let cmux: Vec<String> = present.iter().filter(|k| k.starts_with("CMUX_")).cloned().collect();
    let mut keys = env_to_scrub(present);
    keys.extend(cmux);
    keys.sort();
    keys.dedup();
    keys
}

struct Shared {
    engine: CodexEngine,
    thread_id: Option<String>,
    /// key -> (request id, method, params)
    approvals: HashMap<String, (Value, String, Value)>,
    fanout: Fanout,
    scrollback: Vec<u8>,
    /// Last reported (status, turn open, detail), to emit only changes.
    reported: (Status, bool, Option<String>),
}

impl Shared {
    fn output(&mut self, text: &str) {
        self.fanout.send(text.as_bytes());
        self.scrollback.extend_from_slice(text.as_bytes());
        let excess = self.scrollback.len().saturating_sub(SCROLLBACK);
        self.scrollback.drain(..excess);
        if self.fanout.needs_repaint() {
            // A client fell behind and lost output (bounded queue): resend the recent stream.
            let mut again = b"\r\n[mirante: output skipped, recent stream follows]\r\n".to_vec();
            again.extend_from_slice(&self.scrollback);
            self.fanout.repaint(&again);
        }
    }

    /// The status event to emit, if anything the hub shows changed.
    fn report(&mut self, cause: String) -> Option<DriverEvent> {
        let now = (self.engine.status(), self.engine.turn_open(), self.engine.detail());
        if now == self.reported {
            return None;
        }
        self.reported = now.clone();
        Some(DriverEvent::Status { to: now.0, cause, turn_open: now.1, detail: now.2 })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

enum Work {
    Send(String),
    Interrupt,
}

pub struct CodexDriver {
    conn: Arc<RpcConn>,
    shared: Arc<Mutex<Shared>>,
    sink: Sink,
    work: Mutex<Option<mpsc::Sender<Work>>>,
    child: Mutex<Option<Child>>,
}

fn on_incoming(shared: &Mutex<Shared>, sink: &Sink, conn: &OnceLock<Arc<RpcConn>>, incoming: Incoming) {
    match incoming {
        Incoming::Notification { method, params } => {
            let status = {
                let mut s = lock(shared);
                let t = s.engine.apply(&CodexInput::Notification { method: &method, params: &params });
                if let Some(text) = render(&method, &params) {
                    s.output(&text);
                }
                s.report(t.map_or_else(|| format!("codex:{method}"), |t| t.cause))
            };
            if let Some(st) = step(&method, &params) {
                sink.emit(DriverEvent::Step(st));
            }
            if let Some(ev) = status {
                sink.emit(ev);
            }
        }
        Incoming::Request { id, method, params } if APPROVAL_METHODS.contains(&method.as_str()) => {
            let key = id.to_string();
            let (tool, detail) = approval_card(&method, &params);
            let status = {
                let mut s = lock(shared);
                s.output(&format!("[needs you] {tool}: {detail}  (mirante perms)\r\n"));
                s.approvals.insert(key.clone(), (id, method, params));
                let t = s.engine.apply(&CodexInput::ApprovalRequested(key.clone()));
                s.report(t.map_or_else(|| "codex:approval_requested".into(), |t| t.cause))
            };
            sink.emit(DriverEvent::PermissionRequested { key, tool, detail });
            if let Some(ev) = status {
                sink.emit(ev);
            }
        }
        Incoming::Request { id, method, .. } => {
            let Some(conn) = conn.get() else { return };
            // MCP elicitations are declined (never accepted on the user's behalf); other
            // server requests (user input, tool calls, auth refresh) are not supported.
            let _ = if method == "mcpServer/elicitation/request" {
                conn.respond(&id, json!({ "action": "decline" }))
            } else {
                conn.respond_error(&id, -32601, "not supported by Mirante")
            };
            sink.emit(DriverEvent::Step(json!({ "type": "server_request_declined", "method": method })));
        }
        Incoming::Closed => {
            {
                let mut s = lock(shared);
                s.engine.apply(&CodexInput::Closed);
                s.fanout = Fanout::default();
            }
            sink.emit(DriverEvent::Exited { code: None });
        }
    }
}

fn send_now(conn: &RpcConn, shared: &Mutex<Shared>, sink: &Sink, text: &str, timeout: Duration) {
    let (thread, turn) = {
        let s = lock(shared);
        (s.thread_id.clone(), s.engine.turn_id().map(str::to_string))
    };
    let input = json!([{ "type": "text", "text": text }]);
    let start = || conn.request("turn/start", json!({ "threadId": thread, "input": input }), timeout);
    let r = match turn {
        Some(t) => conn
            .request("turn/steer", json!({ "threadId": thread, "expectedTurnId": t, "input": input }), timeout)
            .or_else(|e| {
                // The turn ended between the check and the call: start a new one.
                sink.emit(DriverEvent::Step(json!({ "type": "steer_fell_back", "error": e })));
                start()
            }),
        None => start(),
    };
    if let Err(e) = r {
        sink.emit(DriverEvent::Step(json!({ "type": "send_failed", "error": e })));
    }
}

impl CodexDriver {
    /// Spawns `codex app-server` in `cwd` and connects (see [`CodexDriver::connect`]).
    pub fn spawn(
        bin: &Path,
        cwd: &str,
        resume: Option<&str>,
        env_remove: &[String],
        stderr: Stdio,
        sink: Sink,
    ) -> Result<Arc<CodexDriver>, DriverError> {
        let mut cmd = Command::new(bin);
        cmd.arg("app-server").current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(stderr);
        for k in env_remove {
            cmd.env_remove(k);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| DriverError::Failed("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| DriverError::Failed("no stdout".into()))?;
        CodexDriver::connect(BufReader::new(stdout), stdin, Some(child), cwd, resume, sink, Duration::from_secs(30))
    }

    /// Handshake over any stream pair: `initialize`, `initialized`, then `thread/start` or
    /// `thread/resume`. Emits `NativeId` and `Boot(Ready)`. On failure the driver is
    /// dropped, which kills `child`.
    pub fn connect(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        child: Option<Child>,
        cwd: &str,
        resume: Option<&str>,
        sink: Sink,
        timeout: Duration,
    ) -> Result<Arc<CodexDriver>, DriverError> {
        let shared = Arc::new(Mutex::new(Shared {
            engine: CodexEngine::new(),
            thread_id: None,
            approvals: HashMap::new(),
            fanout: Fanout::default(),
            scrollback: Vec::new(),
            reported: (Status::Idle, false, None),
        }));
        let slot: Arc<OnceLock<Arc<RpcConn>>> = Arc::default();
        let (sh, sk, sl) = (shared.clone(), sink.clone(), slot.clone());
        let conn = RpcConn::start(reader, writer, move |i| on_incoming(&sh, &sk, &sl, i));
        let _ = slot.set(conn.clone());
        let (tx, rx) = mpsc::channel();
        let driver = Arc::new(CodexDriver {
            conn: conn.clone(),
            shared: shared.clone(),
            sink: sink.clone(),
            work: Mutex::new(Some(tx)),
            child: Mutex::new(child),
        });
        let fail = |e: String| DriverError::Failed(format!("codex app-server: {e}"));
        let info =
            json!({ "clientInfo": { "name": "mirante", "title": "Mirante", "version": env!("CARGO_PKG_VERSION") } });
        conn.request("initialize", info, timeout).map_err(fail)?;
        conn.notify("initialized", Value::Null)?;
        let (method, params) = match resume {
            Some(id) => ("thread/resume", thread_params(cwd, Some(id))),
            None => ("thread/start", thread_params(cwd, None)),
        };
        let result = conn.request(method, params, timeout).map_err(fail)?;
        let thread_id = result["thread"]["id"]
            .as_str()
            .ok_or_else(|| DriverError::Failed(format!("{method} returned no thread id: {result}")))?
            .to_string();
        lock(&shared).thread_id = Some(thread_id.clone());
        sink.emit(DriverEvent::NativeId(thread_id));
        sink.emit(DriverEvent::Boot(BootState::Ready));
        std::thread::spawn(move || {
            for w in rx {
                match w {
                    Work::Send(text) => send_now(&conn, &shared, &sink, &text, timeout),
                    Work::Interrupt => {
                        let (thread, turn) = {
                            let s = lock(&shared);
                            (s.thread_id.clone(), s.engine.turn_id().map(str::to_string))
                        };
                        if let Some(turn) = turn {
                            let p = json!({ "threadId": thread, "turnId": turn });
                            if let Err(e) = conn.request("turn/interrupt", p, timeout) {
                                sink.emit(DriverEvent::Step(json!({ "type": "interrupt_failed", "error": e })));
                            }
                        }
                    }
                }
            }
        });
        Ok(driver)
    }

    fn queue(&self, w: Work) -> Result<(), DriverError> {
        lock(&self.work)
            .as_ref()
            .ok_or_else(|| DriverError::Failed("driver stopped".into()))?
            .send(w)
            .map_err(|_| DriverError::Failed("driver stopped".into()))
    }

    fn end_process(&self, grace: Duration) {
        lock(&self.work).take();
        self.conn.close();
        if let Some(mut child) = lock(&self.child).take() {
            let deadline = Instant::now() + grace;
            while Instant::now() < deadline && !matches!(child.try_wait(), Ok(Some(_))) {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for CodexDriver {
    fn drop(&mut self) {
        self.end_process(Duration::ZERO);
    }
}

impl Driver for CodexDriver {
    fn send(&self, text: &str) -> Result<(), DriverError> {
        self.queue(Work::Send(text.to_string()))
    }

    fn interrupt(&self) -> Result<(), DriverError> {
        self.queue(Work::Interrupt)
    }

    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError> {
        let (id, method, params) = lock(&self.shared)
            .approvals
            .remove(key)
            .ok_or_else(|| DriverError::NoDialog(format!("no pending approval {key}")))?;
        self.conn.respond(&id, approval_response(&method, &params, allow))?;
        let ev = {
            let mut s = lock(&self.shared);
            s.output(&format!("[{}]\r\n", if allow { "allowed once" } else { "denied" }));
            let t = s.engine.apply(&CodexInput::ApprovalAnswered(key.to_string()));
            s.report(t.map_or_else(|| "mirante:approval_answered".into(), |t| t.cause))
        };
        if let Some(ev) = ev {
            self.sink.emit(ev);
        }
        Ok(())
    }

    fn trust(&self, _accept: bool) -> Result<(), DriverError> {
        Err(DriverError::Unsupported("Codex has no trust prompt"))
    }

    fn dialog_open(&self) -> bool {
        false
    }

    fn tail_transcript(&self, _path: &Path, _offset: u64) {}

    fn attach(&self, _rows: u16, _cols: u16) -> Result<Attachment, DriverError> {
        let mut s = lock(&self.shared);
        let (id, output) = s.fanout.subscribe();
        Ok(Attachment { id, initial: s.scrollback.clone(), output, input: false })
    }

    fn input(&self, _attachment: u64, _bytes: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::Unsupported("typing into a Codex session (use mirante say)"))
    }

    fn detach(&self, attachment: u64) {
        lock(&self.shared).fanout.remove(attachment);
    }

    fn screen_tail(&self) -> String {
        let s = lock(&self.shared);
        let text = String::from_utf8_lossy(&s.scrollback).replace("\r\n", "\n");
        super::screen::tail(&text, 20)
    }

    fn stop(&self) {
        self.end_process(Duration::from_secs(3));
    }
}
```

Replace `crates/mirante-daemon/src/driver/real.rs` with:
```rust
//! The production driver factory: the real `claude` in a PTY with Mirante's scoped hook
//! settings, or the real `codex app-server`. Binaries: `MIRANTE_CLAUDE_BIN` /
//! `MIRANTE_CODEX_BIN`, else PATH without wrapper shims. The hook command:
//! `MIRANTE_HOOK_BIN`, else `mirante-hook` next to the daemon executable.
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use mirante_core::claude_launch::{launch_spec, resolve_claude_bin_from_env, settings_json, LaunchInput};
use mirante_core::model::AgentKind;
use mirante_ipc::paths::HOME_ENV;

use super::claude::{ClaudeDriver, ClaudeTiming};
use super::codex::{codex_env_to_scrub, resolve_codex_bin, CodexDriver};
use super::pty::PtySpec;
use super::{Driver, DriverError, DriverFactory, Sink, SpawnRequest};

#[derive(Debug, Clone)]
pub struct RealFactory {
    /// PTY size before any client attaches.
    pub rows: u16,
    pub cols: u16,
}

impl Default for RealFactory {
    fn default() -> Self {
        RealFactory { rows: 40, cols: 120 }
    }
}

pub fn hook_bin() -> Result<PathBuf, DriverError> {
    let p = match std::env::var_os("MIRANTE_HOOK_BIN").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => std::env::current_exe()?.with_file_name("mirante-hook"),
    };
    let ok = std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if ok {
        Ok(p)
    } else {
        Err(DriverError::Failed(format!("hook binary not found: {}", p.display())))
    }
}

/// Writes `contents` to `path` (mode 0600) inside a 0700 directory.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(path)?;
    f.write_all(contents.as_bytes())
}

fn parent_env() -> Vec<String> {
    std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect()
}

impl RealFactory {
    fn claude(&self, req: &SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        let claude = resolve_claude_bin_from_env().map_err(|e| DriverError::Failed(e.to_string()))?;
        let hook = hook_bin()?;
        let settings = req.paths.session_dir(&req.task.id).join("claude-settings.json");
        write_private(&settings, &settings_json(&hook.to_string_lossy(), &[]).to_string())?;
        let spec = launch_spec(LaunchInput {
            claude_bin: &claude,
            worktree: Path::new(&req.task.worktree_path),
            settings_path: &settings,
            task_id: &req.task.id,
            session_id: &req.session_id,
            resume: req.resume.as_deref(),
            parent_env: parent_env(),
        });
        let mut env_set = spec.env_set;
        // The hook (a child of claude) must reach this daemon, not one under another home.
        env_set.push((HOME_ENV.to_string(), req.paths.home().to_string_lossy().into_owned()));
        env_set.push(("TERM".to_string(), "xterm-256color".to_string()));
        let pty = PtySpec {
            program: spec.program,
            args: spec.args,
            cwd: spec.cwd,
            env_remove: spec.env_remove,
            env_set,
            rows: self.rows,
            cols: self.cols,
        };
        Ok(ClaudeDriver::spawn(pty, sink, ClaudeTiming::default())?)
    }

    fn codex(&self, req: &SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        let over = std::env::var("MIRANTE_CODEX_BIN").ok();
        let bin = resolve_codex_bin(over.as_deref(), std::env::var("PATH").ok().as_deref())?;
        let log = req.paths.session_dir(&req.task.id).join("codex-stderr.log");
        write_private(&log, "")?;
        let stderr = std::fs::OpenOptions::new().append(true).open(&log)?;
        let env_remove = codex_env_to_scrub(parent_env());
        let d = CodexDriver::spawn(
            &bin,
            &req.task.worktree_path,
            req.resume.as_deref(),
            &env_remove,
            Stdio::from(stderr),
            sink,
        )?;
        Ok(d)
    }
}

impl DriverFactory for RealFactory {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        match req.task.agent {
            AgentKind::Claude => self.claude(&req, sink),
            AgentKind::Codex => self.codex(&req, sink),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::test_dir::TestDir;

    #[test]
    fn private_files_are_0600_in_0700_dirs() {
        let d = TestDir::new();
        let p = d.path().join("sessions/t1/claude-settings.json");
        write_private(&p, "{}").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(p.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        write_private(&p, "{\"x\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"x\":1}");
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 76 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): codex app-server driver with user approvals, steer fallback, resume

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

## Phase 2b-iv — End to end, verification, dogfood

### Task 17: End to end with a fake `claude`; real-agent integration tests

**Files:**
- Create: `crates/mirante-cli/tests/fake-claude.sh`
- Create: `crates/mirante-cli/tests/e2e.rs`
- Create: `crates/mirante-daemon/tests/real_agents.rs`

`tests/e2e.rs` drives the real binaries (`mirante` → auto-started `mirante-daemon` → PTY → `fake-claude.sh` → `mirante-hook` → daemon) in a temp home: dispatch with a goal, status from hooks (`working` then a closed turn), native session id recorded, steer, `ls`, stop, resume on the same native id with `--permission-mode manual` and no bypass flag, archive refused while live then allowed, daemon stop. The fake shows the ready footer, emits hooks like Claude Code and starts/ends a turn per typed line. The e2e test needs every workspace binary built: `cargo test --workspace` builds them; with `-p mirante-cli` alone run `cargo build --workspace` first.

`tests/real_agents.rs` holds two `#[ignore]` tests against the real Claude Code and Codex (they need your logins and spend quota): turn → permission card answered once → file created → stop → resume → the secret is remembered (Claude); start → steer "BANANA" mid-run → turn ends → stop → resume by thread id → remembered (Codex). They run in a throwaway temp repo; the Claude one answers the trust dialog only when you set `MIRANTE_TEST_TRUST=1`.

- [ ] **Step 1: Write the files**

`crates/mirante-cli/tests/fake-claude.sh`:
```sh
#!/bin/sh
# Fake `claude` for the end-to-end test. Like Claude Code it shows the "manual mode on"
# footer when ready, reports hooks through the hook command, starts a turn for each typed
# line and ends it, and quits on /exit. It logs its arguments to $FAKE_CLAUDE_LOG.
hook() { printf '%s' "$1" | "$MIRANTE_HOOK_BIN"; }
SID=fake-session-1
SRC=startup
for a in "$@"; do [ "$a" = "--resume" ] && SRC=resume; done
echo "$*" >> "$FAKE_CLAUDE_LOG"
printf '  manual mode on\r\n'
hook "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"$SID\",\"source\":\"$SRC\"}"
while IFS= read -r line; do
  case "$line" in
    */exit*)
      hook "{\"hook_event_name\":\"SessionEnd\",\"session_id\":\"$SID\"}"
      exit 0 ;;
  esac
  hook "{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"$SID\",\"prompt\":\"typed\"}"
  printf 'echo: %s\r\n' "$line"
  hook "{\"hook_event_name\":\"Stop\",\"session_id\":\"$SID\"}"
done
```

`crates/mirante-cli/tests/e2e.rs`:
```rust
//! End to end through the real binaries (`mirante`, `mirante-daemon`, `mirante-hook`) with
//! a fake `claude` (tests/fake-claude.sh): dispatch, status from hooks, steer, stop,
//! resume on the native session id, archive, daemon stop. Everything lives in a temp dir.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use mirante_ipc::test_dir::TestDir;
use mirante_ipc::types::{method, Snapshot};
use mirante_ipc::{Client, Paths};
use serde_json::{json, Value};

fn bin(name: &str) -> PathBuf {
    // target/debug/deps/e2e-<hash> -> target/debug/<name>
    let p = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join(name);
    assert!(p.exists(), "{} missing: run `cargo build --workspace` first", p.display());
    p
}

struct Env {
    dir: TestDir,
    home: PathBuf,
    repo: PathBuf,
    log: PathBuf,
}

impl Env {
    fn new() -> Env {
        let dir = TestDir::new();
        let home = dir.path().join("h");
        let repo = dir.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q", "-b", "trunk"][..],
            &["config", "user.email", "t@example.com"],
            &["config", "user.name", "T"],
            &["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let log = dir.path().join("fake-claude.log");
        Env { dir, home, repo, log }
    }

    fn mirante(&self, args: &[&str]) -> Output {
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake-claude.sh");
        Command::new(bin("mirante"))
            .args(args)
            .env("MIRANTE_HOME", &self.home)
            .env("MIRANTE_DAEMON_BIN", bin("mirante-daemon"))
            .env("MIRANTE_HOOK_BIN", bin("mirante-hook"))
            .env("MIRANTE_CLAUDE_BIN", fake)
            .env("FAKE_CLAUDE_LOG", &self.log)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.mirante(args);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "mirante {args:?}: {text}{}", String::from_utf8_lossy(&out.stderr));
        text
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.mirante(&["daemon", "stop"]);
        let _ = &self.dir;
    }
}

/// Reads `event` notifications until `n` of kind `kind` arrived (10 s budget).
fn wait_events(sub: &mut Client, kind: &str, n: usize) -> Vec<Value> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while seen.iter().filter(|e: &&Value| e["kind"] == kind).count() < n {
        assert!(Instant::now() < deadline, "only {seen:?}");
        let note = sub.next_notification().unwrap().expect("daemon alive");
        seen.push(note.params);
    }
    seen
}

#[test]
fn dispatch_steer_stop_resume_archive_with_a_fake_claude() {
    let env = Env::new();
    assert!(env.ok(&["daemon", "status"]).contains("not running"));
    assert!(env.ok(&["daemon", "start"]).contains("started"));
    let mut sub = Client::connect(&Paths::new(&env.home).socket()).unwrap();
    sub.set_timeout(Some(Duration::from_secs(10))).unwrap();
    let _: Snapshot = sub.call(method::EVENTS_SUBSCRIBE, json!({})).unwrap();

    let repo = env.repo.to_string_lossy().into_owned();
    let out = env.ok(&["new", &repo, "--agent", "claude", "--branch", "e2e", "say", "hi"]);
    assert!(out.contains("ready"), "{out}");
    let events = wait_events(&mut sub, "turn_ended", 1);
    let statuses: Vec<&str> =
        events.iter().filter(|e| e["kind"] == "status").filter_map(|e| e["payload"]["to"].as_str()).collect();
    assert!(statuses.contains(&"working"), "{statuses:?}");
    assert!(events.iter().any(|e| e["kind"] == "native_session_id"));

    assert!(env.ok(&["say", "e2e", "again"]).contains("sent now"));
    wait_events(&mut sub, "turn_ended", 1);
    let ls = env.ok(&["ls"]);
    assert!(ls.lines().any(|l| l.contains("e2e") && l.contains("idle")), "{ls}");

    assert!(env.ok(&["stop", "e2e"]).contains("ended"));
    let out = env.ok(&["resume", "e2e"]);
    assert!(out.contains("ready (resumed)"), "{out}");
    let args = std::fs::read_to_string(&env.log).unwrap();
    assert!(args.lines().last().unwrap().contains("--resume fake-session-1"), "{args}");
    assert!(args.contains("--permission-mode manual") && !args.contains("dangerously"), "{args}");

    assert!(!env.mirante(&["archive", "e2e"]).status.success(), "archive must refuse a live session");
    env.ok(&["stop", "e2e"]);
    assert!(env.ok(&["archive", "e2e"]).contains("archived"));
    assert!(env.ok(&["ls"]).contains("no tasks"));
    assert!(env.ok(&["daemon", "stop"]).contains("stopped"));
    assert!(env.ok(&["daemon", "status"]).contains("not running"));
}

#[test]
fn bad_input_is_reported_not_swallowed() {
    let env = Env::new();
    let out = env.mirante(&["new", "/nope", "--agent", "claude", "--branch", "b", "go"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("/nope"));
    let out = env.mirante(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    let out = env.mirante(&["say", "missing-task", "hi"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("no task missing-task"));
}
```

`crates/mirante-daemon/tests/real_agents.rs`:
```rust
//! The real agents through the daemon (hub + socket + hook binary). Ignored by default:
//! they need your Claude / Codex logins and spend plan quota. Everything runs in a fresh
//! temp git repo; no permission is broadened and nothing is auto-approved (the test answers
//! the one permission it provokes, for a `touch` inside the throwaway worktree).
//!
//!   cargo build --workspace
//!   MIRANTE_TEST_TRUST=1 MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude \
//!     cargo test -p mirante-daemon --test real_agents -- --ignored --test-threads=1 claude
//!   MIRANTE_CODEX_BIN=$(dirname $(readlink -f $(which node)))/codex \
//!     cargo test -p mirante-daemon --test real_agents -- --ignored --test-threads=1 codex
//!
//! `MIRANTE_TEST_TRUST=1` is you answering "yes" to Claude's trust dialog for the temp
//! worktree; without it the Claude test stops at the dialog.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use mirante_core::model::{AgentKind, Event, Status};
use mirante_core::store::Store;
use mirante_daemon::driver::real::RealFactory;
use mirante_daemon::hub::Hub;
use mirante_daemon::server;
use mirante_ipc::test_dir::TestDir;
use mirante_ipc::types::{AnswerPermission, BootState, CreateTask, SendMessage, SendMode, StartSession, TrustAnswer};
use mirante_ipc::Paths;

struct Rig {
    hub: Arc<Hub>,
    events: mpsc::Receiver<Event>,
    paths: Paths,
    repo: PathBuf,
    _dir: TestDir,
}

fn rig() -> Rig {
    let dir = TestDir::new();
    let target = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    let hook = target.join("mirante-hook");
    assert!(hook.exists(), "run `cargo build --workspace` first");
    // Single-threaded test run (--test-threads=1): setting the variable is safe here.
    std::env::set_var("MIRANTE_HOOK_BIN", &hook);
    let paths = Paths::new(&dir.path().join("h"));
    paths.ensure_home().unwrap();
    let repo = dir.path().join("app");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "-b", "trunk"][..], &["commit", "-q", "--allow-empty", "-m", "init"]] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=T"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    let store = Store::open(&paths.database()).unwrap();
    let (hub, rx) = Hub::new(store, paths.clone(), Box::new(RealFactory::default())).unwrap();
    let hub = Arc::new(hub);
    hub.spawn_pump(rx);
    let listener = server::bind(&paths.socket()).unwrap();
    let h = hub.clone();
    std::thread::spawn(move || server::serve(h, listener, Arc::new(|| {})));
    let (_, events) = hub.subscribe().unwrap();
    Rig { hub, events, paths, repo, _dir: dir }
}

impl Rig {
    fn create(&self, agent: AgentKind, branch: &str, goal: &str) -> String {
        let v = self
            .hub
            .create_task(CreateTask {
                repo: self.repo.to_string_lossy().into_owned(),
                agent,
                branch: branch.into(),
                base: None,
                goal: goal.into(),
                title: None,
            })
            .unwrap();
        v.task.id
    }

    /// Waits for an event of `kind` on `task` matching `pred`; returns it.
    fn wait_event(&self, task: &str, kind: &str, secs: u64, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let e = self.events.recv_timeout(left).unwrap_or_else(|_| panic!("no {kind} event in {secs}s"));
            if e.task_id.as_deref() == Some(task) && e.kind == kind && pred(&e) {
                return e;
            }
        }
    }

    fn start(&self, task: &str, prompt: Option<&str>, resume: bool) -> bool {
        let mut r = self
            .hub
            .start_session(StartSession { task: task.into(), prompt: prompt.map(String::from) }, resume)
            .unwrap();
        if r.boot == BootState::TrustPrompt {
            assert_eq!(
                std::env::var("MIRANTE_TEST_TRUST").as_deref(),
                Ok("1"),
                "Claude asks to trust the temp worktree:\n{}\nrerun with MIRANTE_TEST_TRUST=1 to answer yes",
                r.screen
            );
            r = self.hub.trust(TrustAnswer { task: task.into(), accept: true }).unwrap();
        }
        assert_eq!(r.boot, BootState::Ready, "{}", r.screen);
        r.resumed
    }

    fn say(&self, task: &str, text: &str) {
        self.hub.send(SendMessage { task: task.into(), text: text.into(), mode: SendMode::Steer }).unwrap();
    }

    /// Last assistant text in the live session's transcript (read through a second store
    /// connection; WAL allows a concurrent reader).
    fn last_answer(&self, task: &str) -> String {
        let store = Store::open(&self.paths.database()).unwrap();
        let s = store.live_session(task).unwrap().expect("live session");
        let text = std::fs::read_to_string(s.transcript_path.expect("transcript path")).unwrap();
        text.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["type"] == "assistant")
            .filter_map(|v| v["message"]["content"].as_array().cloned())
            .flatten()
            .filter_map(|c| c["text"].as_str().map(String::from))
            .next_back()
            .unwrap_or_default()
    }
}

fn to(e: &Event) -> Status {
    serde_json::from_value(e.payload["to"].clone()).unwrap()
}

#[test]
#[ignore]
fn claude_turn_permission_stop_and_resume() {
    let r = rig();
    let goal = "A palavra secreta é JABUTICABA. Responda apenas OK.";
    let t = r.create(AgentKind::Claude, "real-claude", goal);
    assert!(!r.start(&t, Some(goal), false));
    r.wait_event(&t, "status", 120, |e| to(e) == Status::Working);
    r.wait_event(&t, "turn_ended", 120, |_| true);

    r.say(&t, "Use o Bash para rodar exatamente `touch allowed.txt` e diga se deu certo.");
    r.wait_event(&t, "permission_requested", 120, |_| true);
    let p = r.hub.permissions().unwrap();
    assert_eq!(p.len(), 1, "{p:?}");
    r.hub.answer_permission(AnswerPermission { id: p[0].request.id.clone(), allow: true }).unwrap();
    r.wait_event(&t, "turn_ended", 120, |_| true);
    let worktree = r.hub.get_task(&t).unwrap().task.worktree_path;
    assert!(Path::new(&worktree).join("allowed.txt").exists());

    r.hub.stop(&t).unwrap();
    assert!(r.start(&t, None, true), "resume must reuse the native session id");
    r.say(&t, "Qual é a palavra secreta? Responda só a palavra.");
    r.wait_event(&t, "turn_ended", 120, |_| true);
    assert!(r.last_answer(&t).contains("JABUTICABA"));
    r.hub.stop(&t).unwrap();
}

#[test]
#[ignore]
fn codex_steer_and_resume() {
    let r = rig();
    let goal = "Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma chamada por vez, com N de 1 a 5. Ao terminar, diga FIM.";
    let t = r.create(AgentKind::Codex, "real-codex", goal);
    assert!(!r.start(&t, Some(goal), false));
    r.wait_event(&t, "status", 60, |e| to(e) == Status::Working);
    std::thread::sleep(Duration::from_secs(8));
    r.say(&t, "Mudança de planos: pare o que está fazendo e responda apenas a palavra BANANA.");
    r.wait_event(&t, "step", 180, |e| e.payload["type"] == "agent_message" && e.payload["text"] == "BANANA");
    r.wait_event(&t, "turn_ended", 180, |_| true);
    r.wait_event(&t, "status", 60, |e| to(e) == Status::Idle);

    r.hub.stop(&t).unwrap();
    assert!(r.start(&t, None, true), "resume must reuse the thread id");
    r.say(&t, "Qual palavra você respondeu antes? Responda só a palavra.");
    r.wait_event(&t, "step", 180, |e| e.payload["type"] == "agent_message" && e.payload["text"] == "BANANA");
    r.hub.stop(&t).unwrap();
}
```

- [ ] **Step 2: Make the fake agent executable**

```bash
chmod 755 crates/mirante-cli/tests/fake-claude.sh
```

- [ ] **Step 3: Build and run the tests**

Run: `cargo build --workspace && cargo test -p mirante-cli --test e2e && cargo test -p mirante-daemon --test real_agents`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/mirante-cli/tests crates/mirante-daemon/tests
git commit -F - <<'EOF'
test: end-to-end CLI/daemon/hook run with a fake claude; ignored real-agent tests

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 18: Full verification, real-agent run, dogfood checklist

**Files:**
- Create: `docs/dogfood/2026-11-07-plan-2b-dogfood.md`

The automated part proves the plumbing; this task answers the product question (spec §1): does the cockpit replace cmux? The implementer runs Steps 1, 2 and 4; the founder runs Steps 3, 5 and 6.

- [ ] **Step 1: Run everything**

Run: `cargo build --workspace && cargo test --workspace 2>&1 | grep 'test result'`

Expected, in this order (mirante-cli lib, `mirante` bin, e2e, mirante-core, mirante-daemon lib, daemon bin, real_agents, mirante-hook lib, hook bin, mirante-ipc, spike-kit lib, then the spike-kit bins and doc tests), each line followed by `; finished in …`:
```
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 141 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 76 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 61 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
That is 303 passed and 2 ignored. If `mirante-core` gained tests after `99b8a2a`, only its line differs.

- [ ] **Step 2: Lint the new crates; check that core and spike-kit are untouched**

Run: `cargo clippy -p mirante-ipc -p mirante-hook -p mirante-daemon -p mirante-cli --all-targets 2>&1 | grep -E '^(warning|error)'`

Expected: no output.

Run: `git diff --stat <commit before Task 1> -- crates/spike-kit crates/mirante-core`

Expected:
```
 crates/spike-kit/src/bin/{mirante-hook.rs => spike-hook.rs} | 0
 1 file changed, 0 insertions(+), 0 deletions(-)
```

- [ ] **Step 3: The founder runs the real-agent tests (they spend plan quota)**

```bash
cargo build --workspace
MIRANTE_TEST_TRUST=1 MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude \
  cargo test -p mirante-daemon --test real_agents -- --ignored --test-threads=1 claude
MIRANTE_CODEX_BIN=~/.nvm/versions/node/v22.14.0/bin/codex \
  cargo test -p mirante-daemon --test real_agents -- --ignored --test-threads=1 codex
```

Expected: `test claude_turn_permission_stop_and_resume ... ok` and `test codex_steer_and_resume ... ok`. If the Claude test stops at boot, read the `screen` in the assertion message. If the TUI footer or the trust wording changed, adjust `screen::is_ready` / `screen::is_trust_dialog` (Task 13). Never add a time-based "ready" fallback: it would type the goal into whatever dialog is on screen.

- [ ] **Step 4: Write the dogfood checklist**

`docs/dogfood/2026-11-07-plan-2b-dogfood.md`:
````markdown
# Plan 2b dogfood: replace cmux with `mirante`

Spec §6 metrics this run feeds: the founder uses it instead of cmux for 2 consecutive weeks; context recreated by hand after closing and reopening = 0; wrong status = 0 in 50 hand-checked transitions; mid-run steering works on both agents or the fallback is explicit. (The "3+ external developers" metric needs an installable build and is out of scope here.)

## Setup (once)

- [ ] `cargo build --release --workspace`, then put the three binaries in one directory on PATH. The CLI finds `mirante-daemon` next to itself, and the daemon finds `mirante-hook` next to itself:
      `mkdir -p ~/.local/bin && ln -sf $PWD/target/release/{mirante,mirante-daemon,mirante-hook} ~/.local/bin/`
- [ ] In the shell profile, point at the real agents (PATH `claude` / `codex` may be cmux shims):
      `export MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude` and `export MIRANTE_CODEX_BIN=<real codex path>`.
- [ ] Open a new shell (the daemon inherits those variables when it starts), then `mirante daemon start` and `mirante daemon status` → `running`.

## Day 0 acceptance (one sitting, about 45 minutes)

Keep `mirante watch` open in one terminal for the whole run.

**A. Dispatch**
- [ ] `mirante new <repo> --agent claude --branch <yours> "<goal>"`: the trust question is shown and you answer it; `ready`; the board shows `working`, then `idle`.
- [ ] Same with `--agent codex`: no trust question; `ready`; `working` → `idle`.
- [ ] Both worktrees are under `~/.mirante/worktrees/<repo>/<branch>`, on the branch you named, based on the repo's current branch.

**B. Status: 50 hand-checked transitions (target: 0 wrong)**

For each step, write down what you see in `mirante attach` and what the board says. Run the Claude steps twice and the Codex steps once:
- a plain reply;
- a command that needs permission, allowed with `mirante allow`;
- another one, denied with `mirante deny`;
- one answered inside `mirante attach` (`mirante perms` must empty itself);
- a multi-tool turn;
- 60 s idle (Claude's idle notification must stay `idle`);
- a slash command (`/context`) and `/compact`;
- a Codex command still running when its turn completes: the board shows `working [turn done, 1 command(s) still finishing]`, then `idle`;
- `mirante stop`.

Export the trail for the log:
`sqlite3 ~/.mirante/mirante.sqlite "select seq, substr(task_id,-8), json_extract(payload,'$.from'), json_extract(payload,'$.to'), json_extract(payload,'$.cause') from events where kind='status' order by seq"`

| # | agent | step | seen in attach | board | ok? |
|---|---|---|---|---|---|
| 1 | | | | | |

**C. Talking mid-run**
- [ ] Claude: start a long multi-step task, then `mirante say <task> "stop, say BANANA"` → `sent now`. The answer is BANANA in the same turn, and the trail has `message_delivery` with `"delivery":"steered"`.
- [ ] Claude: `mirante say --queue <task> "<next>"` during a turn → `queued (#1 …)`; it is delivered right after the turn ends.
- [ ] `mirante say` while a permission dialog is open → refused with "waits on a dialog" (nothing is typed into the dialog).
- [ ] Codex: the same steer (BANANA) and queue checks; `mirante interrupt <task>` stops a running command.

**D. Permissions**
- [ ] Every dialog appears in `mirante perms` and in the board's NEEDS column. `mirante allow` picks the plain "Yes" (watch it in attach: never "always allow", never "auto mode").
- [ ] A Codex approval (ask for something outside the worktree, or with network) → card → `mirante deny` → Codex continues without it.

**E. Context survives**
- [ ] Mid-conversation, `mirante daemon stop`; `mirante ls` (restarts the daemon) shows the task `ended`; `mirante resume <task>` prints `ready (resumed)`; ask about something from before → remembered. Do this for both agents.
- [ ] `kill -9` the daemon (`pgrep mirante-daemon`) during a turn, then `mirante ls` and `mirante resume`: remembered.
- [ ] Reboot the machine once during the two weeks; resume everything; nothing recreated by hand.

**F. Cleanup**
- [ ] `mirante archive` on a task with uncommitted changes is refused; after committing, it succeeds and the branch still exists.

## Two-week log

| date | tasks dispatched | opened cmux? (why) | wrong statuses | context recreated by hand | steering fallback needed |
|---|---|---|---|---|---|
| | | | | | |

## Verdict

- [ ] 14 consecutive days without cmux
- [ ] 0 context recreated by hand
- [ ] 0 wrong statuses in the 50 checked
- [ ] steering worked on both agents (or the fallback used is written down above)
````

- [ ] **Step 5: The founder runs Setup and Day 0 (sections A–F) and fills in the tables**

A wrong status, text typed into a dialog, or lost context is a bug to fix before the two weeks start. The self-review table below names the task that owns each behaviour.

- [ ] **Step 6: Commit the checklist with the Day 0 results**

```bash
git add docs/dogfood/2026-11-07-plan-2b-dogfood.md
git commit -F - <<'EOF'
docs(dogfood): plan 2b acceptance checklist and day-0 results

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

Update and commit the two-week log the same way as it fills in.

---

## Self-review

### Spec and FINDINGS coverage

| Requirement (source) | Where | Test(s) |
|---|---|---|
| Dispatch: new task → agent → worktree; user-chosen branch, no prefix, no assumed `main` (spec §2.1) | Task 6 `create_task` (Plan 2a `WorktreeManager`), Task 12 `mirante new` | `create_makes_a_worktree_on_the_users_branch_and_a_task`, e2e `dispatch_steer_stop_resume_archive_with_a_fake_claude` |
| Status `working / needs_you / idle / ended` only from hooks and protocol events, never the terminal (spec §2.2) | Task 7 (hooks + transcript → `StatusEngine`), Task 15 (Codex notifications); the screen is read only for dialogs (Task 13 module doc) | `hooks_drive_status_turns_and_the_native_id`, `codex_status_events_drive_status_turns_and_detail`, `plain_turn` |
| Notify only when a task needs the user (spec §2.2) | Task 12 `watch` rings the bell on a new `needs_you` | manual (Task 18 §B) |
| Open a task: live terminal (spec §2.3) | Tasks 10, 13, 14 (`attach`: repaint + live bytes + typed input), Task 12 CLI raw passthrough; Codex read-only stream (Task 16) | `attach_streams_raw_bytes_both_ways_until_the_client_closes`, `attach_repaints_then_streams_and_input_reaches_the_agent`, `late_command_completion_is_reported_and_the_stream_is_readable` |
| Talk mid-run: steer now / queue for the turn end (spec §2.4; FINDINGS "Steer UI") | Task 8 `send`, Task 7 `kick`, Task 14 typing, Task 16 `turn/steer` + fallback | `steer_goes_out_now_and_queue_waits_for_the_turn_end`, `queue_on_an_idle_agent_is_delivered_at_once`, `send_starts_a_turn_then_steers_the_running_one_with_its_id`, `a_rejected_steer_falls_back_to_a_new_turn` |
| Steering confirmed from the transcript, not `UserPromptSubmit` (FINDINGS rule 2) | Task 7 `classify_steers` (Plan 2a `classify_message`) | `steer_verdict_comes_from_the_transcript_at_the_turn_end` |
| Never type into a dialog | Task 8 refusal (`NEEDS_YOU`), Task 14 `send` guard, `kick` re-queues | `steer_is_refused_while_a_dialog_is_open_but_queue_is_accepted`, `trust_prompt_waits_for_the_user_then_down_enter_accepts` |
| Permissions: persisted cards, answered once, never auto-approved (spec §2.5, §5) | Tasks 7–8 (hub), 13–14 (Claude exact options), 16 (Codex one-time decisions) | `hook_permission_request_is_persisted_and_settled_when_the_tool_runs`, `claude_permission_answer_types_the_choice_then_persists_once`, `a_failed_driver_answer_persists_nothing`, `approvals_become_permission_cards_answered_once_with_one_time_decisions`, `decisions_per_request_kind` |
| Answers given directly in the attached terminal are detected | Task 7 `settle_permissions` (PostToolUse → allowed, turn end → denied) | `hook_permission_request_is_persisted_and_settled_when_the_tool_runs`, `deny_in_the_terminal_is_settled_by_the_transcript_turn_end` |
| Exact option, never "always" / "auto", below the dialog header (FINDINGS rule 6) | Task 13 `option_key`, `dialog_matches` | `exact_one_time_options_only`, `lookalike_options_above_the_dialog_are_ignored`, `permission_answer_types_the_exact_option_for_the_matching_dialog` |
| Context survives: goal, history, native session id on disk; reopen resumes natively (spec §2.6, FINDINGS S4) | Task 7 resume path, Task 9 recovery, Task 14 `--resume`, Task 16 `thread/resume` | `process_exit_ends_the_session_expires_permissions_and_allows_resume`, `restart_ends_live_sessions_expires_permissions_and_keeps_resume_possible`, `resume_uses_thread_resume_with_the_same_policy`, e2e, `claude_turn_permission_stop_and_resume` / `codex_steer_and_resume` (ignored) |
| Hook delivery survives a down daemon | Task 4 spool, Task 9 ingest | `daemon_down_spools_one_private_line_per_event`, `spooled_session_start_records_the_native_id_and_the_spool_is_consumed` |
| One owner of state; Unix socket 0600; peer uid; no TCP (spec §4, §5) | Task 10 | `socket_is_private_and_the_peer_is_us`, Task 10 smoke test |
| One driver interface; unsupported is a typed result (spec §4) | Task 5 trait, Task 16 (`trust`, `input` on Codex) | `errors_map_to_typed_rpc_codes`, `late_command_completion_is_reported_and_the_stream_is_readable` |
| Events carry `seq` + `boot_id`; clients reconcile from snapshots (spec §4) | Task 6 `subscribe`, Task 10 `events.subscribe`, Task 12 `watch` (re-snapshot per event) | `subscribers_get_every_event_after_the_snapshot`, `subscribe_sends_a_snapshot_then_events` |
| State persisted on every transition (spec §4) | Every hub mutation goes through a `Store` method (Plan 2a: state + audit event in one transaction) | `hooks_drive_status_turns_and_the_native_id` (turn records), recovery tests |
| Hooks scoped by env var; no global install (spec §4) | Task 14 per-session `--settings` file, `MIRANTE_TASK_ID`; Task 4 hook does nothing without it | `without_a_task_id_nothing_happens`, e2e (`--permission-mode manual`, no bypass flag) |
| Worktree removed on archive, branch kept (spec §4) | Task 6 `archive` (refuses live sessions and dirty worktrees) | `archive_removes_the_worktree_keeps_the_branch_and_hides_the_task`, e2e |
| Per-task lock, no orphaned lock, failures reach a final state (spec §8) | Store lock (Plan 2a) + Task 7 spawn-failure path | `start_takes_the_per_task_lock_and_boot_ready_delivers_the_first_prompt`, `spawn_failure_releases_the_lock_and_ends_the_session` |
| Parse errors never silently empty (spec §8) | Task 7 `hook_unparseable` event + RPC error, Task 10 parse errors answered | `hooks_for_unknown_or_ended_sessions_are_logged_not_applied`, `garbage_lines_get_a_parse_error_and_the_connection_survives` |
| Trust dialog shown to the user, never accepted silently (FINDINGS rule 5) | Task 14 watcher reports it; Task 7 `trust`; Task 12 CLI asks `[y/N]` | `trust_is_only_answered_when_the_prompt_is_open`, `trust_prompt_waits_for_the_user_then_down_enter_accepts`, `declining_trust_ends_the_process` |
| Real binaries, env scrub, never auth vars, `--permission-mode manual`, additive `--settings` (FINDINGS rule 4; Codex launch hygiene) | Task 14 (via Plan 2a `launch_spec`), Task 16 | `binary_resolution_skips_shims_and_env_scrub_never_touches_auth`, e2e argument check |
| Codex `approvalPolicy` not `never`; the user approves | Task 16 `thread_params` | `handshake_starts_a_thread_where_the_user_approves`, `resume_uses_thread_resume_with_the_same_policy` |
| Codex: open commands after `turn/completed` (FINDINGS rule 8) | Task 15 engine, shown as `detail` (Tasks 6/12) | `command_finishing_after_turn_completed_keeps_working_with_detail`, `late_command_completion_is_reported_and_the_stream_is_readable`, `table_shows_status_needs_and_detail` |
| Codex: steer lands at the next step; stop-now is interrupt (FINDINGS rule 9) | Task 16 `turn/interrupt`, Task 12 `mirante interrupt` | `send_starts_a_turn_then_steers_the_running_one_with_its_id` |
| Resumed Claude session must not replay old turns (Plan 2a risk note) | Task 7 `follow_transcript` starts at the file's end | `transcript_path_starts_the_tailer_at_the_end_of_an_existing_file` |
| Tail loops while `more`, never while offset < length (Plan 2a API) | Task 14 tail thread | `transcript_lines_and_offsets_are_emitted_from_the_given_offset` |
| Process lifecycle: killed on drop, exit reported | Tasks 13, 14, 16 | `dropping_the_process_kills_it_and_resize_reaches_the_grid`, `stop_types_exit_and_the_driver_reports_the_exit`, `server_eof_is_an_exit` |
| CLI commands from the brief (`new`, `ls`, `attach`, `say [--queue]`, `perms`, `allow`/`deny`, `stop`, `resume`, `archive`, `watch`, `daemon start/stop/status`) plus `show`, `interrupt` | Tasks 11–12 | `simple_commands`, `new_takes_flags_anywhere_and_joins_the_goal`, e2e |
| Tests sandboxed in temp dirs; real-agent tests `#[ignore]` with documented commands | All tasks; Task 17 | every test uses `TestDir` / temp git repos; `real_agents.rs` header |
| Spec §6 metrics | Task 18 checklist | manual |

Gaps, deliberately left to Plan 3 or later (also listed under "Out of scope"): editable / reorderable queue, step feed and per-turn diff, notification suppression for the task on screen, the steer verdict shown in a UI (recorded as `message_delivery` events now), quota display, cost ceilings, token redaction in logs, explicit "from the human operator" labels on mid-run messages (Claude receives them as typed prompts and Codex as user input, which is what they are), and a `capabilities()` method (each operation returns a typed `Unsupported` instead, and `attach` reports whether input is accepted; add the method when the UI needs to grey out buttons).

### Placeholder scan

No "TBD", "TODO", "implement later" or "similar to Task N". Every code step shows complete file contents; every run step has the command and the output it produced in the replay. `RealFactory` returns a typed `Unsupported` in Tasks 10–15: that is shipped behaviour of those intermediate commits (the CLI reports it with code 3), not a placeholder, and Tasks 14 and 16 replace it.

### Type and name consistency

- Wire types (Task 2) are the only ones crossing the socket: `TaskView`, `StartResult { task, boot, resumed, screen }`, `SendResult { delivered, queued }`, `PermissionView { request, task_title, branch }`, `AttachResult { input }`, `HookParams { task, payload, ts_ms }`, `Snapshot { boot_id, seq, tasks }`, `DaemonStatus`. The hub (Tasks 6–9), the server dispatch (Task 10), the hook (Task 4) and the CLI (Tasks 11–12) use exactly these.
- `Driver` (Task 5) is implemented by `FakeDriver` (Task 5), `ClaudeDriver` (Task 14) and `CodexDriver` (Task 16) with the same eleven methods; `DriverEvent` variants emitted by the drivers are the ones `State::handle` (Task 7) matches.
- `Sink::new(session_id, tx)` / `Sink::emit` (Task 5) are the only way drivers report; `Hub::handle_driver_event` / `drain` / `spawn_pump` (Task 7) consume them.
- `Live` fields (Task 6) are used by Task 7 (`engine`, `turn_id`, `native_id`, `transcript_path`, `boot`, `detail`, `queue`, `steers`, `transcript`) and Task 8 (`perm_keys`, `driver`, `agent`).
- `CodexEngine::{apply, status, turn_id, turn_open, detail}` and `CodexInput` (Task 15) are what `CodexDriver` (Task 16) calls. `RpcConn::{start, request, notify, respond, respond_error, close}` and `Incoming` (Task 15) likewise.
- `screen::{has_permission_prompt, option_key, is_trust_dialog, is_ready, dialog_matches, tail}` (Task 13) are what `ClaudeDriver` and `CodexDriver::screen_tail` call. `PtyProcess::{spawn, write, grid, repaint, resize, subscribe, unsubscribe, exited, kill, wait_exit}` (Task 13) are what `ClaudeDriver` calls.
- Environment variables: `MIRANTE_HOME` (`paths::HOME_ENV`), `MIRANTE_TASK_ID` (`claude_launch::TASK_ENV`), `MIRANTE_CLAUDE_BIN`, `MIRANTE_CODEX_BIN`, `MIRANTE_HOOK_BIN`, `MIRANTE_DAEMON_BIN`, `MIRANTE_TEST_TRUST` (test only).

### Decisions the founder should know

1. **No async runtime.** std threads, one per connection and per agent stream. Simpler to test; a local daemon has a handful of clients. tokio was allowed, not needed.
2. **A fourth crate, `mirante-ipc`.** The daemon, CLI and hook share the wire types and framing; the hook and CLI stay synchronous and small.
3. **Attach framing.** Same socket: a JSON-RPC `attach` response, then raw bytes both ways. Detach is client-side (Ctrl-]). The PTY is resized to the attaching terminal (one size at a time); resizing mid-attach needs a re-attach.
4. **The daemon owns the agent processes.** Stopping or crashing the daemon ends every session; recovery marks them `Ended` and `mirante resume` restores the context natively. Pending permission cards of an ended session expire as `denied` (the dialog died with the process); a restart therefore does not keep an *answerable* card, which is how spec §2.5's "survives a restart" is read here.
5. **Trust UX.** `mirante new` / `resume` show Claude's trust dialog and ask `[y/N]`; `--trust` is the explicit yes up front. Every new worktree is a new folder for Claude, so expect the question once per task.
6. **Queue is daemon-side.** For both agents "queue" means Mirante holds the message and delivers it when the turn ends, rather than relying on Claude's own type-ahead. Queued messages are in memory: when a session ends they are reported in a `queue_dropped` event, not re-sent.
7. **Ready needs the input footer.** No time-based fallback: an unknown startup dialog leaves the task "starting" (attach and handle it) instead of receiving typed text.

### Risks

- **Claude TUI text is a moving target.** Ready (`manual mode on`), trust (`trust this folder` / `Do you trust`) and permission dialogs (`Do you want to …?`, `1. Yes`) are matched on 2.1.285 screens; the trust wording was inferred from the Week-1 notes ("Yes, I trust this folder") and is first exercised for real in Task 18 Step 3. A Claude update can break boot detection (safe failure: the task stays "starting").
- **Codex approvals were never exercised live.** Their request/response shapes come from the generated 0.167 schema, not from a run (Week 1 used `approvalPolicy: "never"`). Task 18 §D is the first real approval; check `codex-stderr.log` under `~/.mirante/sessions/<task>/` if it misbehaves.
- **Codex terms review is still open** (FINDINGS): driving `codex app-server` with a ChatGPT login is the documented IDE surface but not yet reviewed for Mirante.
- **The steer verdict is only in the audit trail** in 2b (`message_delivery` events); the CLI prints "sent now", not "steered" vs "queued".
- **Linux is compiled, not run.** The `SO_PEERCRED` branch and `stat`/`nc` smoke commands are macOS-tested only.
- **Spike settings.** After Task 1, `target/debug/mirante-hook` is the forwarding hook; re-running a Week-1 spike needs its settings pointed at `target/debug/spike-hook`.
- **Messages are single-line.** Newlines are collapsed to spaces before typing into Claude (an Enter would submit early); multi-line prompts need bracketed paste, untested.
