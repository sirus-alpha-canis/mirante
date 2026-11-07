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
  src/driver/proctree.rs                       # Proc (pid + start time), descendants, kill_tree (Task 13)
  src/driver/screen.rs                         # Claude TUI: live dialog / trust / input prompt (UI actions only, never status)
  src/driver/termq.rs                          # terminal queries (CPR, DA1, DSR, OSC 10/11) answered by the daemon
  src/driver/paste.rs                          # sanitize_for_pty, bracketed paste, `/` and `!` rules
  src/driver/pty.rs                            # PtyProcess: PTY + vt100 mirror + query answers + fan-out + tree kill
  src/driver/claude.rs                         # ClaudeDriver: ready/trust, re-checking typist, answers, transcript tail
  src/driver/codex_status.rs                   # CodexEngine: our thread's status from app-server traffic (rule 8)
  src/driver/codex_rpc.rs                      # RpcConn: JSON-RPC over stdio, bytes in, writer thread out
  src/driver/codex.rs                          # CodexDriver: auto-edits handshake, steer/start, approvals, read-only stream
  src/memguard/{mod,sampler,policy,orphans}.rs # memory sampling, the pressure ladder, orphan sweep (Task 18)
  src/hub/mod.rs                               # Hub, State, Live, HubError, task CRUD, publish/subscribe, testkit
  src/hub/session.rs                           # start/resume/trust/stop, hooks, driver events, queue pump, verdicts
  src/hub/messages.rs                          # steer/queue send, permission list/answer
  src/hub/memory.rs                            # memory guard thread, hibernate/wake, doctor (Task 19)
  src/recover.rs                               # daemon start: end stale sessions, ingest hook spool
  src/server.rs                                # socket (0600, peer uid), dispatch, subscribe, attach upgrade
  tests/real_agents.rs                         # #[ignore] real Claude / Codex through the daemon
crates/mirante-cli/                            # bin `mirante` + lib `mirante_cli`
  src/lib.rs, src/args.rs, src/render.rs       # parser, tables
  src/term.rs, src/daemon_ctl.rs               # raw mode / window size, auto-start
  src/main.rs                                  # commands
  tests/fake-claude.sh, tests/e2e.rs           # end to end through the real binaries with a fake claude
docs/dogfood/2026-11-07-plan-2b-dogfood.md     # Task 21: the founder's acceptance checklist and 2-week log
docs/dogfood/memory-stress.md                  # Task 18: manual memory stress procedure (close apps first)
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
| 13 | mirante-daemon / mirante-cli / mirante-ipc | 92 / 13 / 22 |
| 14 | mirante-daemon | 114 |
| 15 | mirante-daemon | 133 |
| 16 | mirante-daemon | 147 |
| 17 | mirante-daemon | 163 |
| 18 | mirante-daemon | 178 |
| 19 | mirante-daemon / mirante-cli / mirante-ipc | 185 / 14 / 22 |
| 20 | e2e 7; real_agents 2 ignored | — |
| 21 | workspace | 464 passed, 2 ignored (14 cli + 7 e2e + 157 core + 185 daemon + 2 daemon_signals + 14 hook + 2 hook binary + 22 ipc + 61 spike-kit) |

Tasks 13–21 (rows above) were replayed on top of `2b8030d` (the robustness round: core 157, daemon lib 80, workspace 350 before Task 13).

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

## Phase 2b-ii — Driver interface v2 and the Claude driver

Tasks 13–21 were revised on 2026-11-07 (OSS-comparison findings for both drivers, a memory guard, more end-to-end cases) and replayed in order in a scratch clone on top of `2b8030d`: each "fails" step failed with the quoted first error, each "passes" step passed with the quoted counts (the daemon suite three times or more per task, clippy clean at every commit), and every code block below was compared byte for byte with the replayed commit by script.

### Task 13: Driver interface v2, process trees, hub plumbing

**Files:**
- Create: `crates/mirante-daemon/src/driver/proctree.rs`
- Modify: `crates/mirante-daemon/src/driver/mod.rs`, `crates/mirante-daemon/src/driver/fake.rs`
- Modify: `crates/mirante-daemon/src/hub/mod.rs`, `crates/mirante-daemon/src/hub/session.rs`
- Modify: `crates/mirante-ipc/src/types.rs`
- Modify: `crates/mirante-cli/src/render.rs`, `crates/mirante-cli/src/args.rs`, `crates/mirante-cli/src/main.rs`

Everything the two drivers (Tasks 14–17) and the memory guard (Tasks 18–19) plug into, landed once so those tasks touch only their own files.

- **Process trees (`proctree`).** A process is its pid *and* its start time (`Proc`): a pid is reused as soon as its process is reaped, so nothing is signaled unless the start time still matches. Agents run as the leader of their own process group (the PTY child calls `setsid`; Task 17 spawns `codex app-server` with `process_group(0)`), so one `killpg` reaches every child that stayed in the group; children that left it (`setsid`, `setpgrp`) are found by walking parent links (macOS `proc_listchildpids` + `proc_pidinfo(PROC_PIDTBSDINFO)`; Linux `/proc/<pid>/stat`). `kill_tree` sends SIGTERM to the group and every descendant, re-walks the tree on every poll during the grace period (a process forked while dying is caught), sends SIGKILL to what is left, and verifies. A dead root's pid is never walked (it may name another process now). Every driver's `stop` is a `kill_tree`: no more Esc + `/exit`.
- **Interface v2.** `DriverError::NotNow` (e.g. interrupt with no running turn) and `DriverError::NativeGone` (Codex: the thread to resume has no rollout) both map to RPC `CONFLICT`; `NativeGone`'s message says `mirante resume <task> --fresh` starts a new conversation. New events: `PermissionResolved { key }` (Codex withdrew a request: the hub settles that card, and only that card, as denied with cause `codex:request_resolved`), `DeliveryAborted { text, reason }` (the typist did not type a message after all: it goes back to the front of the queue, event `message_requeued`), `ResumeFailed { reason }` (event `resume_failed` with the `--fresh` hint; Mirante never starts a fresh conversation on its own), `Usage(String)` (Codex quota/tokens, kept in memory and shown in `mirante ls`; not written to the audit trail). `Driver::session_started` (the hub forwards this session's `SessionStart` hook; Claude readiness needs it) and `Driver::root` (the agent's root `Proc`, for memory sampling) have no-op defaults.
- **Hold flag.** Each `Sink` carries a flag the hub sets while the session has a pending permission request (`State::sync_holds`, run at the start of every `publish`, so every mutation refreshes it). The Claude typist re-checks it right before typing (Task 15): text typed while a dialog waits could answer it.
- **Native id from any fenced hook.** A hook stamped with the live Mirante session (`MIRANTE_SESSION_ID`, the fence from the robustness round) carries Claude's real session id; it is adopted even when it changed (`/clear`, a resume that forks), never dropped as `hook_orphan`. Unstamped hooks keep the old rule.
- **CLI.** `mirante ls` gets a `MODE` column (`manual` for Claude, `auto-edits` for Codex; `mirante show` explains it), Codex usage after the status note, `archiving` for a task whose worktree is being removed, and `mirante resume <task> --fresh` (an explicit fresh conversation; `session.start`). A resume whose agent exits at once tells the user about `--fresh`.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/driver/proctree.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::sync::Mutex;

    /// Records every signal, then sends it.
    struct Recorder {
        calls: Mutex<Vec<(i32, i32, bool)>>,
    }

    impl Signaler for Recorder {
        fn signal(&self, pid: i32, sig: i32) -> bool {
            self.calls.lock().unwrap().push((pid, sig, false));
            Kill.signal(pid, sig)
        }
        fn signal_group(&self, pgid: i32, sig: i32) -> bool {
            self.calls.lock().unwrap().push((pgid, sig, true));
            Kill.signal_group(pgid, sig)
        }
    }

    /// A shell in its own process group (like an agent), reaped by a background thread so
    /// it never lingers as a zombie.
    fn spawn_group(script: &str) -> (Proc, std::thread::JoinHandle<()>) {
        use std::os::unix::process::CommandExt;
        let mut child: Child = Command::new("/bin/sh").args(["-c", script]).process_group(0).spawn().unwrap();
        let pid = child.id() as i32;
        let p = proc_of(pid).expect("just started");
        (p, std::thread::spawn(move || drop(child.wait())))
    }

    fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_process_is_its_pid_and_start_time() {
        let me = proc_of(std::process::id() as i32).unwrap();
        assert!(alive(me));
        assert!(!alive(Proc { pid: me.pid, start: me.start + 1 }), "another start time is another process");
        assert_eq!(proc_of(-5), None);
        assert_eq!(proc_of(0), None);
    }

    #[test]
    fn descendants_include_children_that_left_the_group() {
        // The grandchild calls setpgrp: out of the agent's process group, still below it.
        let (root, reaper) = spawn_group("perl -e 'setpgrp(0,0); sleep 30' & sleep 30 & wait");
        wait_until("one descendant in the group and one outside it", || {
            let groups: Vec<Option<i32>> = descendants(root).iter().map(|p| pgid(p.pid)).collect();
            groups.contains(&Some(root.pid)) && groups.iter().any(|g| g.is_some_and(|g| g != root.pid))
        });
        let r = kill_tree(root, Duration::from_secs(2), &Kill);
        assert!(r.survivors.is_empty(), "{r:?}");
        reaper.join().unwrap();
    }

    #[test]
    fn kill_tree_ends_a_tree_that_forks_while_dying_and_ignores_sigterm() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let pidfile = dir.path().join("forked.pid");
        let (r1, r2) = (dir.path().join("r1"), dir.path().join("r2"));
        // Child 1 ignores SIGTERM. Child 2 forks a new sleeper (in its own group) when told
        // to stop, records its pid, and keeps running. Each says when its trap is set.
        let script = format!(
            "sh -c 'trap \"\" TERM; touch {r1}; while :; do sleep 0.1; done' & \
             sh -c 'trap \"perl -e \\\"setpgrp(0,0); sleep 60\\\" & echo \\$! > {p}\" TERM; touch {r2}; while :; do sleep 0.1; done' & \
             wait",
            p = pidfile.display(),
            r1 = r1.display(),
            r2 = r2.display()
        );
        let (root, reaper) = spawn_group(&script);
        wait_until("both traps set", || r1.exists() && r2.exists());
        let rec = Recorder { calls: Mutex::new(Vec::new()) };
        let r = kill_tree(root, Duration::from_millis(800), &rec);
        assert!(r.survivors.is_empty(), "{r:?}");
        assert!(r.killed >= 1, "the TERM-ignoring child needed SIGKILL: {r:?}");
        let forked: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(proc_of(forked).is_none(), "the child forked during the kill is gone too");
        assert!(rec.calls.lock().unwrap().iter().any(|(pid, _, group)| *pid == forked && !group));
        reaper.join().unwrap();
    }

    #[test]
    fn a_reused_pid_is_never_signaled() {
        let rec = Recorder { calls: Mutex::new(Vec::new()) };
        let me = proc_of(std::process::id() as i32).unwrap();
        // Our own pid with a start time that is not ours: a "reused" pid.
        let stale = Proc { pid: me.pid, start: me.start.wrapping_sub(1) };
        let r = kill_tree(stale, Duration::ZERO, &rec);
        assert!(rec.calls.lock().unwrap().is_empty(), "{:?}", rec.calls.lock().unwrap());
        assert_eq!((r.terminated, r.killed), (0, 0));
        assert!(alive(me));
    }
}
```

Replace `crates/mirante-daemon/src/driver/mod.rs` with:
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use mirante_core::model::{Status, Task};
use mirante_ipc::types::BootState;
use mirante_ipc::Paths;
use serde_json::Value;

#[cfg(test)]
pub mod fake;
pub mod proctree;
pub mod real;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverError {
    /// The agent cannot do this (e.g. a trust prompt on Codex).
    Unsupported(&'static str),
    /// The dialog the operation targets is not on screen / not pending.
    NoDialog(String),
    /// The operation does not apply now (e.g. an interrupt with no running turn).
    NotNow(String),
    /// The native conversation to resume is gone (Codex: no rollout for the thread). The
    /// user may start a fresh conversation; Mirante never does so on its own.
    NativeGone(String),
    Failed(String),
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DriverError::Unsupported(what) => write!(f, "not supported by this agent: {what}"),
            DriverError::NoDialog(m) => write!(f, "no matching dialog: {m}"),
            DriverError::NotNow(m) => write!(f, "{m}"),
            DriverError::NativeGone(m) => {
                write!(f, "the agent's conversation is gone ({m}); `mirante resume <task> --fresh` starts a new one")
            }
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
    /// Codex: request `key` no longer waits for an answer (the server resolved it, its turn
    /// ended, or the process exited). The hub settles its card if it is still pending.
    PermissionResolved {
        key: String,
    },
    /// Codex: the thread id, used for `thread/resume`.
    NativeId(String),
    Boot(BootState),
    /// A message handed to `send` was not typed after all (a dialog appeared, a permission
    /// became pending): the hub puts it back at the front of the queue.
    DeliveryAborted {
        text: String,
        reason: String,
    },
    /// Resuming the native conversation failed (Claude exited right away). The hub records
    /// `resume_failed`; a fresh start is the user's call (`mirante resume --fresh`).
    ResumeFailed {
        reason: String,
    },
    /// Codex: quota / token usage, one line for `mirante ls`.
    Usage(String),
    /// A step for the feed and audit trail (command finished, agent message, ...).
    Step(Value),
    Exited {
        code: Option<i32>,
    },
}

/// Channel end a driver emits into. Every event is tagged with the driver's session id so
/// late events from an ended session are ignored. The sink also carries the hub's hold flag
/// for the session (see [`Sink::held`]).
#[derive(Clone)]
pub struct Sink {
    session_id: String,
    tx: mpsc::Sender<(String, DriverEvent)>,
    hold: Arc<AtomicBool>,
}

impl Sink {
    pub fn new(session_id: &str, tx: mpsc::Sender<(String, DriverEvent)>) -> Sink {
        Sink { session_id: session_id.to_string(), tx, hold: Arc::default() }
    }

    pub fn emit(&self, event: DriverEvent) {
        let _ = self.tx.send((self.session_id.clone(), event));
    }

    /// True while the hub has a pending permission request for this session: typed text
    /// could answer that dialog, so nothing may be typed.
    pub fn held(&self) -> bool {
        self.hold.load(Ordering::SeqCst)
    }

    /// The flag behind [`Sink::held`]; the hub keeps a clone and sets it.
    pub fn hold_flag(&self) -> Arc<AtomicBool> {
        self.hold.clone()
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
    /// Ends the agent and every process below it (SIGTERM to the tree, SIGKILL after a
    /// grace period; see [`proctree::kill_tree`]). Blocks until they are gone; the driver
    /// still emits `Exited`.
    fn stop(&self);
    /// Claude: the hub saw this session's `SessionStart` hook (one of the readiness
    /// conditions). Must not block.
    fn session_started(&self) {}
    /// The agent's root process (memory sampling, orphan checks), when there is one.
    fn root(&self) -> Option<proctree::Proc> {
        None
    }
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

    #[test]
    fn the_hold_flag_is_shared_between_clones() {
        let (tx, _rx) = mpsc::channel();
        let sink = Sink::new("s1", tx);
        let driver_side = sink.clone();
        assert!(!driver_side.held());
        sink.hold_flag().store(true, Ordering::SeqCst);
        assert!(driver_side.held());
    }
}
```

In `crates/mirante-daemon/src/hub/session.rs`, add `use mirante_ipc::types::SendMessage;` to the imports of the test module, and add these tests at the end of it (before `compact_keeps_what_classification_reads`):
```rust
    fn kinds_payloads(k: &Kit, task: &str, kind: &str) -> Vec<Value> {
        let st = k.hub.lock();
        st.store.task_events(task).unwrap().into_iter().filter(|e| e.kind == kind).map(|e| e.payload).collect()
    }

    #[test]
    fn a_resolved_codex_request_settles_only_its_own_card() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::PermissionRequested { key: "0".into(), tool: "command".into(), detail: "ls".into() });
        k.emit(DriverEvent::PermissionRequested { key: "7".into(), tool: "command".into(), detail: "pwd".into() });
        k.emit(DriverEvent::PermissionResolved { key: "0".into() });
        let left: Vec<String> = k.pending(&t.id).into_iter().map(|p| p.detail).collect();
        assert_eq!(left, vec!["pwd".to_string()]);
        let settled = kinds_payloads(&k, &t.id, "permission_settled");
        assert_eq!(settled.len(), 1);
        assert_eq!(
            (settled[0]["answer"].as_str(), settled[0]["cause"].as_str()),
            (Some("denied"), Some("codex:request_resolved"))
        );
        // Unknown or repeated keys change nothing.
        k.emit(DriverEvent::PermissionResolved { key: "0".into() });
        k.emit(DriverEvent::PermissionResolved { key: "99".into() });
        assert_eq!(k.pending(&t.id).len(), 1);
        assert_eq!(k.hub.lock().live[&t.id].perm_keys.len(), 1);
    }

    #[test]
    fn an_aborted_delivery_goes_back_to_the_front_of_the_queue() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        k.hub.send(SendMessage { task: t.id.clone(), text: "steer me".into(), mode: SendMode::Steer }).unwrap();
        k.hub.send(SendMessage { task: t.id.clone(), text: "later".into(), mode: SendMode::Queue }).unwrap();
        k.emit(DriverEvent::DeliveryAborted { text: "steer me".into(), reason: "a dialog appeared".into() });
        {
            let st = k.hub.lock();
            let l = &st.live[&t.id];
            assert_eq!(l.queue.iter().cloned().collect::<Vec<_>>(), vec!["steer me".to_string(), "later".to_string()]);
            assert!(l.steers.is_empty());
        }
        let ev = kinds_payloads(&k, &t.id, "message_requeued");
        assert_eq!(ev, vec![json!({"text": "steer me", "reason": "a dialog appeared"})]);
        // At the turn end the requeued message goes out first.
        k.hook(&t.id, json!({"hook_event_name": "Stop"}));
        assert_eq!(k.calls().last().map(String::as_str), Some("send:steer me"));
    }

    #[test]
    fn a_failed_resume_is_recorded_and_fresh_start_stays_the_users_call() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.emit(DriverEvent::ResumeFailed { reason: "exited after 0.4 s".into() });
        k.emit(DriverEvent::Exited { code: Some(1) });
        let ev = kinds_payloads(&k, &t.id, "resume_failed");
        assert_eq!(ev.len(), 1);
        assert!(ev[0]["hint"].as_str().unwrap().contains("--fresh"));
        // Nothing was started on its own.
        assert_eq!(k.hub.status().live_sessions, 0);
        assert_eq!(k.factory.resumes.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_vanished_native_conversation_fails_the_resume_with_a_typed_error() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        k.emit(DriverEvent::NativeId("thr-gone".into()));
        k.hub.stop(&t.id).unwrap();
        *k.factory.fail_with.lock().unwrap() = Some(DriverError::NativeGone("no rollout found for thr-gone".into()));
        let r = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, true);
        assert!(matches!(r, Err(HubError::Driver(DriverError::NativeGone(_)))), "{r:?}");
        assert_eq!(kinds_payloads(&k, &t.id, "resume_failed").len(), 1);
        assert_eq!(k.hub.status().live_sessions, 0);
        // The user chooses a fresh start explicitly.
        *k.factory.fail_with.lock().unwrap() = None;
        let fresh = k.hub.start_session(StartSession { task: t.id.clone(), prompt: None }, false).unwrap();
        assert!(!fresh.resumed);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned(), Some(None));
    }

    #[test]
    fn usage_reports_show_in_the_task_view() {
        let k = Kit::new();
        let t = k.started(AgentKind::Codex, "c").task;
        assert_eq!(k.hub.get_task(&t.id).unwrap().usage, None);
        k.emit(DriverEvent::Usage("5h 42% · 31k tokens".into()));
        assert_eq!(k.hub.get_task(&t.id).unwrap().usage.as_deref(), Some("5h 42% · 31k tokens"));
        assert!(kinds_payloads(&k, &t.id, "usage").is_empty(), "usage is not written to the audit trail");
    }

    #[test]
    fn a_pending_permission_holds_the_drivers_typing() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let sink = k.factory.last_sink();
        assert!(!sink.held());
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t.id, perm("t1", "ls"));
        assert!(sink.held());
        let id = k.pending(&t.id)[0].id.clone();
        k.hub.answer_permission(mirante_ipc::types::AnswerPermission { id, allow: true }).unwrap();
        assert!(!sink.held());
    }

    #[test]
    fn session_start_reaches_the_driver_and_fenced_hooks_update_the_native_id() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        let session = k.hub.lock().live[&t.id].session_id.clone();
        k.hook_session(
            &t.id,
            json!({"hook_event_name": "SessionStart", "session_id": "nat-1", "source": "startup"}),
            &session,
        );
        assert!(k.calls().contains(&"session_started".to_string()));
        // Claude's own id changed under the same Mirante session: adopted, not dropped.
        k.hook_session(
            &t.id,
            json!({"hook_event_name": "UserPromptSubmit", "session_id": "nat-2", "prompt": "x"}),
            &session,
        );
        assert_eq!(status(&k, &t.id), Status::Working);
        assert_eq!(k.hub.lock().live[&t.id].native_id.as_deref(), Some("nat-2"));
        let st = k.hub.lock();
        assert_eq!(st.store.latest_conversation(&t.id).unwrap().unwrap().native_session_id.as_deref(), Some("nat-2"));
    }
```

In `crates/mirante-daemon/src/hub/mod.rs`, extend `errors_map_to_typed_rpc_codes` (after the `NOT_FOUND` assertion):
```rust
        let e: RpcError = HubError::Driver(DriverError::NotNow("no running turn".into())).into();
        assert_eq!((e.code, e.message.as_str()), (code::CONFLICT, "no running turn"));
        let e: RpcError = HubError::Driver(DriverError::NativeGone("no rollout".into())).into();
        assert_eq!(e.code, code::CONFLICT);
        assert!(e.message.contains("conversation is gone"), "{}", e.message);
```

Replace the test module of `crates/mirante-cli/src/render.rs` with:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{PermissionStatus, Task};

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
            usage: None,
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
        assert!(lines[0].contains("MODE") && lines[1].contains("manual"));
        assert!(!t.contains('\x1b'));
        assert!(tasks_table(&[view(Status::Idle, 0, None)], true).contains("\x1b[32m"));
    }

    #[test]
    fn codex_rows_show_the_auto_edits_mode_and_usage() {
        let mut v = view(Status::Working, 0, Some("turn done, 1 command still finishing"));
        v.task.agent = AgentKind::Codex;
        v.usage = Some("5h 42% · 31k tokens".into());
        let t = tasks_table(&[v.clone()], false);
        assert!(t.contains("auto-edits"), "{t}");
        assert!(t.contains("[turn done, 1 command still finishing · 5h 42% · 31k tokens]"), "{t}");
        assert!(task_detail(&v, &[]).contains("mode:      auto-edits (edits and commands inside the worktree"));
        v.task.lifecycle = Lifecycle::Archiving;
        assert!(tasks_table(&[v], false).contains("[archiving · 5h 42% · 31k tokens]"));
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

In the `simple_commands` test of `crates/mirante-cli/src/args.rs`, replace the `resume` assertion with:
```rust
        assert_eq!(p("resume feat --trust").unwrap(), Cmd::Resume { task: "feat".into(), trust: true, fresh: false });
        assert_eq!(p("resume --fresh feat").unwrap(), Cmd::Resume { task: "feat".into(), trust: false, fresh: true });
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error[E0425]: cannot find type `Proc` in module `proctree`
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/driver/proctree.rs`:
```rust
//! Process trees: which processes descend from an agent, and ending all of them.
//!
//! A process is named by its pid AND its start time ([`Proc`]): a pid can be reused as soon
//! as its process is reaped, so nothing is ever signaled unless its start time still
//! matches. Agents run as the leader of their own process group (the PTY child calls
//! `setsid`; `codex app-server` is spawned with `process_group(0)`), so one `killpg`
//! reaches every child that stayed in the group; children that moved to another group
//! (`setsid`, `setpgrp`) are found by walking parent links (macOS: `proc_listchildpids`;
//! Linux: `/proc/<pid>/stat`).
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// One process: pid plus start time (µs since the epoch on macOS, clock ticks since boot
/// on Linux). Two values with the same pid and a different start are different processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Proc {
    pub pid: i32,
    pub start: u64,
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};

    /// `SZOMB` from `<sys/proc.h>`: exited, not yet reaped.
    const SZOMB: u32 = 5;

    fn bsdinfo(pid: i32) -> Option<libc::proc_bsdinfo> {
        if pid <= 0 {
            return None;
        }
        // SAFETY: `info` is a plain C struct of the size passed; the kernel fills at most that.
        let mut info: libc::proc_bsdinfo = unsafe { zeroed() };
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut c_void, size) };
        (n == size && info.pbi_status != SZOMB).then_some(info)
    }

    pub fn start_time(pid: i32) -> Option<u64> {
        bsdinfo(pid).map(|i| i.pbi_start_tvsec * 1_000_000 + i.pbi_start_tvusec)
    }

    pub fn pgid(pid: i32) -> Option<i32> {
        bsdinfo(pid).map(|i| i.pbi_pgid as i32)
    }

    pub fn children(pid: i32) -> Vec<i32> {
        if pid <= 0 {
            return Vec::new();
        }
        let mut cap = 64usize;
        loop {
            let mut buf = vec![0 as libc::pid_t; cap];
            let bytes = (cap * size_of::<libc::pid_t>()) as libc::c_int;
            // SAFETY: the buffer holds `cap` pids and its size in bytes is passed.
            let n = unsafe { libc::proc_listchildpids(pid, buf.as_mut_ptr() as *mut c_void, bytes) };
            if n < 0 {
                return Vec::new();
            }
            let n = n as usize;
            if n < cap {
                buf.truncate(n);
                buf.retain(|p| *p > 0);
                return buf;
            }
            cap *= 4;
        }
    }
}

#[cfg(target_os = "linux")]
mod sys {
    /// (state, ppid, pgid, start) from `/proc/<pid>/stat` (fields 3, 4, 5 and 22).
    fn stat(pid: i32) -> Option<(char, i32, i32, u64)> {
        if pid <= 0 {
            return None;
        }
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name may contain spaces and parentheses: fields start after the last ')'.
        let rest = &s[s.rfind(')')? + 2..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        let state = f.first()?.chars().next()?;
        Some((state, f.get(1)?.parse().ok()?, f.get(2)?.parse().ok()?, f.get(19)?.parse().ok()?))
    }

    pub fn start_time(pid: i32) -> Option<u64> {
        stat(pid).filter(|s| s.0 != 'Z' && s.0 != 'X').map(|s| s.3)
    }

    pub fn pgid(pid: i32) -> Option<i32> {
        stat(pid).map(|s| s.2)
    }

    pub fn children(pid: i32) -> Vec<i32> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .filter(|c| stat(*c).is_some_and(|s| s.1 == pid))
            .collect()
    }
}

/// The process `pid` now (`None`: gone or a zombie).
pub fn proc_of(pid: i32) -> Option<Proc> {
    sys::start_time(pid).map(|start| Proc { pid, start })
}

/// Still running, and still the same process (not a reused pid).
pub fn alive(p: Proc) -> bool {
    sys::start_time(p.pid) == Some(p.start)
}

pub fn pgid(pid: i32) -> Option<i32> {
    sys::pgid(pid)
}

/// Direct children of `pid` (unverified pids; see [`descendants`]).
pub fn children(pid: i32) -> Vec<i32> {
    sys::children(pid)
}

/// Every live process below `root` (not `root` itself), parents before children. Empty
/// when `root` itself is gone (its pid may name another process now). A child that started
/// before its parent is a reused pid, not a descendant, and is skipped.
pub fn descendants(root: Proc) -> Vec<Proc> {
    let mut out = Vec::new();
    if !alive(root) {
        return out;
    }
    let mut seen = HashSet::from([root.pid]);
    let mut next = vec![root];
    while let Some(parent) = next.pop() {
        for pid in children(parent.pid) {
            let Some(p) = proc_of(pid) else { continue };
            if p.start >= parent.start && seen.insert(pid) {
                out.push(p);
                next.push(p);
            }
        }
    }
    out
}

/// Where signals go. Tests pass a recorder; production uses [`Kill`].
pub trait Signaler: Send + Sync {
    fn signal(&self, pid: i32, sig: i32) -> bool;
    fn signal_group(&self, pgid: i32, sig: i32) -> bool;
}

/// `kill(2)` / `killpg(2)`.
pub struct Kill;

impl Signaler for Kill {
    fn signal(&self, pid: i32, sig: i32) -> bool {
        // SAFETY: plain syscall; `pid` > 0 is checked by the caller.
        pid > 0 && unsafe { libc::kill(pid, sig) } == 0
    }
    fn signal_group(&self, pgid: i32, sig: i32) -> bool {
        // SAFETY: plain syscall; `pgid` > 1 is checked by the caller.
        pgid > 1 && unsafe { libc::killpg(pgid, sig) } == 0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillReport {
    /// Processes that got SIGTERM.
    pub terminated: usize,
    /// Processes that needed SIGKILL.
    pub killed: usize,
    /// Still alive after the verify pass (should be empty).
    pub survivors: Vec<Proc>,
}

/// Signals `sig` to the root's process group (when the group still provably belongs to
/// this tree) and to every known process of the tree that is still the same process.
fn signal_tree(root: Proc, known: &[Proc], sig: i32, s: &dyn Signaler) -> usize {
    let group_ours = alive(root) || known.iter().any(|p| alive(*p) && pgid(p.pid) == Some(root.pid));
    if group_ours && root.pid > 1 {
        s.signal_group(root.pid, sig);
    }
    let mut n = 0;
    for p in std::iter::once(&root).chain(known) {
        if p.pid > 1 && alive(*p) {
            s.signal(p.pid, sig);
            n += 1;
        }
    }
    n
}

/// Ends `root` and everything below it: SIGTERM to the tree, then up to `grace` for it to
/// exit (the tree is re-walked on every poll, so a process forked meanwhile is caught and
/// told too), then SIGKILL to whatever is left, then a verify pass. Never signals a
/// process that is not (by pid AND start time) part of the tree.
pub fn kill_tree(root: Proc, grace: Duration, s: &dyn Signaler) -> KillReport {
    let mut known: Vec<Proc> = descendants(root);
    let mut report = KillReport { terminated: signal_tree(root, &known, libc::SIGTERM, s), ..Default::default() };
    let deadline = Instant::now() + grace;
    loop {
        // Newly forked processes get SIGTERM too.
        let mut fresh = Vec::new();
        for p in std::iter::once(root).chain(known.clone()) {
            if alive(p) {
                for d in descendants(p) {
                    if !known.contains(&d) {
                        fresh.push(d);
                    }
                }
            }
        }
        for d in fresh {
            if !known.contains(&d) {
                if alive(d) && s.signal(d.pid, libc::SIGTERM) {
                    report.terminated += 1;
                }
                known.push(d);
            }
        }
        let left = !alive(root) && known.iter().all(|p| !alive(*p));
        if left || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // Hard kill, repeated until nothing new shows up (a dying parent may still fork).
    for _ in 0..5 {
        for p in std::iter::once(root).chain(known.clone()) {
            if alive(p) {
                for d in descendants(p) {
                    if !known.contains(&d) {
                        known.push(d);
                    }
                }
            }
        }
        let n = signal_tree(root, &known, libc::SIGKILL, s);
        report.killed += n;
        if n == 0 {
            break;
        }
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until && (alive(root) || known.iter().any(|p| alive(*p))) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    report.survivors = std::iter::once(root).chain(known).filter(|p| alive(*p)).collect();
    report
}
```

Replace `crates/mirante-daemon/src/driver/fake.rs` with:
```rust
//! Test double: records every call, lets the test emit events through the captured sink.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use super::proctree::Proc;
use super::{Attachment, Driver, DriverError, DriverEvent, DriverFactory, Sink, SpawnRequest};

#[derive(Default)]
pub struct FakeDriver {
    pub calls: Mutex<Vec<String>>,
    pub dialog: AtomicBool,
    pub fail_answers: AtomicBool,
    /// The attached client's output channel, kept open until `detach` (like a live agent).
    pub attached: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    /// What `root()` reports (memory sampling tests set it).
    pub root: Mutex<Option<Proc>>,
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
    fn resize(&self, rows: u16, cols: u16) -> Result<(), DriverError> {
        self.record(format!("resize:{rows}x{cols}"));
        Ok(())
    }
    fn screen_tail(&self) -> String {
        "fake screen".into()
    }
    fn stop(&self) {
        self.record("stop".into());
    }
    fn session_started(&self) {
        self.record("session_started".into());
    }
    fn root(&self) -> Option<Proc> {
        *self.root.lock().unwrap()
    }
}

/// Hands out one shared `FakeDriver` and keeps every sink and spawn request.
#[derive(Default)]
pub struct FakeFactory {
    pub driver: Arc<FakeDriver>,
    pub sinks: Mutex<Vec<Sink>>,
    pub resumes: Mutex<Vec<Option<String>>>,
    pub fail: AtomicBool,
    /// When set, `spawn` fails with this error (e.g. `NativeGone`).
    pub fail_with: Mutex<Option<DriverError>>,
    /// When set, the next `spawn` reports on the first channel that it started, then blocks
    /// until the second one yields (or closes).
    pub spawn_gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
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
        if let Some((entered, release)) = self.spawn_gate.lock().unwrap().take() {
            let _ = entered.send(());
            let _ = release.recv();
        }
        self.resumes.lock().unwrap().push(req.resume.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(DriverError::Failed("spawn refused".into()));
        }
        if let Some(e) = self.fail_with.lock().unwrap().clone() {
            return Err(e);
        }
        self.sinks.lock().unwrap().push(sink);
        Ok(self.driver.clone())
    }
}
```

In `crates/mirante-daemon/src/hub/mod.rs`:

1. Add `use std::sync::atomic::{AtomicBool, Ordering};` to the imports.
2. In `impl From<HubError> for RpcError`, replace the arm `| HubError::Driver(DriverError::NoDialog(_)) => code::CONFLICT,` with:
```rust
            | HubError::Driver(DriverError::NoDialog(_) | DriverError::NotNow(_) | DriverError::NativeGone(_)) => {
                code::CONFLICT
            }
```
3. Add two fields at the end of `struct Live` (and `hold: Arc::default(), usage: None,` at the end of `Live::new`):
```rust
    /// The driver's hold flag ([`crate::driver::Sink::held`]): set while this session has a
    /// pending permission request, so the driver types nothing.
    pub(crate) hold: Arc<AtomicBool>,
    /// Codex quota / token usage, as the driver last reported it.
    pub(crate) usage: Option<String>,
```
4. In `State::view`, add `usage: live.and_then(|l| l.usage.clone()),` after the `detail` field.
5. Add `sync_holds` above `publish`, and call it first thing in `publish`:
```rust
    /// Sets each live session's hold flag: held while it has a pending permission request
    /// (typed text could answer that dialog).
    pub(crate) fn sync_holds(&mut self) {
        let State { store, live, .. } = self;
        for (task_id, l) in live.iter() {
            let held = store
                .pending_permissions(task_id)
                .map(|p| p.iter().any(|p| p.session_id == l.session_id))
                .unwrap_or(true);
            l.hold.store(held, Ordering::SeqCst);
        }
    }

    /// Sends every event newer than `last_seq` to the subscribers, dropping closed ones.
    /// Every hub mutation ends here, so the hold flags are brought up to date first.
    pub(crate) fn publish(&mut self) {
        self.sync_holds();
        loop {
            // … unchanged
```

In `crates/mirante-daemon/src/hub/session.rs`:

1. Import `DriverError`: `use crate::driver::{Attachment, Driver, DriverError, DriverEvent, Sink, SpawnRequest};`
2. In `State::handle`, add these arms right after the `DriverEvent::PermissionRequested` arm:
```rust
            DriverEvent::PermissionResolved { key } => {
                let Some(l) = self.live.get(task_id) else {
                    return Ok(());
                };
                let card = l.perm_keys.iter().find(|(_, k)| **k == key).map(|(id, _)| id.clone());
                let Some(id) = card else {
                    // Already answered through Mirante (its key was removed then).
                    return Ok(());
                };
                let pending = self.store.pending_permissions(task_id)?.iter().any(|p| p.id == id);
                if pending {
                    self.settle_card(task_id, &id, PermissionStatus::Denied, "codex:request_resolved")?;
                } else if let Some(l) = self.live.get_mut(task_id) {
                    l.perm_keys.remove(&id);
                }
                Ok(())
            }
            DriverEvent::DeliveryAborted { text, reason } => {
                if let Some(l) = self.live.get_mut(task_id) {
                    if let Some(i) = l.steers.iter().rposition(|s| *s == text) {
                        l.steers.remove(i);
                    }
                    l.queue.push_front(text.clone());
                    l.delivery_in_flight = None;
                }
                self.event(Some(task_id), "message_requeued", json!({ "text": text, "reason": reason }))
            }
            DriverEvent::ResumeFailed { reason } => {
                let payload = json!({
                    "session_id": session_id,
                    "reason": reason,
                    "hint": "`mirante resume <task> --fresh` starts a new conversation",
                });
                self.event(Some(task_id), "resume_failed", payload)
            }
            DriverEvent::Usage(summary) => {
                if let Some(l) = self.live.get_mut(task_id) {
                    l.usage = Some(summary);
                }
                Ok(())
            }
```
3. In `Hub::start_session`, create the sink inside the locked block (so `Live` gets its hold flag) and return it with the other values; the block now ends like this, and the old `let sink = Sink::new(&session_id, self.events.clone());` line after it is removed (the destructuring becomes `let (task, session_id, native, sink) = {`):
```rust
            let sink = Sink::new(&session.id, self.events.clone());
            let mut live = Live::new(&session.id, task.agent, native.clone());
            live.hold = sink.hold_flag();
            if let Some(p) = req.prompt.filter(|p| !p.trim().is_empty()) {
                live.queue.push_back(p);
            }
            st.live.insert(task.id.clone(), live);
            st.publish();
            (task, session.id, native, sink)
        };
```
4. In the `Err(e)` arm of the spawn `match`, record a vanished native conversation before ending the session:
```rust
            Err(e) => {
                let mut st = self.lock();
                if let DriverError::NativeGone(reason) = &e {
                    let payload = json!({
                        "session_id": session_id,
                        "reason": reason,
                        "hint": "`mirante resume <task> --fresh` starts a new conversation",
                    });
                    st.event(Some(&task.id), "resume_failed", payload)?;
                }
                st.finish_session(&task.id, &session_id, "mirante:spawn_failed")?;
                st.publish();
                return Err(e.into());
            }
```
5. In `Hub::apply_hook_envelope`, replace the generation fence and the native-id check that follows it with:
```rust
        // Generation fence: a hook stamped with an earlier Mirante session of this task (its
        // Claude process outlived the session, or the hook was slow) must not touch the new
        // one. Unstamped hooks (an older hook binary) are still applied.
        if let Some(stale) = stamped.as_ref().filter(|s| **s != session_id) {
            let payload = json!({ "hook": ev.hook_event_name, "session": stale, "live_session": session_id });
            st.event(Some(&p.task), "hook_stale", payload)?;
            st.publish();
            return Ok(());
        }
        if ev.hook_event_name == "SessionStart" {
            if let Some(d) = live.driver.clone() {
                d.session_started();
            }
        }
        // Stamped with this very Mirante session: Claude's own session id may change under
        // it (`/clear`, a resume that forks), so the hook's id is adopted, never dropped.
        if stamped.is_some() {
            if let Some(sid) = &ev.session_id {
                st.set_native_id(&p.task, &session_id, sid)?;
            }
        } else if ev.hook_event_name != "SessionStart" {
            if let (Some(known), Some(sid)) = (native.as_deref(), ev.session_id.as_deref()) {
                if known != sid {
                    let payload = json!({ "hook": ev.hook_event_name, "session_id": sid, "native_id": known });
                    st.event(Some(&p.task), "hook_orphan", payload)?;
                    st.publish();
                    return Ok(());
                }
            }
        }
```
In `crates/mirante-ipc/src/types.rs`, add the field at the end of `TaskView`, and `usage: None,` to the `TaskView` literal in `task_view_flattens_the_task`:
```rust
    /// Codex quota / token usage while live, e.g. "5h 42% · week 9% · 31k tokens".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
```

In `crates/mirante-cli/src/render.rs`, replace everything above the test module with:
```rust
//! Plain-text tables. Colors only on a terminal (`color` = stdout is a tty).
use mirante_core::model::{AgentKind, Lifecycle, PermissionRequest, Status};
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

/// The approval mode Mirante runs each agent in (fixed per agent; never a bypass mode).
pub fn mode_label(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "manual",
        AgentKind::Codex => "auto-edits",
    }
}

/// What the mode means, for `mirante show`.
pub fn mode_help(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "Claude asks before every tool that needs permission (--permission-mode manual)",
        AgentKind::Codex => {
            "edits and commands inside the worktree run without asking (sandbox workspace-write); \
             anything beyond it asks you (approvalPolicy on-request, approvalsReviewer user)"
        }
    }
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
    let state = match (v.boot, &v.detail) {
        _ if v.task.lifecycle == Lifecycle::Archiving => Some("archiving".to_string()),
        (Some(BootState::TrustPrompt), _) => Some("trust prompt: mirante resume / attach".to_string()),
        (Some(BootState::Booting), _) => Some("starting".to_string()),
        (_, Some(d)) => Some(d.clone()),
        _ => None,
    };
    state.into_iter().chain(v.usage.clone()).collect::<Vec<_>>().join(" · ")
}

pub fn tasks_table(tasks: &[TaskView], color: bool) -> String {
    if tasks.is_empty() {
        return "no tasks (mirante new <repo> --agent claude|codex --branch <name> <goal>)\n".into();
    }
    let mut out = format!(
        "{}  {}  {}  {}  {}  {}  {}  {}\n",
        fit("ID", 8),
        fit("BRANCH", 22),
        fit("AGENT", 6),
        fit("MODE", 10),
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
            "{}  {}  {}  {}  {}  {}  {}  {}\n",
            fit(short(&v.task.id), 8),
            fit(&v.task.branch, 22),
            fit(v.task.agent.as_str(), 6),
            fit(mode_label(v.task.agent), 10),
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
    out.push_str(&format!("  mode:      {} ({})\n", mode_label(t.agent), mode_help(t.agent)));
    out.push_str(&format!("  live:      {}  queued: {}\n", v.live, v.queued));
    for p in perms {
        out.push_str(&format!("  needs you: {} {} {}\n", short(&p.id), p.tool, p.detail));
    }
    out
}
```

In `crates/mirante-cli/src/args.rs`: in `USAGE`, replace the `resume` entry with
```
  resume <task> [--trust] [--fresh]
                         restart the agent on its native session (context kept);
                         --fresh starts a new conversation instead (your explicit choice)
```
add `fresh: bool,` to `Cmd::Resume`, and parse it:
```rust
        "resume" => {
            let p = split(rest, none, &["--trust", "--fresh"])?;
            let has = |f: &str| p.bools.iter().any(|b| b == f);
            Ok(Cmd::Resume { task: one(&p, "task")?, trust: has("--trust"), fresh: has("--fresh") })
        }
```
In `crates/mirante-cli/src/main.rs`, in `boot_flow` add this arm before the plain `BootState::Exited` arm:
```rust
            BootState::Exited if r.resumed => {
                return Err(format!(
                    "the agent exited right after resuming (its conversation may be gone):\n{}\n\
                     `mirante resume {id} --fresh` starts a new conversation",
                    r.screen
                )
                .into())
            }
```
and replace the `Cmd::Resume { task, trust }` arm of `run` with the two arms below (the second keeps the old body):
```rust
        Cmd::Resume { task, trust, fresh: true } => {
            let mut c = client(&paths)?;
            let r: StartResult = c.call(method::SESSION_START, StartSession { task, prompt: None })?;
            println!("starting a fresh conversation (the earlier one is not resumed)");
            boot_flow(&mut c, r, trust)?;
        }
        Cmd::Resume { task, trust, fresh: false } => {
            let mut c = client(&paths)?;
            let r: StartResult = c.call(method::SESSION_RESUME, StartSession { task, prompt: None })?;
            // … unchanged
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon -p mirante-ipc -p mirante-cli`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 92 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
(mirante-cli lib, `mirante` bin, mirante-daemon lib, daemon bin, `daemon_signals`, mirante-ipc, then the three doc-test targets.) No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon crates/mirante-ipc crates/mirante-cli
git commit -F - <<'EOF'
feat(daemon): driver interface v2, process-tree kill, hold flag, resolved/aborted/usage events

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 14: PTY process, terminal queries, screen reading, paste sanitizing

**Files:**
- Modify: `crates/mirante-daemon/src/driver/mod.rs` (module list)
- Create: `crates/mirante-daemon/src/driver/termq.rs`
- Create: `crates/mirante-daemon/src/driver/paste.rs`
- Create: `crates/mirante-daemon/src/driver/screen.rs`
- Create: `crates/mirante-daemon/src/driver/pty.rs`

The building blocks of the Claude driver (Task 15). Decisions, stated plainly:

- **Terminal queries are answered by the daemon, always** (`termq.rs`). Claude Code stalls until its cursor-position query (`ESC[6n`) is answered (Week-1 spike); it may also ask for device attributes (`ESC[c`), status (`ESC[5n`) and the foreground/background colors (OSC 10/11 `?`). The daemon answers every one of them, attached or not, and strips them from the bytes attached clients receive, so a client terminal never answers a second time (a second answer would arrive in Claude as typed input). The parser keeps state across reads: a query split between two chunks is held back (only while the held bytes are still a prefix of a known query, so at most 7 bytes) and completed by the next chunk; everything else passes through byte for byte, including bytes that are not UTF-8. Answers are written in stream order, so a CPR answer reports the cursor as it is right after the bytes before the query.
- **The agent is a process-group leader** (portable-pty calls `setsid` in the child). `PtyProcess::root()` is the child's `Proc` (pid + start time, Task 13) captured at spawn; `kill_tree` ends the group and every descendant through `proctree::kill_tree` (SIGTERM, grace, SIGKILL, verify). Dropping a `PtyProcess` does a short `kill_tree` and reaps the child.
- **Attach repaints the full terminal state**: vt100 `state_formatted()` (screen contents plus input modes: bracketed paste, application cursor keys, mouse, cursor visibility), so a client attaching mid-session types into the same modes the agent set. `PtyProcess::attach` takes the fan-out lock before reading the mirror (the reader holds the same lock while mirroring and sending a chunk): the repaint and the first live bytes neither overlap nor leave a gap.
- **Screen reading** (`screen.rs`) is used only for UI actions, never for status (spec §2.2). A live dialog sits in the bottom rows and ends with a key-hint footer, so only the last 20 non-blank rows are searched and a footer is required: a permission dialog is a `Do you want to …?` header, a `1.` option and an `Esc to …` footer below it; the trust dialog is an option labelled exactly "Yes, I trust this folder" with a footer (`Esc to …` / `Enter to confirm`) below it. The same words in the conversation above (quoted dialogs, prose questions) never count. `has_any_dialog` also blocks typing for dialogs Mirante does not know (plan approval, pickers): a `❯ N.` option with a footer. FINDINGS rule 6 is unchanged: only an exact "Yes" / "No" label below the live header is ever chosen; plan approval ("Yes, and auto-accept edits", …) has no plain "Yes" and is never answered by Mirante.
- **Messages are sanitized and pasted** (`paste.rs`): one bracketed paste (`ESC[200~` … `ESC[201~`), so a newline no longer submits early and multi-line messages work. Before framing, every terminal control is removed: CRLF / CR become LF, CSI / OSC / other escape sequences are dropped whole (a smuggled `ESC[201~` cannot end the paste early), C0 controls except LF and TAB, DEL and C1 controls are dropped, TAB becomes 4 spaces; all other characters (accents, emoji) are kept. Two prefixes are special and this is the rule: a message starting with `/` is a Claude slash command and is typed as plain keystrokes (sanitized, one line only, otherwise refused) so Claude runs it as the command the user wrote; a message starting with `!` would run a shell command in Claude's bash mode with no permission prompt, so Mirante refuses it ("attach and type it yourself").

- [ ] **Step 1: Write the failing tests**

In `crates/mirante-daemon/src/driver/mod.rs`, replace the module list with:
```rust
#[cfg(test)]
pub mod fake;
pub mod paste;
pub mod proctree;
pub mod pty;
pub mod real;
pub mod screen;
pub mod termq;
```

`crates/mirante-daemon/src/driver/termq.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(p: &[Piece]) -> Vec<u8> {
        p.iter()
            .flat_map(|p| match p {
                Piece::Bytes(b) => b.clone(),
                Piece::Query(_) => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn queries_are_split_out_in_stream_order() {
        let mut t = TermQueries::default();
        let p = t.feed(b"ab\x1b[6ncd\x1b[c\x1b[0c\x1b[5n");
        assert_eq!(
            p,
            vec![
                Piece::Bytes(b"ab".to_vec()),
                Piece::Query(Query::CursorPosition),
                Piece::Bytes(b"cd".to_vec()),
                Piece::Query(Query::DeviceAttributes),
                Piece::Query(Query::DeviceAttributes),
                Piece::Query(Query::Status),
            ]
        );
    }

    #[test]
    fn a_query_split_across_reads_is_completed() {
        let mut t = TermQueries::default();
        assert_eq!(t.feed(b"x\x1b["), vec![Piece::Bytes(b"x".to_vec())]);
        assert_eq!(t.feed(b"6"), vec![]);
        assert_eq!(t.feed(b"ny"), vec![Piece::Query(Query::CursorPosition), Piece::Bytes(b"y".to_vec())]);
        assert_eq!(t.feed(b"\x1b]1"), vec![]);
        assert_eq!(t.feed(b"1;?\x1b"), vec![]);
        assert_eq!(t.feed(b"\\"), vec![Piece::Query(Query::Background { bel: false })]);
    }

    #[test]
    fn osc_color_queries_with_bel_and_st() {
        let mut t = TermQueries::default();
        let p = t.feed(b"\x1b]10;?\x07\x1b]11;?\x1b\\");
        assert_eq!(
            p,
            vec![Piece::Query(Query::Foreground { bel: true }), Piece::Query(Query::Background { bel: false })]
        );
        assert_eq!(answer(Query::Foreground { bel: true }, (0, 0)), b"\x1b]10;rgb:ffff/ffff/ffff\x07");
        assert_eq!(answer(Query::Background { bel: false }, (0, 0)), b"\x1b]11;rgb:0000/0000/0000\x1b\\");
        assert_eq!(answer(Query::CursorPosition, (4, 9)), b"\x1b[5;10R");
    }

    #[test]
    fn other_sequences_pass_through_untouched() {
        let mut t = TermQueries::default();
        let input = b"\x1b[2J\x1b[1;31mred\x1b[0m\x1b]0;title\x07\x1b[6;5n\x1b[?2004h\x1b]8;;http://x\x1b\\";
        let p = t.feed(input);
        assert_eq!(bytes(&p), input.to_vec());
        assert!(p.iter().all(|p| matches!(p, Piece::Bytes(_))));
    }

    #[test]
    fn a_held_prefix_that_is_no_query_passes_through_with_the_next_chunk() {
        let mut t = TermQueries::default();
        assert_eq!(t.feed(b"\x1b]1"), vec![]);
        assert_eq!(bytes(&t.feed(b"2;?\x07")), b"\x1b]12;?\x07".to_vec());
        // At the end of the stream an unfinished prefix is returned as-is.
        t.feed(b"\x1b[");
        assert_eq!(t.flush(), b"\x1b[".to_vec());
        assert_eq!(t.flush(), Vec::<u8>::new());
    }

    #[test]
    fn bytes_without_escape_are_unchanged_whatever_they_are() {
        let mut t = TermQueries::default();
        let data: Vec<u8> = (0..=255u8).filter(|b| *b != 0x1b).cycle().take(5000).collect();
        let mut got = Vec::new();
        for chunk in data.chunks(333) {
            got.extend(bytes(&t.feed(chunk)));
        }
        assert_eq!(got, data);
    }
}
```

`crates/mirante-daemon/src/driver/paste.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_and_smuggled_paste_end_are_removed_newlines_and_emoji_kept() {
        let framed = bracketed("a\r\nb\x1b[201~c\td\x00e🙂");
        assert_eq!(framed, "\x1b[200~a\nbc    de🙂\x1b[201~".as_bytes());
    }

    #[test]
    fn every_kind_of_control_is_dropped() {
        assert_eq!(sanitize_for_pty("x\ry"), "x\ny");
        assert_eq!(sanitize_for_pty("t\x1b]0;title\x07u\x1b]8;;http://e\x1b\\v"), "tuv");
        assert_eq!(sanitize_for_pty("p\x1bPq data\x1b\\r"), "pr");
        assert_eq!(sanitize_for_pty("a\x1bcb\x1b"), "ab");
        assert_eq!(sanitize_for_pty("1\u{9b}2\u{85}3\x7f4\x085"), "12345");
        assert_eq!(sanitize_for_pty("ação ✓ 日本"), "ação ✓ 日本");
    }

    #[test]
    fn slash_commands_are_keystrokes_and_bang_is_refused() {
        assert_eq!(typed_message("  /compact keep the plan\n").unwrap(), b"/compact keep the plan".to_vec());
        assert!(typed_message("/compact\nmore").unwrap_err().contains("one line"));
        assert!(typed_message("!rm -rf build").unwrap_err().contains("attach and type it yourself"));
        assert!(typed_message("\x1b[31m\x00").unwrap_err().contains("empty"));
        assert_eq!(typed_message("use tabs\nthen test").unwrap(), b"\x1b[200~use tabs\nthen test\x1b[201~".to_vec());
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
    const TRUST: &str = "\
 Do you trust the files in this folder?
 /Users/u/.mirante/worktrees/app/feat
 ❯ 1. No, exit
   2. Yes, I trust this folder
 Enter to confirm · Esc to exit";
    const PLAN: &str = "\
 Would you like to proceed?
 ❯ 1. Yes, and auto-accept edits
   2. Yes, and manually approve edits
   3. No, keep planning
 ctrl-g to edit in editor · Esc to cancel";

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
    fn dialogs_need_a_footer_in_the_bottom_rows() {
        assert!(has_permission_prompt(DIALOG) && has_permission_prompt(EDIT_DIALOG));
        assert!(!has_permission_prompt(IDLE));
        // Prose that asks the same question is not a dialog: no footer.
        assert!(!has_permission_prompt("Do you want to proceed?\n1. Yes\n2. No\n❯ "));
        // A real dialog scrolled up above the bottom rows is history, not a live dialog.
        let scrolled = format!("{DIALOG}\n{}❯\n  ⏸ manual mode on", "output line\n".repeat(BOTTOM_ROWS));
        assert!(!has_permission_prompt(&scrolled) && !dialog_open(&scrolled));
    }

    #[test]
    fn the_trust_dialog_is_the_exact_option_with_a_footer() {
        assert!(is_trust_dialog(TRUST));
        assert_eq!(trust_key(TRUST), Some('2'));
        assert!(dialog_open(TRUST));
        // The question quoted in the conversation is not the dialog.
        let prose = "❯ Do you trust the files in this folder? asked the README\n  Yes, I trust this folder\n❯ \n  ⏸ manual mode on";
        assert!(!is_trust_dialog(prose));
        // Without a footer below the option there is no live dialog.
        assert!(!is_trust_dialog(" ❯ 1. No, exit\n   2. Yes, I trust this folder"));
    }

    #[test]
    fn plan_approval_has_no_plain_yes_but_still_blocks_typing() {
        assert_eq!(option_key(PLAN, "Yes"), None);
        assert!(!has_permission_prompt(PLAN));
        assert!(has_any_dialog(PLAN) && dialog_open(PLAN));
    }

    #[test]
    fn the_input_prompt_and_the_ready_fast_path() {
        assert!(has_input_prompt(IDLE) && has_manual_mode_footer(IDLE));
        assert!(!dialog_open(IDLE));
        assert!(has_input_prompt("> \n"));
        assert!(!has_input_prompt(" ❯ 1. Yes\n Esc to cancel"));
        assert!(!has_manual_mode_footer(TRUST));
    }

    #[test]
    fn dialog_matches_the_request_detail() {
        assert!(dialog_matches(DIALOG, "touch s3-allowed.txt"));
        assert!(!dialog_matches(DIALOG, "rm -rf /"));
        assert!(dialog_matches(EDIT_DIALOG, "/Users/u/.mirante/worktrees/app/feat/src/main.rs"));
        assert!(dialog_matches(DIALOG, ""));
        assert!(!dialog_matches(IDLE, "touch s3-denied.txt"));
        let wrapped = "Bash command\n  cargo test --workspace --all-\n  features -- --nocapture\nDo you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";
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
    fn queries_are_answered_while_attached_and_never_reach_the_client() {
        // `stty raw -echo` so the reply reaches `dd` byte for byte.
        let p = PtyProcess::spawn(
            &sh("stty raw -echo; printf 'R\\r\\n'; dd bs=1 count=1 >/dev/null 2>&1; printf 'ab\\033[6n'; \
                 r=$(dd bs=1 count=6 2>/dev/null); printf '\\r\\nreply:%s' \"$r\" | tr '\\033' 'E'; sleep 5"),
            |_| {},
        )
        .unwrap();
        wait_for("raw mode", || p.grid().contains('R'));
        let (_, _initial, rx) = p.attach();
        p.write(b"g").unwrap();
        wait_for("reply", || p.grid().contains("reply:E[2;3R"));
        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains("reply:") {
            seen.extend(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        assert!(!seen.windows(4).any(|w| w == b"\x1b[6n"), "{:?}", String::from_utf8_lossy(&seen));
        assert!(String::from_utf8_lossy(&seen).contains("ab"));
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
    fn the_repaint_restores_input_modes() {
        let p = PtyProcess::spawn(&sh("printf '\\033[?2004hhello'; sleep 5"), |_| {}).unwrap();
        wait_for("hello", || p.grid().contains("hello"));
        let (_, initial, _rx) = p.attach();
        let text = String::from_utf8_lossy(&initial);
        assert!(text.contains("hello") && text.contains("\x1b[?2004h"), "{text:?}");
    }

    #[test]
    fn dropping_the_process_ends_its_tree_and_resize_reaches_the_grid() {
        let p = PtyProcess::spawn(&sh("sleep 30 & sleep 30"), |_| {}).unwrap();
        let root = p.root().expect("root process");
        wait_for("the background child", || !proctree::descendants(root).is_empty());
        let kids = proctree::descendants(root);
        p.resize(30, 100).unwrap();
        assert_eq!(p.parser.lock().unwrap().screen().size(), (nz(30), nz(100)));
        let started = Instant::now();
        drop(p);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!proctree::alive(root));
        assert!(kids.iter().all(|k| !proctree::alive(*k)), "{kids:?}");
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

Add above the test module in `crates/mirante-daemon/src/driver/termq.rs`:
```rust
//! Terminal queries in the agent's output, answered by the daemon. A TUI asks its terminal
//! for the cursor position (CPR `ESC[6n`), device attributes (DA1 `ESC[c`), status (DSR
//! `ESC[5n`) and colors (OSC 10/11 `?`); Claude Code stalls until CPR is answered (Week-1
//! spike). The daemon always answers (attached or not) and strips the queries from what
//! attached clients get, so a client terminal never answers a second time (a second reply
//! would arrive as typed input).
//!
//! The parser keeps state across reads: a query split between two chunks (`ESC[` then
//! `6n`) is held back and completed with the next chunk. Only bytes that are still a prefix
//! of a known query are held (at most 7); everything else passes through byte for byte.

/// A query the agent sent to its terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    /// `ESC[6n`: where is the cursor?
    CursorPosition,
    /// `ESC[c` / `ESC[0c`: what kind of terminal is this?
    DeviceAttributes,
    /// `ESC[5n`: are you OK?
    Status,
    /// `ESC]10;?`: the foreground color. `bel`: terminated by BEL (else by ST).
    Foreground { bel: bool },
    /// `ESC]11;?`: the background color.
    Background { bel: bool },
}

/// Output split into what passes through and the queries, in stream order (a CPR answer
/// needs the cursor as it is after the bytes before the query).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Bytes(Vec<u8>),
    Query(Query),
}

const QUERIES: [(&[u8], Query); 8] = [
    (b"\x1b[6n", Query::CursorPosition),
    (b"\x1b[c", Query::DeviceAttributes),
    (b"\x1b[0c", Query::DeviceAttributes),
    (b"\x1b[5n", Query::Status),
    (b"\x1b]10;?\x07", Query::Foreground { bel: true }),
    (b"\x1b]10;?\x1b\\", Query::Foreground { bel: false }),
    (b"\x1b]11;?\x07", Query::Background { bel: true }),
    (b"\x1b]11;?\x1b\\", Query::Background { bel: false }),
];

/// What a partial match at the end of the input could still become.
enum Match {
    Full(Query, usize),
    Prefix,
    No,
}

fn match_at(buf: &[u8]) -> Match {
    let mut prefix = false;
    for (pat, q) in QUERIES {
        if buf.len() >= pat.len() {
            if &buf[..pat.len()] == pat {
                return Match::Full(q, pat.len());
            }
        } else if pat.starts_with(buf) {
            prefix = true;
        }
    }
    if prefix {
        Match::Prefix
    } else {
        Match::No
    }
}

#[derive(Debug, Default)]
pub struct TermQueries {
    held: Vec<u8>,
}

impl TermQueries {
    /// Splits one chunk of output (plus whatever was held from the previous one).
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Piece> {
        let mut buf = std::mem::take(&mut self.held);
        buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut plain = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != 0x1b {
                plain.push(buf[i]);
                i += 1;
                continue;
            }
            match match_at(&buf[i..]) {
                Match::Full(q, len) => {
                    if !plain.is_empty() {
                        out.push(Piece::Bytes(std::mem::take(&mut plain)));
                    }
                    out.push(Piece::Query(q));
                    i += len;
                }
                Match::Prefix => {
                    // Only reachable at the end of the input: wait for the next chunk.
                    self.held = buf[i..].to_vec();
                    break;
                }
                Match::No => {
                    plain.push(0x1b);
                    i += 1;
                }
            }
        }
        if !plain.is_empty() {
            out.push(Piece::Bytes(plain));
        }
        out
    }

    /// Bytes held back at the end of the stream (an unfinished query prefix), as-is.
    pub fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }
}

/// The reply a terminal would send. `cursor` is (row, col), 0-based.
pub fn answer(q: Query, cursor: (u16, u16)) -> Vec<u8> {
    let st = |bel: bool| if bel { "\x07" } else { "\x1b\\" };
    match q {
        Query::CursorPosition => format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1).into_bytes(),
        // A VT100 with advanced video: the plainest answer every TUI accepts.
        Query::DeviceAttributes => b"\x1b[?1;2c".to_vec(),
        Query::Status => b"\x1b[0n".to_vec(),
        Query::Foreground { bel } => format!("\x1b]10;rgb:ffff/ffff/ffff{}", st(bel)).into_bytes(),
        Query::Background { bel } => format!("\x1b]11;rgb:0000/0000/0000{}", st(bel)).into_bytes(),
    }
}
```

Add above the test module in `crates/mirante-daemon/src/driver/paste.rs`:
```rust
//! Turning a user's message into the bytes typed into Claude Code's input box.
//!
//! A message is sent as one bracketed paste (`ESC[200~` … `ESC[201~`), so newlines stay
//! inside the message instead of submitting it early, and the text is sanitized first:
//! nothing in it can act as a terminal control (no escape sequences, so a smuggled
//! `ESC[201~` cannot end the paste early; no control characters). Two prefixes are special:
//! - `/…` is a Claude slash command. It is typed as plain keystrokes (no paste) so Claude
//!   runs it as the command the user wrote. It must be one line.
//! - `!…` is Claude's bash mode, which runs a shell command with no permission prompt.
//!   Mirante refuses it: a relayed message must never run a shell command unasked; the user
//!   attaches and types it.

pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// The text with every terminal control removed: CRLF and lone CR become LF; CSI, OSC and
/// other escape sequences are dropped whole; C0 controls except LF and TAB, DEL and C1
/// controls (U+0080–U+009F) are dropped; TAB becomes 4 spaces. All other characters,
/// including non-ASCII and emoji, are kept.
pub fn sanitize_for_pty(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            '\x1b' => match chars.next() {
                // CSI: parameters and intermediates up to a final byte @..~.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC, DCS, SOS, PM, APC: a string up to BEL or ST (ESC \).
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Any other escape: ESC plus one character.
                _ => {}
            },
            c if (c as u32) < 0x20 || c == '\x7f' => {}
            c if (0x80..=0x9f).contains(&(c as u32)) => {}
            c => out.push(c),
        }
    }
    out
}

/// `text` sanitized and framed as one bracketed paste.
pub fn bracketed(text: &str) -> Vec<u8> {
    let mut out = PASTE_START.to_vec();
    out.extend_from_slice(sanitize_for_pty(text).as_bytes());
    out.extend_from_slice(PASTE_END);
    out
}

/// The bytes to type for a message (Enter is sent separately), or why it is refused.
pub fn typed_message(text: &str) -> Result<Vec<u8>, String> {
    let clean = sanitize_for_pty(text);
    let lead = clean.trim_start();
    if lead.trim().is_empty() {
        return Err("the message is empty once control characters are removed".into());
    }
    if lead.starts_with('!') {
        return Err("a message starting with `!` would run a shell command in Claude's bash mode without a \
                    permission prompt; Mirante does not send it (attach and type it yourself)"
            .into());
    }
    if lead.starts_with('/') {
        let line = lead.trim_end();
        if line.contains('\n') {
            return Err("a slash command must be one line".into());
        }
        return Ok(line.as_bytes().to_vec());
    }
    Ok(bracketed(text))
}
```

Add above the test module in `crates/mirante-daemon/src/driver/screen.rs`:
```rust
//! Reading Claude Code's TUI (2.1.285) off the emulated screen. Used ONLY for UI actions
//! (which key answers this dialog, is a dialog in the way of typed text, is the trust
//! prompt up, is the input box there). Status never comes from here: it comes from hooks
//! and the transcript (`mirante_core::status`). Dialog matching ported from
//! `spike_kit::screen`, whose fixtures are real 2.1.285 grids.
//!
//! Live dialogs sit at the bottom of the screen and end with a key-hint footer ("Esc to
//! cancel", "Enter to confirm · Esc to exit"). Only the last [`BOTTOM_ROWS`] non-blank
//! rows are searched and a footer is required, so the same words in the conversation above
//! (a quoted dialog, prose asking "Do you want to …?") never count as a dialog.

/// How many non-blank rows at the bottom of the screen a live dialog can occupy.
pub const BOTTOM_ROWS: usize = 20;

/// The last [`BOTTOM_ROWS`] non-blank lines of the screen.
fn bottom(grid: &str) -> Vec<&str> {
    let lines: Vec<&str> = grid.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(BOTTOM_ROWS)..].to_vec()
}

fn is_footer(l: &str) -> bool {
    l.contains("Esc to")
}

/// `(digit, label)` of a numbered option line (`❯ 1. Yes`, `  2. No`).
fn option(l: &str) -> Option<(char, &str)> {
    let t = l.trim().trim_start_matches('❯').trim();
    let (num, rest) = t.split_once(". ")?;
    let digit = num.parse::<u8>().ok().filter(|d| (1..10).contains(d))?;
    Some((char::from(b'0' + digit), rest.trim()))
}

/// In the bottom rows: the index of the last permission-dialog header (`Do you want to …?`)
/// that is followed by a `1.` option and then a footer.
fn dialog_start(lines: &[&str]) -> Option<usize> {
    let footer = lines.iter().rposition(|l| is_footer(l))?;
    (0..footer).rev().find(|&i| {
        let t = lines[i].trim();
        t.starts_with("Do you want to")
            && t.ends_with('?')
            && lines[i + 1..footer].iter().any(|l| option(l).is_some_and(|(d, _)| d == '1'))
    })
}

/// A permission dialog is open at the bottom of the screen.
pub fn has_permission_prompt(grid: &str) -> bool {
    dialog_start(&bottom(grid)).is_some()
}

/// Any selection dialog at the bottom of the screen: an option list with the cursor on an
/// option (`❯ N.`) and a footer below it. Covers dialogs Mirante does not know (plan
/// approval, model pickers): typed text must never land in one.
pub fn has_any_dialog(grid: &str) -> bool {
    let lines = bottom(grid);
    let Some(footer) = lines.iter().rposition(|l| is_footer(l) || l.contains("Enter to confirm")) else {
        return false;
    };
    lines[..footer].iter().any(|l| l.trim_start().starts_with('❯') && option(l).is_some())
}

/// The digit of the live permission dialog's option whose label is exactly `label`
/// ("Yes" / "No"). Only options below the dialog header and above its footer count, and
/// only an exact label matches, so "Yes, and always allow …" and "Yes, and switch to auto
/// mode …" are never chosen (FINDINGS rule 6).
pub fn option_key(grid: &str, label: &str) -> Option<char> {
    let lines = bottom(grid);
    let start = dialog_start(&lines)?;
    let footer = lines.iter().rposition(|l| is_footer(l))?;
    lines[start + 1..footer].iter().find_map(|l| option(l).filter(|(_, t)| *t == label).map(|(d, _)| d))
}

pub const TRUST_YES: &str = "Yes, I trust this folder";

/// The digit of the trust dialog's exact "Yes, I trust this folder" option, when the
/// dialog is live: that option and a key-hint footer below it in the bottom rows.
pub fn trust_key(grid: &str) -> Option<char> {
    let lines = bottom(grid);
    let i = lines.iter().position(|l| option(l).is_some_and(|(_, t)| t == TRUST_YES))?;
    let footer = lines[i + 1..].iter().any(|l| is_footer(l) || l.contains("Enter to confirm"));
    footer.then(|| option(lines[i]).map(|(d, _)| d)).flatten()
}

/// Claude's workspace-trust dialog (new folder) is live. Its default option is "No, exit".
pub fn is_trust_dialog(grid: &str) -> bool {
    trust_key(grid).is_some()
}

/// Typed text would land in a dialog (permission, trust, or any other selection).
pub fn dialog_open(grid: &str) -> bool {
    has_permission_prompt(grid) || is_trust_dialog(grid) || has_any_dialog(grid)
}

/// The input box is on screen: a `❯` / `>` prompt line in the bottom rows that is not a
/// dialog option.
pub fn has_input_prompt(grid: &str) -> bool {
    bottom(grid).iter().any(|l| {
        let t = l.trim_start();
        (t.starts_with('❯') || t.starts_with('>')) && option(l).is_none()
    })
}

/// The ready fast path: Mirante launches with `--permission-mode manual`, whose footer
/// reads "manual mode on".
pub fn has_manual_mode_footer(grid: &str) -> bool {
    bottom(grid).iter().any(|l| l.contains("manual mode on"))
}

/// Does the dialog on screen concern `detail` (a permission request's command or path)?
/// Compared without whitespace, so a command wrapped across lines still matches: the
/// first 30 characters of the detail, or, for an absolute path, its file name (the dialog
/// shows paths relative to the project). An empty detail matches any dialog.
pub fn dialog_matches(grid: &str, detail: &str) -> bool {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let detail = detail.lines().next().unwrap_or("").trim();
    let lines = bottom(grid);
    let Some(start) = dialog_start(&lines) else {
        return false;
    };
    if detail.is_empty() {
        return true;
    }
    // The tool box sits right above the question: search up to 15 lines before it.
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
//! An agent process in a pseudo-terminal, mirrored into a vt100 grid.
//! - The child is a session and process-group leader (portable-pty calls `setsid`), so
//!   [`PtyProcess::kill_tree`] ends it with everything it started ([`proctree::kill_tree`]).
//! - Terminal queries in the output (cursor position, device attributes, status, colors)
//!   are answered by the daemon, attached or not, and stripped from what attached clients
//!   get ([`termq`]): one answer, never two.
//! - Output fans out to attached clients over bounded queues. A client that falls behind
//!   gets a full repaint; a new client starts from a repaint that also restores the input
//!   modes (bracketed paste, cursor keys, mouse) the agent switched on.
//!
//! The process tree is ended when the `PtyProcess` is dropped.
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use super::proctree::{self, KillReport, Proc};
use super::termq::{self, Piece, TermQueries};
use super::Fanout;

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
    root: Option<Proc>,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Lock order: `fanout`, then `parser` (the reader and `attach` both follow it, so a
    /// new client's repaint and its first live bytes never overlap or leave a gap).
    fanout: Arc<Mutex<Fanout>>,
    exited: Arc<AtomicBool>,
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

pub(crate) fn nz(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n.max(1)).expect("max(1) is non-zero")
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Bytes that redraw `screen` on a fresh terminal, input modes included.
fn repaint_of(screen: &vt100::Screen) -> Vec<u8> {
    let mut out = b"\x1b[2J\x1b[H".to_vec();
    out.extend_from_slice(screen.state_formatted().as_bytes());
    out
}

/// One chunk of agent output: mirror it, answer its queries, fan it out.
fn pump(chunk_pieces: Vec<Piece>, parser: &Mutex<vt100::Parser>, fanout: &Mutex<Fanout>, writer: &Writer) {
    let mut f = lock(fanout);
    let mut live = Vec::new();
    for piece in chunk_pieces {
        match piece {
            Piece::Bytes(b) => {
                lock(parser).process(&b);
                live.extend_from_slice(&b);
            }
            Piece::Query(q) => {
                let cursor = lock(parser).screen().cursor_position();
                let mut w = lock(writer);
                let _ = w.write_all(&termq::answer(q, cursor)).and_then(|()| w.flush());
            }
        }
    }
    if !live.is_empty() {
        f.send(&live);
    }
    if f.needs_repaint() {
        // A client fell behind and lost chunks (bounded queue): send it the whole screen,
        // after which it gets live bytes again.
        let screen = repaint_of(lock(parser).screen());
        f.repaint(&screen);
    }
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
        // Captured now, while the pid surely names our child.
        let root = child.process_id().and_then(|pid| proctree::proc_of(pid as i32));
        let mut reader = pair.master.try_clone_reader().map_err(other)?;
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(other)?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(nz(spec.rows), nz(spec.cols), 0)));
        let fanout = Arc::new(Mutex::new(Fanout::default()));
        let p = Arc::new(PtyProcess {
            master: Mutex::new(pair.master),
            writer: writer.clone(),
            child: Mutex::new(child),
            root,
            parser: parser.clone(),
            fanout: fanout.clone(),
            exited: Arc::new(AtomicBool::new(false)),
        });
        std::thread::spawn(move || {
            let mut queries = TermQueries::default();
            let mut buf = [0u8; 8192];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                pump(queries.feed(&buf[..n]), &parser, &fanout, &writer);
            }
            let rest = queries.flush();
            if !rest.is_empty() {
                pump(vec![Piece::Bytes(rest)], &parser, &fanout, &writer);
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
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
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

    /// Escape sequences that repaint the current screen, input modes included, on a
    /// fresh terminal.
    pub fn repaint(&self) -> Vec<u8> {
        repaint_of(lock(&self.parser).screen())
    }

    /// A new client: its repaint and a live stream that continues exactly after it.
    pub fn attach(&self) -> (u64, Vec<u8>, mpsc::Receiver<Vec<u8>>) {
        let mut f = lock(&self.fanout);
        let initial = repaint_of(lock(&self.parser).screen());
        let (id, rx) = f.subscribe();
        (id, initial, rx)
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

    /// The agent's root process (the session leader), as it was at spawn.
    pub fn root(&self) -> Option<Proc> {
        self.root
    }

    /// Ends the agent and every process below it: SIGTERM, `grace`, SIGKILL, verify.
    pub fn kill_tree(&self, grace: Duration) -> KillReport {
        match self.root {
            Some(root) => proctree::kill_tree(root, grace, &proctree::Kill),
            None => {
                let _ = lock(&self.child).kill();
                KillReport::default()
            }
        }
    }

    /// Waits up to `timeout` for the process to exit.
    pub fn wait_exit(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.exited() || matches!(lock(&self.child).try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Test only: feeds `bytes` into the mirror as if the agent had printed them.
    #[cfg(test)]
    pub fn mirror(&self, bytes: &[u8]) {
        lock(&self.parser).process(bytes);
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        let running = !matches!(lock(&self.child).try_wait(), Ok(Some(_)));
        if running {
            self.kill_tree(Duration::from_millis(200));
        }
        let _ = lock(&self.child).wait();
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 114 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): PTY process in its own group, daemon-answered terminal queries, footer-gated dialog reading, sanitized bracketed paste

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 15: Claude driver and its launch

**Files:**
- Modify: `crates/mirante-daemon/src/driver/mod.rs` (module list)
- Create: `crates/mirante-daemon/src/driver/claude.rs`
- Modify: `crates/mirante-daemon/src/driver/real.rs`
- Modify: `crates/mirante-daemon/src/hub/messages.rs` (refuse what the typist would refuse)

The unmodified interactive `claude` in a PTY (FINDINGS: the mode that fits the terms, steers mid-run and resumes), launched from Plan 2a's `claude_launch::launch_spec`: the real binary (`MIRANTE_CLAUDE_BIN` or PATH without shims), nested-session and terminal variables scrubbed (auth never touched), `--permission-mode manual`, Mirante's hooks added through `--settings` (a 0600 file in a 0700 `~/.mirante/sessions/<task>/`), the agent terminal variables (`TERM=xterm-256color`, `COLORTERM`, `TERM_PROGRAM=mirante`), `MIRANTE_TASK_ID`, and `MIRANTE_SESSION_ID` = the Mirante session id of the spawn request. The hook echoes that id back and the hub drops every hook stamped with another session (`hook_stale`), so this variable must be exactly `SpawnRequest.session_id`; a test asserts it. The factory adds `MIRANTE_HOME` (the hook must reach this daemon). Never `--dangerously-skip-permissions`; the allowlist is empty.

- **Native id up front.** A fresh start passes `--session-id <uuid v4>` (16 bytes from `/dev/urandom`, version and variant bits set) and emits `NativeId(uuid)` before Claude prints anything, so even a crash during boot leaves a resumable id. A resume passes `--resume <id>`. A resumed process that exits within `resume_window` (3 s) reports `ResumeFailed` before `Exited`; the hub records `resume_failed` and starting a fresh conversation stays the user's call (`mirante resume <task> --fresh`, Task 13). The driver never starts fresh on its own.
- **Ready** needs all of: the hub saw this session's `SessionStart` hook (`Driver::session_started`), no dialog on screen, and either the "manual mode on" footer (fast path) or an input prompt (`❯` / `>`) on a screen unchanged for `ready_stable` (500 ms). There is no time-based fallback: an unknown startup screen keeps the session `Booting` (the user attaches) instead of receiving typed text. The trust dialog is looked for only before the first `Ready`.
- **Trust.** Reported as `Boot(TrustPrompt)`, answered only by the user. Accept types the digit of the option labelled exactly "Yes, I trust this folder" (never Down+Enter, which depends on the option order); no such option on screen is `NoDialog`. Decline ends the process tree.
- **Messages** go through one typist thread, one at a time, so two sends never interleave: a pause (`pre_type`), the bracketed paste (or a slash command's keystrokes; `!` is refused, Task 14), a pause (`type_pause`, the TUI drops an Enter that arrives too fast), Enter. Nothing is typed before `Ready` (`NotNow`). Right before the paste AND right before Enter the typist checks again that no dialog is on screen and that the hub holds no pending permission for this session (`Sink::held`, Task 13): a dialog can open between `send` and the keystrokes, and typed text, or an Enter, would answer it. The second check matters because a dialog can open during `type_pause`; Enter would then confirm its default option. A message stopped before the paste is handed back untouched (`DeliveryAborted`, the hub re-queues it at the front); stopped after the paste, the input line is cleared with Ctrl-U, which no dialog takes as an answer (never Enter, Esc or a digit), and the message is handed back the same way.
- **Interrupt** = Esc. **Permission answers** type the digit of the exact option, only if the dialog on screen matches the request's detail (first 30 non-space characters, or the file name of an absolute path).
- **Transcript tail.** One thread per transcript path (switches on `/clear`), polling `TranscriptTail` and looping while `more` (never while offset < length), emitting lines then the new offset.
- **Stop** ends the whole process tree (`kill_tree`: SIGTERM to the group and every descendant, `stop_grace`, SIGKILL, verify), not Esc + `/exit`: a hung TUI or a background child (a dev server, a test watcher) must not outlive the session. Dropping the driver does the same with a short grace.

- [ ] **Step 1: Write the failing tests**

In `crates/mirante-daemon/src/driver/mod.rs`, replace the module list with:
```rust
pub mod claude;
#[cfg(test)]
pub mod fake;
pub mod paste;
pub mod proctree;
pub mod pty;
pub mod real;
pub mod screen;
pub mod termq;
```

`crates/mirante-daemon/src/driver/claude.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::proctree;
    use mirante_ipc::test_dir::TestDir;
    use std::sync::atomic::AtomicBool;

    fn fast() -> ClaudeTiming {
        ClaudeTiming {
            tick: Duration::from_millis(20),
            pre_type: Duration::from_millis(150),
            type_pause: Duration::from_millis(50),
            tail_poll: Duration::from_millis(20),
            stop_grace: Duration::from_secs(1),
            boot_limit: Duration::from_secs(10),
            ready_stable: Duration::from_millis(150),
            resume_window: Duration::from_secs(3),
        }
    }

    struct Fake {
        d: Arc<ClaudeDriver>,
        rx: mpsc::Receiver<(String, DriverEvent)>,
        /// The hub's side of the hold flag.
        hold: Arc<AtomicBool>,
    }

    /// A fake `claude`: a shell script in the PTY (raw mode, no echo).
    fn fake_with(script: &str, timing: ClaudeTiming, resuming: bool) -> Fake {
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
        let sink = Sink::new("s1", tx);
        let hold = sink.hold_flag();
        Fake { d: ClaudeDriver::spawn(spec, sink, timing, resuming).unwrap(), rx, hold }
    }

    fn fake(script: &str) -> Fake {
        fake_with(script, fast(), false)
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

    /// Ready on screen (footer fast path); the test still has to report `SessionStart`.
    const READY: &str = "printf '\\033[2J\\033[H> \\r\\n  manual mode on\\r\\n'";

    fn ready(f: &Fake) {
        f.d.session_started();
        assert_eq!(next(&f.rx, |e| matches!(e, DriverEvent::Boot(_))), DriverEvent::Boot(BootState::Ready));
    }

    const DIALOG: &[u8] = b"\x1b[2J\x1b[H Bash command\r\n   touch x\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n   2. No\r\n Esc to cancel\r\n";

    #[test]
    fn the_trust_prompt_waits_for_the_user_then_its_exact_option_is_typed() {
        let f = fake(
            "printf 'Do you trust the files in this folder?\\r\\n \\342\\235\\257 1. No, exit\\r\\n   2. Yes, I trust this folder\\r\\n Enter to confirm \\302\\267 Esc to exit\\r\\n'; \
             k=$(dd bs=1 count=1 2>/dev/null); \
             printf '\\033[2J\\033[Hkeys:%s\\r\\n> \\r\\n  manual mode on\\r\\n' \"$k\"; sleep 5",
        );
        assert_eq!(next(&f.rx, |e| matches!(e, DriverEvent::Boot(_))), DriverEvent::Boot(BootState::TrustPrompt));
        assert!(f.d.dialog_open());
        assert!(matches!(f.d.send("hi"), Err(DriverError::NotNow(_))));
        f.d.trust(true).unwrap();
        wait_grid(&f.d, "keys:2");
        ready(&f);
        assert!(matches!(f.d.trust(true), Err(DriverError::NoDialog(_))));
    }

    #[test]
    fn declining_trust_ends_the_process() {
        let f = fake(
            "printf ' \\342\\235\\257 1. No, exit\\r\\n   2. Yes, I trust this folder\\r\\n Enter to confirm\\r\\n'; sleep 30",
        );
        next(&f.rx, |e| *e == DriverEvent::Boot(BootState::TrustPrompt));
        f.d.trust(false).unwrap();
        assert!(matches!(next(&f.rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { .. }));
    }

    #[test]
    fn ready_needs_session_start_and_a_settled_input_prompt() {
        let f = fake("printf '> '; sleep 5");
        wait_grid(&f.d, ">");
        // The prompt alone is not enough: no SessionStart yet.
        std::thread::sleep(Duration::from_millis(400));
        assert!(f.rx.try_iter().all(|(_, e)| !matches!(e, DriverEvent::Boot(_))));
        ready(&f);
    }

    #[test]
    fn nothing_is_typed_before_ready() {
        let f = fake(&format!(
            "{READY}; stty min 0 time 10; k=$(dd bs=1 count=10 2>/dev/null); printf 'got:[%s]' \"$k\"; sleep 5"
        ));
        wait_grid(&f.d, "manual mode on");
        assert!(matches!(f.d.send("hi"), Err(DriverError::NotNow(_))));
        wait_grid(&f.d, "got:[]");
    }

    #[test]
    fn messages_are_one_bracketed_paste_then_enter() {
        let dir = TestDir::new();
        let out = dir.path().join("typed");
        let want = [paste::bracketed("use\r\ntabs\x1b[201~ now"), b"\r".to_vec(), b"/compact\r".to_vec()].concat();
        let f = fake(&format!(
            "{READY}; dd bs=1 count={} of={} 2>/dev/null; printf 'done'; sleep 5",
            want.len(),
            out.display()
        ));
        ready(&f);
        assert!(f.d.send("!rm -rf build").unwrap_err().to_string().contains("attach and type it yourself"));
        f.d.send("use\r\ntabs\x1b[201~ now").unwrap();
        f.d.send("/compact").unwrap();
        wait_grid(&f.d, "done");
        assert_eq!(std::fs::read(&out).unwrap(), want);
        assert_eq!(want[..6], *b"\x1b[200~");
    }

    #[test]
    fn two_concurrent_sends_never_interleave() {
        let dir = TestDir::new();
        let out = dir.path().join("typed");
        let (a, b) = ("first message ".repeat(20), "second message ".repeat(20));
        let pa = [paste::bracketed(&a), b"\r".to_vec()].concat();
        let pb = [paste::bracketed(&b), b"\r".to_vec()].concat();
        let f = fake(&format!(
            "{READY}; dd bs=1 count={} of={} 2>/dev/null; printf 'done'; sleep 5",
            pa.len() + pb.len(),
            out.display()
        ));
        ready(&f);
        let (d1, d2) = (f.d.clone(), f.d.clone());
        let t1 = std::thread::spawn(move || d1.send(&a).unwrap());
        let t2 = std::thread::spawn(move || d2.send(&b).unwrap());
        t1.join().unwrap();
        t2.join().unwrap();
        wait_grid(&f.d, "done");
        let got = std::fs::read(&out).unwrap();
        assert!(
            got == [pa.clone(), pb.clone()].concat() || got == [pb, pa].concat(),
            "{:?}",
            String::from_utf8_lossy(&got)
        );
    }

    #[test]
    fn a_dialog_that_appears_before_the_keystrokes_gets_nothing() {
        let f = fake(&format!(
            "{READY}; stty min 0 time 15; k=$(dd bs=1 count=10 2>/dev/null); printf 'got:[%s]' \"$k\"; sleep 5"
        ));
        ready(&f);
        f.d.send("1").unwrap();
        // The dialog opens between `send` and the first keystroke.
        f.d.pty.mirror(DIALOG);
        let ev = next(&f.rx, |e| matches!(e, DriverEvent::DeliveryAborted { .. }));
        assert!(
            matches!(ev, DriverEvent::DeliveryAborted { ref text, ref reason } if text == "1" && reason.contains("nothing was typed"))
        );
        wait_grid(&f.d, "got:[]");
    }

    #[test]
    fn a_permission_pending_before_enter_clears_the_line_instead_of_submitting() {
        let pasted = paste::bracketed("go on");
        let f = fake_with(
            &format!(
                "{READY}; dd bs=1 count={} >/dev/null 2>&1; printf 'pasted'; stty min 0 time 20; \
                 k=$(dd bs=1 count=1 2>/dev/null | od -An -b | tr -d ' '); printf ' next:[%s]' \"$k\"; sleep 5",
                pasted.len()
            ),
            ClaudeTiming { type_pause: Duration::from_millis(800), ..fast() },
            false,
        );
        ready(&f);
        f.d.send("go on").unwrap();
        wait_grid(&f.d, "pasted");
        f.hold.store(true, Ordering::SeqCst);
        let ev = next(&f.rx, |e| matches!(e, DriverEvent::DeliveryAborted { .. }));
        assert!(matches!(ev, DriverEvent::DeliveryAborted { ref reason, .. } if reason.contains("before Enter")));
        // Ctrl-U (octal 025), never Enter.
        wait_grid(&f.d, "next:[025]");
        assert!(matches!(f.d.send("more"), Err(DriverError::Failed(_))));
    }

    #[test]
    fn permission_answer_types_the_exact_option_for_the_matching_dialog() {
        let dialog = "printf '\\033[2J\\033[H Bash command\\r\\n   touch x\\r\\n Do you want to proceed?\\r\\n \\342\\235\\257 1. Yes\\r\\n   2. Yes, and always allow access to /w\\r\\n   3. No\\r\\n Esc to cancel\\r\\n'";
        let f = fake(&format!(
            "{READY}; sleep 1; {dialog}; k=$(dd bs=1 count=1 2>/dev/null); printf '\\r\\npicked:%s' \"$k\"; sleep 5"
        ));
        ready(&f);
        wait_grid(&f.d, "Do you want to proceed?");
        assert!(f.d.dialog_open());
        assert!(matches!(f.d.answer_permission("rm -rf /", true), Err(DriverError::NoDialog(_))));
        f.d.answer_permission("touch x", false).unwrap();
        wait_grid(&f.d, "picked:3");
    }

    #[test]
    fn transcript_lines_and_offsets_are_emitted_from_the_given_offset() {
        let dir = TestDir::new();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "{\"old\":true}\n{\"type\":\"user\"}\n").unwrap();
        let f = fake("sleep 5");
        f.d.tail_transcript(&path, 13);
        assert_eq!(
            next(&f.rx, |e| matches!(e, DriverEvent::Transcript(_))),
            DriverEvent::Transcript(json!({"type": "user"}))
        );
        let off = next(&f.rx, |e| matches!(e, DriverEvent::TranscriptOffset { .. }));
        assert_eq!(off, DriverEvent::TranscriptOffset { path: path.to_string_lossy().into_owned(), offset: 29 });
        f.d.tail_transcript(&path, 0);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"n\":2}\n"))
            .unwrap();
        assert_eq!(next(&f.rx, |e| matches!(e, DriverEvent::Transcript(_))), DriverEvent::Transcript(json!({"n": 2})));
    }

    #[test]
    fn stop_ends_the_whole_tree_even_a_child_forked_during_the_kill() {
        let dir = TestDir::new();
        let (pidfile, readyfile, child) =
            (dir.path().join("forked.pid"), dir.path().join("ready"), dir.path().join("child.sh"));
        std::fs::write(
            &child,
            format!(
                "trap 'perl -e \"setpgrp(0,0); sleep 60\" & echo $! > {}' TERM HUP\n: > {}\nwhile :; do sleep 0.1; done\n",
                pidfile.display(),
                readyfile.display()
            ),
        )
        .unwrap();
        let f = fake(&format!("sh {} & wait; sleep 60", child.display()));
        let root = f.d.root().expect("root");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !readyfile.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        f.d.stop();
        assert!(matches!(next(&f.rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { .. }));
        assert!(!proctree::alive(root));
        let forked: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(proctree::proc_of(forked).is_none(), "the child forked during the kill survived");
    }

    #[test]
    fn a_resume_that_exits_at_once_is_reported_never_restarted() {
        let f = fake_with("exit 3", fast(), true);
        let ev = next(&f.rx, |e| matches!(e, DriverEvent::ResumeFailed { .. } | DriverEvent::Exited { .. }));
        assert!(matches!(ev, DriverEvent::ResumeFailed { ref reason } if reason.contains("--resume")), "{ev:?}");
        assert_eq!(next(&f.rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { code: Some(3) });
        // A fresh start that exits is just an exit.
        let g = fake("exit 3");
        let ev = next(&g.rx, |e| matches!(e, DriverEvent::ResumeFailed { .. } | DriverEvent::Exited { .. }));
        assert_eq!(ev, DriverEvent::Exited { code: Some(3) });
    }

    #[test]
    fn attach_repaints_with_input_modes_then_streams_and_input_reaches_the_agent() {
        let f =
            fake("printf '\\033[?2004hhello'; k=$(dd bs=1 count=2 2>/dev/null); printf ' typed:%s' \"$k\"; sleep 5");
        wait_grid(&f.d, "hello");
        let a = f.d.attach(24, 80).unwrap();
        let initial = String::from_utf8_lossy(&a.initial);
        assert!(initial.contains("hello") && initial.contains("\x1b[?2004h"), "{initial:?}");
        f.d.input(a.id, b"ok").unwrap();
        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains("typed:ok") {
            seen.extend(a.output.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        f.d.detach(a.id);
    }

    #[test]
    fn arbitrary_bytes_pass_through_attach_unchanged() {
        let dir = TestDir::new();
        let file = dir.path().join("bytes");
        // Not UTF-8, every value but ESC (the one byte terminal queries start with).
        let mut x: u32 = 0x1234_5678;
        let data: Vec<u8> = std::iter::from_fn(|| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            Some((x >> 24) as u8)
        })
        .filter(|b| *b != 0x1b)
        .take(4096)
        .collect();
        assert!(std::str::from_utf8(&data).is_err());
        std::fs::write(&file, &data).unwrap();
        let f = fake(&format!("printf R; dd bs=1 count=1 >/dev/null 2>&1; cat {}; sleep 5", file.display()));
        wait_grid(&f.d, "R");
        let a = f.d.attach(30, 100).unwrap();
        f.d.input(a.id, b"g").unwrap();
        let mut seen = Vec::new();
        while seen.len() < data.len() {
            seen.extend(a.output.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        assert_eq!(seen, data);
    }
}
```

Append to `crates/mirante-daemon/src/driver/real.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{Lifecycle, Status, Task};
    use mirante_ipc::test_dir::TestDir;
    use mirante_ipc::Paths;

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

    #[test]
    fn uuids_are_random_version_4() {
        let (a, b) = (new_uuid_v4().unwrap(), new_uuid_v4().unwrap());
        assert_ne!(a, b);
        let parts: Vec<usize> = a.split('-').map(str::len).collect();
        assert_eq!(parts, vec![8, 4, 4, 4, 12]);
        assert_eq!(&a[14..15], "4");
        assert!("89ab".contains(&a[19..20]), "{a}");
        assert!(a.chars().all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    fn request(resume: Option<&str>) -> SpawnRequest {
        SpawnRequest {
            task: Task {
                id: "task-1".into(),
                title: "T".into(),
                repo_path: "/r".into(),
                worktree_path: "/w/tree".into(),
                branch: "feat".into(),
                agent: AgentKind::Claude,
                goal: "g".into(),
                notes: String::new(),
                lifecycle: Lifecycle::Active,
                status: Status::Idle,
                created_at: 0,
                updated_at: 0,
            },
            session_id: "sess-42".into(),
            resume: resume.map(String::from),
            paths: Paths::new(Path::new("/h")),
        }
    }

    fn env<'a>(spec: &'a PtySpec, key: &str) -> Option<&'a str> {
        spec.env_set.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_fresh_launch_pins_the_session_id_and_stamps_the_mirante_session() {
        let parent = vec!["CLAUDECODE".to_string(), "ANTHROPIC_API_KEY".to_string()];
        let (spec, pinned) =
            claude_spec(&request(None), Path::new("/bin/claude"), Path::new("/h/s.json"), (40, 120), parent).unwrap();
        let id = pinned.expect("fresh start pins an id");
        let args = spec.args.join(" ");
        assert!(args.contains(&format!("--session-id {id}")), "{args}");
        assert!(args.contains("--permission-mode manual") && args.contains("--settings /h/s.json"), "{args}");
        assert!(!args.contains("--resume") && !args.contains("dangerously"), "{args}");
        assert_eq!(env(&spec, "MIRANTE_SESSION_ID"), Some("sess-42"));
        assert_eq!(env(&spec, "MIRANTE_TASK_ID"), Some("task-1"));
        assert_eq!(env(&spec, "MIRANTE_HOME"), Some("/h"));
        assert_eq!(env(&spec, "TERM"), Some("xterm-256color"));
        assert!(spec.env_remove.contains(&"CLAUDECODE".to_string()));
        assert!(!spec.env_remove.iter().any(|k| k.starts_with("ANTHROPIC_")));
        assert_eq!((spec.cwd.as_path(), spec.rows, spec.cols), (Path::new("/w/tree"), 40, 120));
    }

    #[test]
    fn a_resume_passes_the_native_id_and_pins_nothing() {
        let (spec, pinned) =
            claude_spec(&request(Some("nat-1")), Path::new("/bin/claude"), Path::new("/h/s.json"), (40, 120), vec![])
                .unwrap();
        assert_eq!(pinned, None);
        let args = spec.args.join(" ");
        assert!(args.contains("--resume nat-1") && !args.contains("--session-id"), "{args}");
        assert_eq!(env(&spec, "MIRANTE_SESSION_ID"), Some("sess-42"));
    }
}
```

In `crates/mirante-daemon/src/hub/messages.rs`, add to the test module (before `queue_on_an_idle_agent_is_delivered_at_once`):
```rust
    #[test]
    fn claude_messages_the_typist_would_refuse_are_refused_up_front() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "a").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        for text in ["!rm -rf build", "/compact\nnow"] {
            for mode in [SendMode::Steer, SendMode::Queue] {
                assert!(matches!(send(&k, &t.id, text, mode), Err(HubError::Invalid(_))), "{text:?}");
            }
        }
        assert_eq!(k.hub.get_task(&t.id).unwrap().queued, 0);
        assert!(send(&k, &t.id, "/compact", SendMode::Queue).is_ok());
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
//! is read only to act on dialogs.
//! - Boot: the workspace-trust dialog is reported as `TrustPrompt` and answered only by the
//!   user (`trust`, which types the digit of the exact "Yes, I trust this folder" option).
//!   `Ready` needs the `SessionStart` hook, the input prompt, no dialog, and a screen that
//!   stayed unchanged for `ready_stable` (the "manual mode on" footer skips the wait).
//!   There is no time-based fallback: an unknown startup screen keeps the session booting.
//! - Messages are typed by one typist thread, one at a time: a bracketed paste, a pause,
//!   Enter. Right before the paste AND right before Enter the typist checks again that no
//!   dialog is on screen and that the hub holds no pending permission for the session:
//!   a dialog can appear between `send` and the keystrokes, and typed text would answer it.
//!   A message stopped that way is handed back (`DeliveryAborted`); stopped after the
//!   paste, the input line is cleared with Ctrl-U (never Enter, Esc or a digit, which a
//!   dialog would take as an answer).
//! - Interrupt is Esc; permission answers type the digit of the exact "Yes" / "No" option
//!   of the dialog that matches the request.
//! - Stop ends the whole process tree (SIGTERM, grace, SIGKILL; see `proctree`).
//! - A `--resume` whose process exits within `resume_window` reports `ResumeFailed`; a
//!   fresh start is the user's call.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use mirante_core::transcript_tail::TranscriptTail;
use mirante_ipc::types::BootState;
use serde_json::json;

use super::paste;
use super::proctree::Proc;
use super::pty::{PtyProcess, PtySpec};
use super::{screen, Attachment, Driver, DriverError, DriverEvent, Sink};

const ESC: &[u8] = b"\x1b";
/// Ctrl-U: clears the input line. Not an answer to any dialog.
const CLEAR_LINE: &[u8] = b"\x15";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeTiming {
    /// Screen polling interval during boot.
    pub tick: Duration,
    /// Pause between `send` and the first keystroke (lets a dialog that is about to open
    /// show up before anything is typed).
    pub pre_type: Duration,
    /// Pause between the pasted text and Enter (the TUI drops an Enter that arrives too fast).
    pub type_pause: Duration,
    pub tail_poll: Duration,
    /// SIGTERM to SIGKILL on stop.
    pub stop_grace: Duration,
    /// The boot watcher gives up after this long (the session stays `Booting`).
    pub boot_limit: Duration,
    /// How long the input prompt must stay unchanged before `Ready` (no footer fast path).
    pub ready_stable: Duration,
    /// A resumed process that exits sooner than this failed to resume.
    pub resume_window: Duration,
}

impl Default for ClaudeTiming {
    fn default() -> Self {
        ClaudeTiming {
            tick: Duration::from_millis(100),
            pre_type: Duration::from_millis(150),
            type_pause: Duration::from_millis(400),
            tail_poll: Duration::from_millis(300),
            stop_grace: Duration::from_secs(3),
            boot_limit: Duration::from_secs(600),
            ready_stable: Duration::from_millis(500),
            resume_window: Duration::from_secs(3),
        }
    }
}

enum Typing {
    /// (the message as given, the bytes to type before Enter)
    Message(String, Vec<u8>),
    /// Keys for a dialog or the TUI (answers, Esc): typed as they are.
    Keys(Vec<u8>),
}

pub struct ClaudeDriver {
    pty: Arc<PtyProcess>,
    sink: Sink,
    typist: Mutex<mpsc::Sender<Typing>>,
    timing: ClaudeTiming,
    tail_path: Mutex<Option<PathBuf>>,
    /// Bumped to stop the current tail thread (new path, stop, exit).
    tail_gen: Arc<AtomicU64>,
    /// The hub saw this session's `SessionStart` hook.
    started: Arc<AtomicBool>,
    /// `Ready` was reported: messages may be typed; the trust prompt is no longer looked for.
    ready: Arc<AtomicBool>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Why typing must not happen now, if it must not.
fn blocked(pty: &PtyProcess, sink: &Sink) -> Option<&'static str> {
    if sink.held() {
        Some("a permission request is pending")
    } else if screen::dialog_open(&pty.grid()) {
        Some("a dialog is open on the agent's screen")
    } else {
        None
    }
}

fn type_message(pty: &PtyProcess, sink: &Sink, timing: ClaudeTiming, text: String, bytes: &[u8]) {
    std::thread::sleep(timing.pre_type);
    if let Some(why) = blocked(pty, sink) {
        sink.emit(DriverEvent::DeliveryAborted { text, reason: format!("{why}; nothing was typed") });
        return;
    }
    if pty.write(bytes).is_err() {
        sink.emit(DriverEvent::DeliveryAborted { text, reason: "the agent's terminal is closed".into() });
        return;
    }
    std::thread::sleep(timing.type_pause);
    if let Some(why) = blocked(pty, sink) {
        let _ = pty.write(CLEAR_LINE);
        let reason = format!("{why} before Enter; the typed text was cleared, not sent");
        sink.emit(DriverEvent::DeliveryAborted { text, reason });
        return;
    }
    let _ = pty.write(b"\r");
}

impl ClaudeDriver {
    /// Spawns `spec`. `resuming`: the launch passes `--resume` (an exit within
    /// `resume_window` is then reported as `ResumeFailed`).
    pub fn spawn(
        spec: PtySpec,
        sink: Sink,
        timing: ClaudeTiming,
        resuming: bool,
    ) -> Result<Arc<ClaudeDriver>, DriverError> {
        let tail_gen = Arc::new(AtomicU64::new(0));
        let (exit_sink, exit_gen) = (sink.clone(), tail_gen.clone());
        let born = Instant::now();
        let pty = PtyProcess::spawn(&spec, move |code| {
            exit_gen.fetch_add(1, Ordering::SeqCst);
            let after = born.elapsed();
            if resuming && after < timing.resume_window {
                let reason = format!(
                    "claude exited {:.1} s after --resume (exit code {code:?}); its conversation may be gone",
                    after.as_secs_f32()
                );
                exit_sink.emit(DriverEvent::ResumeFailed { reason });
            }
            exit_sink.emit(DriverEvent::Exited { code });
        })?;
        let (tx, rx) = mpsc::channel::<Typing>();
        let (typist_pty, typist_sink) = (pty.clone(), sink.clone());
        std::thread::spawn(move || {
            for t in rx {
                match t {
                    Typing::Message(text, bytes) => type_message(&typist_pty, &typist_sink, timing, text, &bytes),
                    Typing::Keys(k) => {
                        let _ = typist_pty.write(&k);
                    }
                }
            }
        });
        let started = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let driver = Arc::new(ClaudeDriver {
            pty: pty.clone(),
            sink: sink.clone(),
            typist: Mutex::new(tx),
            timing,
            tail_path: Mutex::new(None),
            tail_gen,
            started: started.clone(),
            ready: ready.clone(),
        });
        std::thread::spawn(move || watch_boot(&pty, &sink, timing, &started, &ready));
        Ok(driver)
    }

    fn type_(&self, t: Typing) -> Result<(), DriverError> {
        lock(&self.typist).send(t).map_err(|_| DriverError::Failed("typist stopped".into()))
    }
}

/// Reports `TrustPrompt` when the trust dialog shows (once per appearance, only before the
/// first `Ready`) and `Ready` once: `SessionStart` seen, no dialog, and either the
/// "manual mode on" footer or an input prompt on a screen unchanged for `ready_stable`.
/// Never answers anything itself.
fn watch_boot(pty: &PtyProcess, sink: &Sink, timing: ClaudeTiming, started: &AtomicBool, ready: &AtomicBool) {
    let start = Instant::now();
    let mut trust_shown = false;
    let (mut last, mut since) = (String::new(), Instant::now());
    while !pty.exited() && start.elapsed() < timing.boot_limit {
        let grid = pty.grid();
        if grid != last {
            last = grid.clone();
            since = Instant::now();
        }
        if screen::is_trust_dialog(&grid) {
            if !trust_shown {
                trust_shown = true;
                sink.emit(DriverEvent::Boot(BootState::TrustPrompt));
            }
        } else {
            trust_shown = false;
            let settled = screen::has_manual_mode_footer(&grid)
                || (screen::has_input_prompt(&grid) && since.elapsed() >= timing.ready_stable);
            if started.load(Ordering::SeqCst) && !screen::dialog_open(&grid) && settled {
                ready.store(true, Ordering::SeqCst);
                sink.emit(DriverEvent::Boot(BootState::Ready));
                return;
            }
        }
        std::thread::sleep(timing.tick);
    }
}

impl Drop for ClaudeDriver {
    fn drop(&mut self) {
        self.tail_gen.fetch_add(1, Ordering::SeqCst);
        if !self.pty.exited() {
            self.pty.kill_tree(Duration::from_millis(200));
        }
    }
}

impl Driver for ClaudeDriver {
    fn send(&self, text: &str) -> Result<(), DriverError> {
        if !self.ready.load(Ordering::SeqCst) {
            return Err(DriverError::NotNow("Claude is not ready yet (starting, or at the trust prompt)".into()));
        }
        if blocked(&self.pty, &self.sink).is_some() {
            return Err(DriverError::Failed("a dialog is open on the agent's screen".into()));
        }
        let bytes = paste::typed_message(text).map_err(DriverError::Failed)?;
        self.type_(Typing::Message(text.to_string(), bytes))
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
        if self.ready.load(Ordering::SeqCst) {
            return Err(DriverError::NoDialog("the session is past its trust prompt".into()));
        }
        let key = screen::trust_key(&self.pty.grid())
            .ok_or_else(|| DriverError::NoDialog(format!("no trust dialog with a \"{}\" option", screen::TRUST_YES)))?;
        if !accept {
            let (pty, grace) = (self.pty.clone(), self.timing.stop_grace);
            std::thread::spawn(move || pty.kill_tree(grace));
            return Ok(());
        }
        self.type_(Typing::Keys(vec![key as u8]))
    }

    fn dialog_open(&self) -> bool {
        screen::dialog_open(&self.pty.grid())
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
        let (id, initial, output) = self.pty.attach();
        Ok(Attachment { id, initial, output, input: true })
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
            self.pty.kill_tree(self.timing.stop_grace);
            self.pty.wait_exit(Duration::from_secs(2));
        }
    }

    fn session_started(&self) {
        self.started.store(true, Ordering::SeqCst);
    }

    fn root(&self) -> Option<Proc> {
        self.pty.root()
    }
}
```

Replace `crates/mirante-daemon/src/driver/real.rs` with:
```rust
//! The production driver factory: the real `claude` in a PTY with Mirante's scoped hook
//! settings (the real `codex app-server` is wired in by Task 17). Binaries:
//! `MIRANTE_CLAUDE_BIN`, else PATH without wrapper shims. The hook command:
//! `MIRANTE_HOOK_BIN`, else `mirante-hook` next to the daemon executable.
//!
//! Claude's launch comes from Plan 2a's `claude_launch::launch_spec` (real binary, env
//! scrub that never touches auth, `--permission-mode manual`, additive `--settings`, the
//! agent terminal variables, `MIRANTE_TASK_ID` and `MIRANTE_SESSION_ID` = this Mirante
//! session, which the hook echoes back and the hub fences on). Added here: `MIRANTE_HOME`
//! (the hook must reach this daemon) and, on a fresh start, `--session-id <uuid>`, so the
//! native id is known before Claude prints anything.
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mirante_core::claude_launch::{launch_spec, resolve_claude_bin_from_env, settings_json, LaunchInput};
use mirante_core::model::AgentKind;
use mirante_ipc::paths::HOME_ENV;

use super::claude::{ClaudeDriver, ClaudeTiming};
use super::pty::PtySpec;
use super::{Driver, DriverError, DriverEvent, DriverFactory, Sink, SpawnRequest};

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

/// A random (version 4) UUID, lowercase `8-4-4-4-12`, from `/dev/urandom`.
pub fn new_uuid_v4() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

fn parent_env() -> Vec<String> {
    std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect()
}

/// The PTY launch for `req`, and the native session id pinned for a fresh start
/// (`None` when resuming: the id to resume is `req.resume`).
pub fn claude_spec(
    req: &SpawnRequest,
    claude: &Path,
    settings: &Path,
    size: (u16, u16),
    parent_env: Vec<String>,
) -> std::io::Result<(PtySpec, Option<String>)> {
    let spec = launch_spec(LaunchInput {
        claude_bin: claude,
        worktree: Path::new(&req.task.worktree_path),
        settings_path: settings,
        task_id: &req.task.id,
        session_id: &req.session_id,
        resume: req.resume.as_deref(),
        parent_env,
    });
    let mut args = spec.args;
    let pinned = match req.resume {
        Some(_) => None,
        None => {
            let id = new_uuid_v4()?;
            args.push("--session-id".to_string());
            args.push(id.clone());
            Some(id)
        }
    };
    let mut env_set = spec.env_set;
    // The hook (a child of claude) must reach this daemon, not one under another home.
    env_set.push((HOME_ENV.to_string(), req.paths.home().to_string_lossy().into_owned()));
    let pty = PtySpec {
        program: spec.program,
        args,
        cwd: spec.cwd,
        env_remove: spec.env_remove,
        env_set,
        rows: size.0,
        cols: size.1,
    };
    Ok((pty, pinned))
}

impl RealFactory {
    fn claude(&self, req: &SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        let claude = resolve_claude_bin_from_env().map_err(|e| DriverError::Failed(e.to_string()))?;
        let hook = hook_bin()?;
        let settings = req.paths.session_dir(&req.task.id).join("claude-settings.json");
        write_private(&settings, &settings_json(&hook.to_string_lossy(), &[]).to_string())?;
        let (pty, pinned) = claude_spec(req, &claude, &settings, (self.rows, self.cols), parent_env())?;
        if let Some(id) = pinned {
            // Known before Claude starts: a crash during boot can still be resumed.
            sink.emit(DriverEvent::NativeId(id));
        }
        Ok(ClaudeDriver::spawn(pty, sink, ClaudeTiming::default(), req.resume.is_some())?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{Lifecycle, Status, Task};
    use mirante_ipc::test_dir::TestDir;
    use mirante_ipc::Paths;

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

    #[test]
    fn uuids_are_random_version_4() {
        let (a, b) = (new_uuid_v4().unwrap(), new_uuid_v4().unwrap());
        assert_ne!(a, b);
        let parts: Vec<usize> = a.split('-').map(str::len).collect();
        assert_eq!(parts, vec![8, 4, 4, 4, 12]);
        assert_eq!(&a[14..15], "4");
        assert!("89ab".contains(&a[19..20]), "{a}");
        assert!(a.chars().all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    fn request(resume: Option<&str>) -> SpawnRequest {
        SpawnRequest {
            task: Task {
                id: "task-1".into(),
                title: "T".into(),
                repo_path: "/r".into(),
                worktree_path: "/w/tree".into(),
                branch: "feat".into(),
                agent: AgentKind::Claude,
                goal: "g".into(),
                notes: String::new(),
                lifecycle: Lifecycle::Active,
                status: Status::Idle,
                created_at: 0,
                updated_at: 0,
            },
            session_id: "sess-42".into(),
            resume: resume.map(String::from),
            paths: Paths::new(Path::new("/h")),
        }
    }

    fn env<'a>(spec: &'a PtySpec, key: &str) -> Option<&'a str> {
        spec.env_set.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_fresh_launch_pins_the_session_id_and_stamps_the_mirante_session() {
        let parent = vec!["CLAUDECODE".to_string(), "ANTHROPIC_API_KEY".to_string()];
        let (spec, pinned) =
            claude_spec(&request(None), Path::new("/bin/claude"), Path::new("/h/s.json"), (40, 120), parent).unwrap();
        let id = pinned.expect("fresh start pins an id");
        let args = spec.args.join(" ");
        assert!(args.contains(&format!("--session-id {id}")), "{args}");
        assert!(args.contains("--permission-mode manual") && args.contains("--settings /h/s.json"), "{args}");
        assert!(!args.contains("--resume") && !args.contains("dangerously"), "{args}");
        assert_eq!(env(&spec, "MIRANTE_SESSION_ID"), Some("sess-42"));
        assert_eq!(env(&spec, "MIRANTE_TASK_ID"), Some("task-1"));
        assert_eq!(env(&spec, "MIRANTE_HOME"), Some("/h"));
        assert_eq!(env(&spec, "TERM"), Some("xterm-256color"));
        assert!(spec.env_remove.contains(&"CLAUDECODE".to_string()));
        assert!(!spec.env_remove.iter().any(|k| k.starts_with("ANTHROPIC_")));
        assert_eq!((spec.cwd.as_path(), spec.rows, spec.cols), (Path::new("/w/tree"), 40, 120));
    }

    #[test]
    fn a_resume_passes_the_native_id_and_pins_nothing() {
        let (spec, pinned) =
            claude_spec(&request(Some("nat-1")), Path::new("/bin/claude"), Path::new("/h/s.json"), (40, 120), vec![])
                .unwrap();
        assert_eq!(pinned, None);
        let args = spec.args.join(" ");
        assert!(args.contains("--resume nat-1") && !args.contains("--session-id"), "{args}");
        assert_eq!(env(&spec, "MIRANTE_SESSION_ID"), Some("sess-42"));
    }
}
```

In `crates/mirante-daemon/src/hub/messages.rs` (`Hub::send`), right after `let task = st.resolve(&m.task)?;`, refuse what the typist would refuse instead of queueing it (a queued `!` line would come back at every turn end):
```rust
        if task.agent == AgentKind::Claude {
            // Refused now rather than queued: the typist would refuse it at every turn end
            // (a `!` bash-mode line, a multi-line slash command).
            crate::driver::paste::typed_message(&m.text).map_err(HubError::Invalid)?;
        }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 133 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon
git commit -F - <<'EOF'
feat(daemon): claude PTY driver: pinned session id, hook-gated ready, re-checked typing, exact trust option, tree stop

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

## Phase 2b-iii — Codex driver

### Task 16: Codex status engine and JSON-RPC connection

**Files:**
- Modify: `crates/mirante-daemon/src/driver/mod.rs` (two `pub mod` lines)
- Create: `crates/mirante-daemon/src/driver/codex_status.rs`
- Create: `crates/mirante-daemon/src/driver/codex_rpc.rs`

Codex status comes straight from app-server traffic (FINDINGS: no hook/transcript fusion needed), and only from **our** thread: every v2 notification carries a `threadId`, and a sub-agent's thread finishing its turn must not end ours. Approval requests are the exception: whoever asks, the user answers, so any pending approval means `NeedsYou`.

FINDINGS rule 8: `turn/completed` does not mean every command finished (run 2: `passo-2`'s `item/completed` came 1.3 s after it). Open `commandExecution` items are tracked by id; while any is open after a **completed** turn, the status stays `Working` with the detail "turn done, N command(s) still finishing". They settle on their own `item/completed`, on `thread/status/changed` → `idle`, or after `settle_after` (30 s): a command whose end Codex never reports must not pin the task at `Working` forever. An `interrupted` or `failed` turn abandons its open commands at once (the driver prints `[command abandoned]`). Item events never reopen a turn, and a late `turn/start` response for a turn that already completed does not reopen it either. Time is an argument (`apply(input, now)`) so the timeout is tested without sleeping.

Approvals are tracked by key with the turn that raised them. They end when Mirante answers, when the server says so (`serverRequest/resolved`, whose `requestId` may be `0`), when their turn completes, or when the process closes; the keys voided by the engine are handed to the driver (`take_resolved`) so it can tell the hub (`DriverEvent::PermissionResolved`). Errors become the status detail: an `error` notification (`willRetry` → "retrying: …", else "error: …", with `codexErrorInfo` such as `usageLimitExceeded`), a failed turn's `turn.error`, a thread `systemError`.

`RpcConn` speaks line-delimited JSON-RPC over any stream pair (the real app-server's stdio, or a socket pair in tests). Lines are read as bytes (`read_until`), so invalid UTF-8 or a log line on stdout is reported as `Incoming::Invalid`, never silently dropped. Responses are routed by id (ids start at 0). Errors are typed (`CallError::{Rpc, Timeout, Transport}`): Task 17 retries a steer only on a JSON-RPC rejection, never on a timeout. Writes go through a dedicated writer thread over a bounded queue (`WRITE_QUEUE` = 256 lines): `request`, `respond` and `notify` never block on the child's stdin, because the hub calls `answer_permission` while holding its lock; a full queue is an error, not a wait.

- [ ] **Step 1: Write the failing tests**

In `crates/mirante-daemon/src/driver/mod.rs`, add the two modules so the module list reads:
```rust
pub mod claude;
pub mod codex_rpc;
pub mod codex_status;
#[cfg(test)]
pub mod fake;
pub mod paste;
pub mod proctree;
pub mod pty;
pub mod real;
pub mod screen;
pub mod termq;
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

    fn engine() -> (CodexEngine, Instant) {
        let mut e = CodexEngine::new(Duration::from_secs(30));
        e.set_thread("th1");
        (e, Instant::now())
    }

    #[test]
    fn plain_turn() {
        let (mut e, t0) = engine();
        let started = json!({"threadId": "th1", "turn": {"id": "tu1"}});
        assert_eq!(e.apply(&n("turn/started", &started), t0).unwrap().to, Status::Working);
        assert_eq!(e.turn_id(), Some("tu1"));
        let done = json!({"threadId": "th1", "turn": {"id": "tu1", "status": "completed"}});
        assert_eq!(e.apply(&n("turn/completed", &done), t0).unwrap().to, Status::Idle);
        assert!(!e.turn_open());
        // A late `turn/start` response for the finished turn does not reopen it.
        assert_eq!(e.apply(&CodexInput::TurnAccepted("tu1".into()), t0), None);
        assert_eq!(e.apply(&CodexInput::TurnAccepted("tu2".into()), t0).unwrap().to, Status::Working);
        assert_eq!(e.turn_id(), Some("tu2"));
    }

    /// FINDINGS rule 8 (S1 Codex run 2): passo-2's item/completed came 1.3 s after
    /// turn/completed. The status must not say idle while it runs, and the late item
    /// must not reopen the turn.
    #[test]
    fn command_finishing_after_turn_completed_keeps_working_with_detail() {
        let (mut e, t0) = engine();
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu1"}})), t0);
        let cmd = json!({"threadId": "th1", "item": {"type": "commandExecution", "id": "c2", "command": "sleep 3"}});
        e.apply(&n("item/started", &cmd), t0);
        let done = json!({"threadId": "th1", "turn": {"id": "tu1", "status": "completed"}});
        assert_eq!(e.apply(&n("turn/completed", &done), t0), None);
        assert_eq!(e.status(), Status::Working);
        assert_eq!(e.detail().as_deref(), Some("turn done, 1 command(s) still finishing"));
        assert!(!e.turn_open());
        let t = e.apply(&n("item/completed", &cmd), t0).unwrap();
        assert_eq!((t.from, t.to, t.cause.as_str()), (Status::Working, Status::Idle, "codex:item/completed"));
        assert!(!e.turn_open());
        assert_eq!(e.detail(), None);
    }

    #[test]
    fn open_commands_settle_on_thread_idle_or_after_the_timeout() {
        let (mut e, t0) = engine();
        let cmd = json!({"threadId": "th1", "item": {"type": "commandExecution", "id": "c1"}});
        let done = json!({"threadId": "th1", "turn": {"id": "tu1", "status": "completed"}});
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu1"}})), t0);
        e.apply(&n("item/started", &cmd), t0);
        e.apply(&n("turn/completed", &done), t0);
        let idle = json!({"threadId": "th1", "status": {"type": "idle"}});
        let t = e.apply(&n("thread/status/changed", &idle), t0).unwrap();
        assert_eq!((t.to, t.cause.as_str()), (Status::Idle, "codex:thread/status/changed"));
        // Second turn: no idle notification, the timeout settles it.
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu2"}})), t0);
        e.apply(&n("item/started", &cmd), t0);
        e.apply(&n("turn/completed", &json!({"threadId": "th1", "turn": {"id": "tu2", "status": "completed"}})), t0);
        assert_eq!(e.apply(&CodexInput::Tick, t0 + Duration::from_secs(29)), None);
        assert_eq!(e.status(), Status::Working);
        let t = e.apply(&CodexInput::Tick, t0 + Duration::from_secs(30)).unwrap();
        assert_eq!((t.to, t.cause.as_str()), (Status::Idle, "mirante:settle_timeout"));
        assert_eq!(e.background_commands(), 0);
    }

    #[test]
    fn an_interrupted_or_failed_turn_abandons_its_open_commands() {
        let (mut e, t0) = engine();
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu1"}})), t0);
        e.apply(&n("item/started", &json!({"threadId": "th1", "item": {"type": "commandExecution", "id": "c1"}})), t0);
        let stop = json!({"threadId": "th1", "turn": {"id": "tu1", "status": "interrupted"}});
        assert_eq!(e.apply(&n("turn/completed", &stop), t0).unwrap().to, Status::Idle);
        assert_eq!((e.take_abandoned(), e.background_commands(), e.detail()), (1, 0, None));
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu2"}})), t0);
        let failed = json!({"threadId": "th1", "turn": {"id": "tu2", "status": "failed",
            "error": {"message": "quota", "codexErrorInfo": "usageLimitExceeded"}}});
        assert_eq!(e.apply(&n("turn/completed", &failed), t0).unwrap().to, Status::Idle);
        assert_eq!(e.detail().as_deref(), Some("turn failed: usageLimitExceeded: quota"));
    }

    #[test]
    fn another_threads_events_never_move_our_status() {
        let (mut e, t0) = engine();
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu1"}})), t0);
        let child_done = json!({"threadId": "child", "turn": {"id": "tc", "status": "completed"}});
        assert_eq!(e.apply(&n("turn/completed", &child_done), t0), None);
        let child_idle = json!({"threadId": "child", "status": {"type": "systemError"}});
        assert_eq!(e.apply(&n("thread/status/changed", &child_idle), t0), None);
        assert_eq!((e.status(), e.turn_id()), (Status::Working, Some("tu1")));
        // A sub-agent's approval still needs the user.
        let req = CodexInput::ApprovalRequested { key: "3".into(), turn: Some("tc".into()) };
        assert_eq!(e.apply(&req, t0).unwrap().to, Status::NeedsYou);
    }

    #[test]
    fn approvals_need_you_until_answered_resolved_or_voided_by_their_turn() {
        let (mut e, t0) = engine();
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu1"}})), t0);
        let ask = |k: &str| CodexInput::ApprovalRequested { key: k.into(), turn: Some("tu1".into()) };
        assert_eq!(e.apply(&ask("5"), t0).unwrap().to, Status::NeedsYou);
        assert_eq!(e.apply(&CodexInput::ApprovalAnswered("5".into()), t0).unwrap().to, Status::Working);
        // Request id 0 is a real id.
        e.apply(&ask("0"), t0);
        let resolved = json!({"threadId": "th1", "requestId": 0});
        let t = e.apply(&n("serverRequest/resolved", &resolved), t0).unwrap();
        assert_eq!((t.to, t.cause.as_str()), (Status::Working, "codex:request_resolved"));
        assert_eq!(e.apply(&n("serverRequest/resolved", &resolved), t0), None);
        e.apply(&ask("6"), t0);
        e.apply(&CodexInput::ApprovalRequested { key: "s1".into(), turn: Some("other".into()) }, t0);
        let done = json!({"threadId": "th1", "turn": {"id": "tu1", "status": "completed"}});
        e.apply(&n("turn/completed", &done), t0);
        assert_eq!(e.take_resolved(), vec!["6".to_string()]);
        assert_eq!(e.status(), Status::NeedsYou, "the sub-agent's request is still open");
        assert_eq!(e.apply(&CodexInput::ApprovalAnswered("s1".into()), t0).unwrap().to, Status::Idle);
    }

    #[test]
    fn errors_become_the_detail() {
        let (mut e, t0) = engine();
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu1"}})), t0);
        let retry = json!({"threadId": "th1", "turnId": "tu1", "willRetry": true,
            "error": {"message": "stream dropped", "codexErrorInfo": {"responseStreamDisconnected": {}}}});
        e.apply(&n("error", &retry), t0);
        assert_eq!(e.detail().as_deref(), Some("retrying: responseStreamDisconnected: stream dropped"));
        let fatal = json!({"threadId": "th1", "turnId": "tu1", "willRetry": false,
            "error": {"message": "limit", "codexErrorInfo": "usageLimitExceeded"}});
        e.apply(&n("error", &fatal), t0);
        assert_eq!(e.detail().as_deref(), Some("error: usageLimitExceeded: limit"));
        e.apply(&CodexInput::ApprovalRequested { key: "1".into(), turn: Some("tu1".into()) }, t0);
        let sys = json!({"threadId": "th1", "status": {"type": "systemError"}});
        assert_eq!(e.apply(&n("thread/status/changed", &sys), t0).unwrap().to, Status::Idle);
        assert_eq!(e.take_resolved(), vec!["1".to_string()]);
        assert!(e.detail().unwrap().contains("system error"));
        // The next turn clears it.
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu2"}})), t0);
        assert_eq!(e.detail(), None);
    }

    #[test]
    fn other_items_and_unknown_notifications_change_nothing() {
        let (mut e, t0) = engine();
        let msg = json!({"threadId": "th1", "item": {"type": "agentMessage", "id": "m"}});
        assert_eq!(e.apply(&n("item/completed", &msg), t0), None);
        assert_eq!(e.apply(&n("thread/tokenUsage/updated", &json!({"threadId": "th1"})), t0), None);
        assert_eq!(e.status(), Status::Idle);
    }

    #[test]
    fn closed_is_ended_final_and_voids_every_request() {
        let (mut e, t0) = engine();
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu1"}})), t0);
        e.apply(&CodexInput::ApprovalRequested { key: "0".into(), turn: None }, t0);
        assert_eq!(e.apply(&CodexInput::Closed, t0).unwrap().to, Status::Ended);
        assert_eq!(e.take_resolved(), vec!["0".to_string()]);
        assert_eq!(e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu2"}})), t0), None);
    }

    #[test]
    fn request_keys_keep_strings_verbatim() {
        assert_eq!(request_key(&json!(0)), "0");
        assert_eq!(request_key(&json!("e1")), "e1");
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
    use std::time::Instant;

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
    fn requests_get_their_own_response_including_id_zero_and_errors_are_typed() {
        let (conn, mut sr, mut sw, _rx) = pair();
        let c2 = conn.clone();
        let h = std::thread::spawn(move || c2.request("thread/start", json!({"cwd": "/w"}), Duration::from_secs(5)));
        let req = read(&mut sr);
        assert_eq!(req["id"], 0, "the first request id is 0");
        assert_eq!((req["method"].as_str(), req["params"]["cwd"].as_str()), (Some("thread/start"), Some("/w")));
        writeln!(sw, "{}", json!({"jsonrpc": "2.0", "id": 0, "result": {"thread": {"id": "th1"}}})).unwrap();
        assert_eq!(h.join().unwrap().unwrap()["thread"]["id"], "th1");
        let c3 = conn.clone();
        let h = std::thread::spawn(move || c3.request("turn/steer", json!({}), Duration::from_secs(5)));
        let req = read(&mut sr);
        let err =
            json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32600, "message": "expectedTurnId mismatch"}});
        writeln!(sw, "{err}").unwrap();
        assert_eq!(
            h.join().unwrap().unwrap_err(),
            CallError::Rpc { code: -32600, message: "expectedTurnId mismatch".into() }
        );
    }

    #[test]
    fn notifications_server_requests_and_bad_lines_reach_the_callback_and_eof_closes() {
        let (conn, mut sr, mut sw, rx) = pair();
        writeln!(sw, "{}", json!({"jsonrpc": "2.0", "method": "turn/started", "params": {"turn": {"id": "t"}}}))
            .unwrap();
        sw.write_all(b"WARN codex: not json \xff\r\n\n").unwrap();
        let req = json!({"jsonrpc": "2.0", "id": 0, "method": "item/commandExecution/requestApproval", "params": {}});
        writeln!(sw, "{req}").unwrap();
        assert_eq!(
            rx.recv().unwrap(),
            Incoming::Notification { method: "turn/started".into(), params: json!({"turn": {"id": "t"}}) }
        );
        assert_eq!(rx.recv().unwrap(), Incoming::Invalid("WARN codex: not json \u{fffd}".into()));
        let Incoming::Request { id, method, .. } = rx.recv().unwrap() else { panic!() };
        assert_eq!((method.as_str(), &id), ("item/commandExecution/requestApproval", &json!(0)));
        conn.respond(&id, json!({"decision": "accept"})).unwrap();
        assert_eq!(read(&mut sr), json!({"jsonrpc": "2.0", "id": 0, "result": {"decision": "accept"}}));
        drop(sw);
        drop(sr);
        conn.close();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Incoming::Closed);
        assert!(conn.notify("x", Value::Null).is_err(), "nothing is written after close");
    }

    #[test]
    fn a_request_without_response_times_out() {
        let (conn, _sr, _sw, _rx) = pair();
        let e = conn.request("initialize", json!({}), Duration::from_millis(50)).unwrap_err();
        assert!(matches!(e, CallError::Timeout(ref m) if m.contains("no response")), "{e:?}");
    }

    /// The server stops reading its stdin (stuck, or busy): our writes queue up to the
    /// bound and then fail fast, and notifications from it keep arriving meanwhile.
    #[test]
    fn a_server_that_stops_reading_never_blocks_writes_or_reads() {
        let (conn, _sr, mut sw, rx) = pair();
        let big = "x".repeat(8 * 1024);
        let started = Instant::now();
        let mut refused = 0;
        for _ in 0..(WRITE_QUEUE + 64) {
            if conn.notify("noise", json!({ "pad": big })).is_err() {
                refused += 1;
            }
        }
        assert!(refused > 0, "the bounded queue must refuse once full");
        assert!(started.elapsed() < Duration::from_secs(5), "writes never block: {:?}", started.elapsed());
        let e = conn.request("turn/steer", json!({}), Duration::from_secs(1)).unwrap_err();
        assert!(matches!(e, CallError::Transport(ref m) if m.contains("write queue full")), "{e:?}");
        writeln!(sw, "{}", json!({"jsonrpc": "2.0", "method": "item/completed", "params": {}})).unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Incoming::Notification { ref method, .. } if method == "item/completed"
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

Add above the test module in `crates/mirante-daemon/src/driver/codex_status.rs`:
```rust
//! Codex status, straight from app-server traffic (no hooks, no transcript):
//! - Only OUR thread moves the status: notifications carrying another `threadId` (a
//!   sub-agent's thread) are ignored here. Approval requests are the exception: whoever
//!   asks, the user must answer, so any approval means `NeedsYou`.
//! - `turn/started` (or the `turn/start` response, whichever comes first) opens the turn
//!   (`Working`); `turn/completed` closes it.
//! - FINDINGS rule 8: a `completed` turn does not mean every command finished. Open
//!   `commandExecution` items are tracked by id; while any is open after the turn the status
//!   stays `Working` with the detail "turn done, N command(s) still finishing". They settle
//!   on their own `item/completed`, on `thread/status/changed` → idle, or after
//!   `settle_after` (a command whose end Codex never reports must not pin the task). An
//!   `interrupted` / `failed` turn abandons its open commands at once. Item events never
//!   reopen a turn.
//! - Approval requests mean `NeedsYou` until answered, resolved by the server
//!   (`serverRequest/resolved`) or voided by the end of the turn that raised them; the keys
//!   voided here are handed to the driver ([`CodexEngine::take_resolved`]).
//! - Errors (`error` notifications, a failed turn's `turn.error`, a thread `systemError`)
//!   become the status detail.
//!
//! Time is passed in (`now`), so the settle timeout is tested without sleeping.
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use mirante_core::model::Status;
use mirante_core::status::Transition;
use serde_json::Value;

/// How long commands still open after a completed turn may keep the task `Working`.
pub const SETTLE_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub enum CodexInput<'a> {
    Notification {
        method: &'a str,
        params: &'a Value,
    },
    /// The `turn/start` response named the turn it started.
    TurnAccepted(String),
    /// A server→client approval request; `turn` is its `turnId` when it has one.
    ApprovalRequested {
        key: String,
        turn: Option<String>,
    },
    /// Mirante answered it.
    ApprovalAnswered(String),
    /// The server says it no longer waits (`serverRequest/resolved`).
    ApprovalResolved(String),
    /// Time passed (the driver ticks about once a second).
    Tick,
    /// The app-server process or its stdout ended.
    Closed,
}

/// A request key as Mirante stores it: a string id verbatim, a number in decimal.
pub fn request_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone)]
pub struct CodexEngine {
    status: Status,
    thread: Option<String>,
    turn: Option<String>,
    /// The last turn that completed (a late `turn/start` response must not reopen it).
    last_completed: Option<String>,
    open_commands: HashSet<String>,
    /// key -> the turn that raised it.
    approvals: HashMap<String, Option<String>>,
    /// When the turn completed while commands were still open.
    finished_at: Option<Instant>,
    settle_after: Duration,
    error: Option<String>,
    resolved: Vec<String>,
    abandoned: usize,
}

impl Default for CodexEngine {
    fn default() -> Self {
        CodexEngine::new(SETTLE_AFTER)
    }
}

fn error_text(err: &Value) -> Option<String> {
    let message = err["message"].as_str().filter(|m| !m.is_empty());
    let info = match &err["codexErrorInfo"] {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.keys().next().cloned(),
        _ => None,
    };
    match (info, message) {
        (Some(i), Some(m)) => Some(format!("{i}: {m}")),
        (Some(i), None) => Some(i),
        (None, Some(m)) => Some(m.to_string()),
        (None, None) => None,
    }
}

impl CodexEngine {
    pub fn new(settle_after: Duration) -> Self {
        CodexEngine {
            status: Status::Idle,
            thread: None,
            turn: None,
            last_completed: None,
            open_commands: HashSet::new(),
            approvals: HashMap::new(),
            finished_at: None,
            settle_after,
            error: None,
            resolved: Vec::new(),
            abandoned: 0,
        }
    }

    /// Our thread: notifications for any other thread are ignored.
    pub fn set_thread(&mut self, id: &str) {
        self.thread = Some(id.to_string());
    }

    pub fn status(&self) -> Status {
        self.status
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

    /// Approval keys voided by the last inputs (their turn ended, the process closed).
    pub fn take_resolved(&mut self) -> Vec<String> {
        std::mem::take(&mut self.resolved)
    }

    /// Commands abandoned by the last interrupted / failed turn.
    pub fn take_abandoned(&mut self) -> usize {
        std::mem::take(&mut self.abandoned)
    }

    /// Shown next to the status: why a finished turn is still `Working`, else the last error.
    pub fn detail(&self) -> Option<String> {
        let n = self.open_commands.len();
        if self.turn.is_none() && n > 0 {
            return Some(format!("turn done, {n} command(s) still finishing"));
        }
        self.error.clone()
    }

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
        let from = self.status;
        let to = self.target();
        if !from.can_transition_to(to) {
            return None;
        }
        self.status = to;
        Some(Transition { from, to, cause })
    }

    fn ours(&self, params: &Value) -> bool {
        match (params["threadId"].as_str(), &self.thread) {
            (Some(t), Some(mine)) => t == mine,
            _ => true,
        }
    }

    fn void_approvals(&mut self, turn: Option<&str>) {
        let gone: Vec<String> = self
            .approvals
            .iter()
            .filter(|(_, t)| turn.is_none() || t.is_none() || t.as_deref() == turn)
            .map(|(k, _)| k.clone())
            .collect();
        for k in gone {
            self.approvals.remove(&k);
            self.resolved.push(k);
        }
    }

    fn close_commands(&mut self) {
        self.open_commands.clear();
        self.finished_at = None;
    }

    pub fn apply(&mut self, input: &CodexInput<'_>, now: Instant) -> Option<Transition> {
        if self.status == Status::Ended {
            return None;
        }
        match input {
            CodexInput::Notification { method, params } => {
                if *method == "serverRequest/resolved" {
                    let key = request_key(&params["requestId"]);
                    return self.apply(&CodexInput::ApprovalResolved(key), now);
                }
                if !self.ours(params) {
                    return None;
                }
                let item = &params["item"];
                match *method {
                    "turn/started" => {
                        let id = params["turn"]["id"].as_str().unwrap_or_default().to_string();
                        self.turn = Some(id);
                        self.error = None;
                        self.finished_at = None;
                    }
                    "turn/completed" => {
                        let turn = &params["turn"];
                        let id = turn["id"].as_str().map(str::to_string).or_else(|| self.turn.clone());
                        self.void_approvals(id.as_deref());
                        self.last_completed = id;
                        self.turn = None;
                        match turn["status"].as_str() {
                            Some("interrupted" | "failed") => {
                                self.abandoned = self.open_commands.len();
                                self.close_commands();
                                if turn["status"] == "failed" {
                                    let why = error_text(&turn["error"]).unwrap_or_else(|| "no reason given".into());
                                    self.error = Some(format!("turn failed: {why}"));
                                }
                            }
                            _ if !self.open_commands.is_empty() => self.finished_at = Some(now),
                            _ => {}
                        }
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
                        if self.open_commands.is_empty() {
                            self.finished_at = None;
                        }
                    }
                    "thread/status/changed" => match params["status"]["type"].as_str() {
                        // The thread is idle: nothing of it is still running.
                        Some("idle") if self.turn.is_none() => self.close_commands(),
                        Some("systemError") => {
                            self.void_approvals(None);
                            self.turn = None;
                            self.close_commands();
                            self.error = Some("codex reported a system error on this thread".into());
                        }
                        _ => return None,
                    },
                    "error" => {
                        let why = error_text(&params["error"]).unwrap_or_else(|| "unknown error".into());
                        self.error = Some(if params["willRetry"] == true {
                            format!("retrying: {why}")
                        } else {
                            format!("error: {why}")
                        });
                    }
                    _ => return None,
                }
                self.settle(format!("codex:{method}"))
            }
            CodexInput::TurnAccepted(id) => {
                if self.turn.is_some() || self.last_completed.as_deref() == Some(id.as_str()) {
                    return None;
                }
                self.turn = Some(id.clone());
                self.error = None;
                self.finished_at = None;
                self.settle("codex:turn/start".into())
            }
            CodexInput::ApprovalRequested { key, turn } => {
                self.approvals.insert(key.clone(), turn.clone());
                self.settle("codex:approval_requested".into())
            }
            CodexInput::ApprovalAnswered(key) => {
                self.approvals.remove(key);
                self.settle("mirante:approval_answered".into())
            }
            CodexInput::ApprovalResolved(key) => {
                self.approvals.remove(key)?;
                self.settle("codex:request_resolved".into())
            }
            CodexInput::Tick => {
                let due = self.finished_at.is_some_and(|t| now.duration_since(t) >= self.settle_after);
                if !due {
                    return None;
                }
                self.close_commands();
                self.settle("mirante:settle_timeout".into())
            }
            CodexInput::Closed => {
                self.void_approvals(None);
                self.turn = None;
                self.close_commands();
                let from = self.status;
                self.status = Status::Ended;
                Some(Transition { from, to: Status::Ended, cause: "codex:closed".into() })
            }
        }
    }
}
```

Add above the test module in `crates/mirante-daemon/src/driver/codex_rpc.rs`:
```rust
//! JSON-RPC 2.0 client over `codex app-server`'s stdio (one JSON object per line).
//! - The reader thread reads lines as bytes (`read_until`), routes responses to waiting
//!   requests by id (0 included), and hands notifications, server→client requests
//!   (approvals) and lines that are not JSON to a callback. EOF is reported once.
//! - Writes go through a dedicated writer thread over a bounded queue: `request`,
//!   `respond` and `notify` never block on the child's stdin (the hub may hold its lock
//!   while answering an approval). A full queue is an error, never a wait.
use std::collections::HashMap;
use std::fmt;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

/// Lines queued for the writer thread.
pub const WRITE_QUEUE: usize = 256;
/// A line that is not JSON is reported cut to this many characters.
const INVALID_SHOWN: usize = 500;

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
    /// A line that is not a JSON object (logged, never silently dropped).
    Invalid(String),
    /// The server's stdout ended.
    Closed,
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    /// The server answered with a JSON-RPC error.
    Rpc { code: i64, message: String },
    /// No answer in time (the request may still have been applied).
    Timeout(String),
    /// Not sent, or the connection ended before the answer.
    Transport(String),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Rpc { code, message } => write!(f, "error {code}: {message}"),
            CallError::Timeout(m) | CallError::Transport(m) => write!(f, "{m}"),
        }
    }
}

type Waiter = mpsc::Sender<Result<Value, CallError>>;
type Pending = Arc<Mutex<HashMap<u64, Waiter>>>;

pub struct RpcConn {
    queue: Mutex<Option<mpsc::SyncSender<Vec<u8>>>>,
    next_id: AtomicU64,
    pending: Pending,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn route(line: &[u8], pending: &Pending, on_incoming: &impl Fn(Incoming)) {
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        let text: String = String::from_utf8_lossy(line).chars().take(INVALID_SHOWN).collect();
        on_incoming(Incoming::Invalid(text));
        return;
    };
    let method = v["method"].as_str().map(str::to_string);
    match (method, v.get("id")) {
        (Some(method), Some(id)) => {
            on_incoming(Incoming::Request { id: id.clone(), method, params: v["params"].clone() })
        }
        (Some(method), None) => on_incoming(Incoming::Notification { method, params: v["params"].clone() }),
        (None, Some(id)) => {
            let waiter = id.as_u64().and_then(|id| lock(pending).remove(&id));
            if let Some(tx) = waiter {
                let r = match v.get("error") {
                    Some(e) => Err(CallError::Rpc {
                        code: e["code"].as_i64().unwrap_or(0),
                        message: e["message"].as_str().unwrap_or("").to_string(),
                    }),
                    None => Ok(v["result"].clone()),
                };
                let _ = tx.send(r);
            }
        }
        (None, None) => {
            on_incoming(Incoming::Invalid(String::from_utf8_lossy(line).chars().take(INVALID_SHOWN).collect()))
        }
    }
}

impl RpcConn {
    pub fn start(
        mut reader: impl BufRead + Send + 'static,
        mut writer: impl Write + Send + 'static,
        on_incoming: impl Fn(Incoming) + Send + 'static,
    ) -> Arc<RpcConn> {
        let pending: Pending = Arc::default();
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(WRITE_QUEUE);
        std::thread::spawn(move || {
            for line in rx {
                if writer.write_all(&line).and_then(|()| writer.flush()).is_err() {
                    break;
                }
            }
            // Dropping the writer closes the server's stdin (app-server exits on EOF).
        });
        let conn =
            Arc::new(RpcConn { queue: Mutex::new(Some(tx)), next_id: AtomicU64::new(0), pending: pending.clone() });
        std::thread::spawn(move || {
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let trimmed = line.trim_ascii();
                if !trimmed.is_empty() {
                    route(trimmed, &pending, &on_incoming);
                }
            }
            // Every waiter gets a transport error at once (dropping its sender).
            lock(&pending).clear();
            on_incoming(Incoming::Closed);
        });
        conn
    }

    /// Queues one line for the writer thread; never blocks.
    fn write(&self, msg: &Value) -> io::Result<()> {
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        let q = lock(&self.queue);
        let tx = q.as_ref().ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "connection closed"))?;
        tx.try_send(line).map_err(|e| match e {
            mpsc::TrySendError::Full(_) => io::Error::new(
                io::ErrorKind::WouldBlock,
                "codex app-server is not reading its input (write queue full)",
            ),
            mpsc::TrySendError::Disconnected(_) => io::Error::new(io::ErrorKind::BrokenPipe, "connection closed"),
        })
    }

    /// Sends a request and waits up to `timeout` for its result.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, CallError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        lock(&self.pending).insert(id, tx);
        if let Err(e) = self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })) {
            lock(&self.pending).remove(&id);
            return Err(CallError::Transport(format!("{method}: {e}")));
        }
        match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&self.pending).remove(&id);
                Err(CallError::Timeout(format!("{method}: no response within {timeout:?}")))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(CallError::Transport(format!("{method}: the connection ended before the answer")))
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

    /// Stops writing: the writer thread drains what is queued, then closes the server's
    /// stdin (app-server exits on EOF).
    pub fn close(&self) {
        lock(&self.queue).take();
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 147 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

`cargo clippy -p mirante-daemon --all-targets` prints no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon/src/driver/mod.rs crates/mirante-daemon/src/driver/codex_status.rs crates/mirante-daemon/src/driver/codex_rpc.rs
git commit -F - <<'EOF'
feat(daemon): codex status engine (our thread only, late and abandoned commands, errors) and non-blocking JSON-RPC

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 17: Codex driver and its launch

**Files:**
- Modify: `crates/mirante-daemon/src/driver/mod.rs` (one `pub mod` line)
- Create: `crates/mirante-daemon/src/driver/codex.rs`
- Modify: `crates/mirante-daemon/src/driver/real.rs`

Method names and payloads are the ones verified in Week 1 plus the generated schema (`codex app-server generate-json-schema`, Codex 0.167): `initialize` (`clientInfo` required) / `initialized`, `thread/start` / `thread/resume` (`threadId`, `excludeTurns`), `turn/start` (`threadId`, `input`, `clientUserMessageId`, `approvalPolicy`, `approvalsReviewer`, `sandboxPolicy`), `turn/steer` (`threadId`, `expectedTurnId`, `input`, `clientUserMessageId`), `turn/interrupt` (`threadId`, `turnId`), `serverRequest/resolved` (`threadId`, `requestId`), `error` (`error.codexErrorInfo`, `willRetry`), `account/rateLimits/updated`, `thread/tokenUsage/updated`.

**The approval mode is "auto-edits" (founder's decision), stated plainly:** Mirante starts or resumes every Codex thread with `approvalPolicy: "on-request"`, `sandbox: "workspace-write"` and `approvalsReviewer: "user"`. Codex edits files and runs commands **inside the worktree without asking**; anything beyond the sandbox (network, writes outside the worktree, a command that wants to escape the sandbox) is an approval request that reaches **you**, never an automatic reviewer. This is less strict than Claude's `manual` mode, where every permissioned tool asks. The mode is shown in `mirante ls` (MODE column) and `mirante show` (Task 13) and is the first line of `mirante attach` on a Codex task. Mirante checks that Codex applied it: the `thread/start` / `thread/resume` response reports the effective `approvalPolicy`, `approvalsReviewer` and sandbox policy (`{"type": "workspaceWrite", …}`), and anything else fails the boot with an error naming what came back. Every `turn/start` sends the mode again (the sandbox policy exactly as the thread response reported it, so the user's own writable roots and network setting are kept). Never `"never"`, never `auto_review`.

- **Approvals** (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`, legacy `execCommandApproval` / `applyPatchApproval`) become permission cards keyed by the request id (`0` and string ids included) and are answered once, one-time only: `accept` / `decline`; for permissions the requested profile with `scope: "turn"` or an empty grant; legacy `approved` / `{"denied": {"rejection": …}}` (the schema's "deny and continue"; `abort` would stop the whole turn). Never `acceptForSession` / `approved_for_session`, never policy amendments. A file-change card shows the patch: the request's `itemId` names the `fileChange` item seen in `item/started`, summarised as `update src/a.rs (+2 -1)` (bounded). A request with `grantRoot` asks for write access for the rest of the session and its card says "session-wide grant". A request from another thread (a sub-agent) gets a card whose tool reads "(sub-agent)". The answer is queued to the writer thread, so answering never blocks the hub. MCP elicitations are declined; other server requests get a JSON-RPC "not supported" error.
- **Withdrawn requests.** `serverRequest/resolved`, the end of the request's turn, or the process exiting emits `DriverEvent::PermissionResolved { key }` (Task 13 settles that card).
- **Messages.** A worker thread sends `turn/steer` with the open turn's id, else `turn/start`, whose **response** names the new turn (the steer that follows does not wait for `turn/started`). A steer rejected by a JSON-RPC error while the engine shows the turn closed is retried once as `turn/start`; rejected while the turn is still open, or not sent at all, the message goes back to the hub's queue (`DeliveryAborted`); a timeout is never retried (it may have landed) and is reported as `send_unconfirmed`. Each message carries a `clientUserMessageId`; the `userMessage` item with that `clientId` confirms it (`message_confirmed` step). Rule 9: a steer lands at the next model step, a running command keeps running; "stop now" is `mirante interrupt` (`turn/interrupt`), which is a clear `NotNow` error when no turn of ours is running.
- **Resume** sends `thread/resume` with `excludeTurns: true`. A thread Codex no longer has (`-32603 "no rollout found …"`) is `DriverError::NativeGone`: Task 13 records `resume_failed` and the user decides (`mirante resume --fresh`); Mirante never starts a fresh thread on its own.
- **Status detail and usage.** Errors become the detail plus a `codex_error` / `turn_failed` / `codex_system_error` step. `account/rateLimits/updated` and `thread/tokenUsage/updated` become one `DriverEvent::Usage` line (`5h 42% · week 9% · 31k tokens`) shown in `mirante ls`.
- **Attach** is a read-only rendered stream (mode banner, turns, `$ command`, exit codes, `[command abandoned]`, agent messages, file changes, errors, approval notices) with 64 KiB of scrollback.
- **Process.** The real binary (`MIRANTE_CODEX_BIN` or PATH without shims), the Claude nested-session set plus every `CMUX_*` variable removed, `OPENAI_*` / `CODEX_*` never touched, `RUST_LOG=warn` unless set. `codex app-server` leads its own process group (`process_group(0)`); `stop` is `proctree::kill_tree` of that tree (SIGTERM, SIGKILL after 3 s, forks during the kill included). stderr goes to `~/.mirante/sessions/<task>/codex-stderr.log` (0600); when stdout closes the child is reaped and a `codex_exited` step carries the exit code and the last 4 KiB of that log before `Exited { code }`. Lines on stdout that are not JSON become a `codex_non_json` step. A failed handshake drops the driver, which ends the process tree.

- [ ] **Step 1: Write the failing tests**

In `crates/mirante-daemon/src/driver/mod.rs`, add `pub mod codex;` so the module list reads:
```rust
pub mod claude;
pub mod codex;
pub mod codex_rpc;
pub mod codex_status;
#[cfg(test)]
pub mod fake;
pub mod paste;
pub mod proctree;
pub mod pty;
pub mod real;
pub mod screen;
pub mod termq;
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

    /// What app-server answers to thread/start|resume when it applied the requested mode.
    fn thread_ok(id: &str) -> Value {
        json!({"thread": {"id": id}, "approvalPolicy": "on-request", "approvalsReviewer": "user",
            "sandbox": {"type": "workspaceWrite", "writableRoots": [], "networkAccess": false},
            "cwd": "/w/tree", "model": "m", "modelProvider": "p"})
    }

    fn timing() -> CodexTiming {
        CodexTiming {
            request: Duration::from_secs(2),
            settle_after: Duration::from_millis(300),
            tick: Duration::from_millis(20),
            stop_grace: Duration::from_secs(2),
        }
    }

    type Rx = mpsc::Receiver<(String, DriverEvent)>;

    /// Connects a driver to a fake app-server; `answer` replies to the thread request.
    fn connect_with(
        resume: Option<&str>,
        answer: impl FnOnce(&mut FakeServer, &Value) + Send + 'static,
    ) -> (Result<Arc<CodexDriver>, DriverError>, FakeServer, Rx, Value) {
        let (client, server) = UnixStream::pair().unwrap();
        let mut fake = FakeServer { r: BufReader::new(server.try_clone().unwrap()), w: server };
        let (tx, rx) = mpsc::channel();
        let resume = resume.map(str::to_string);
        let h = std::thread::spawn(move || {
            let reader = BufReader::new(client.try_clone().unwrap());
            CodexDriver::connect(reader, client, "/w/tree", resume.as_deref(), Sink::new("s1", tx), timing())
        });
        let init = fake.expect("initialize");
        assert_eq!(init["params"]["clientInfo"]["name"], "mirante");
        fake.reply(&init, json!({}));
        let initialized = fake.expect("initialized");
        assert!(initialized.get("params").is_none());
        let thread = fake.next();
        answer(&mut fake, &thread);
        (h.join().unwrap(), fake, rx, thread)
    }

    fn setup(resume: Option<&str>) -> (Arc<CodexDriver>, FakeServer, Rx, Value) {
        let (d, fake, rx, thread) = connect_with(resume, |f, t| f.reply(t, thread_ok("th-1")));
        (d.unwrap(), fake, rx, thread)
    }

    fn next_event(rx: &Rx, want: impl Fn(&DriverEvent) -> bool) -> DriverEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (_, ev) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("event");
            if want(&ev) {
                return ev;
            }
        }
    }

    fn status_to(to: Status) -> impl Fn(&DriverEvent) -> bool {
        move |e| matches!(e, DriverEvent::Status { to: t, .. } if *t == to)
    }

    #[test]
    fn handshake_starts_a_thread_in_auto_edits_mode_and_checks_it_was_applied() {
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
    fn a_thread_whose_effective_mode_differs_fails_the_boot() {
        let (d, _fake, _rx, _) = connect_with(None, |f, t| {
            let mut r = thread_ok("th-1");
            r["approvalPolicy"] = json!("never");
            f.reply(t, r)
        });
        let Err(DriverError::Failed(m)) = d else { panic!("{:?}", d.err()) };
        assert!(m.contains("auto-edits") && m.contains("\"never\""), "{m}");
        let (d, _fake, _rx, _) = connect_with(None, |f, t| {
            let mut r = thread_ok("th-1");
            r["approvalsReviewer"] = json!("auto_review");
            f.reply(t, r)
        });
        assert!(matches!(d, Err(DriverError::Failed(_))));
    }

    #[test]
    fn resume_uses_thread_resume_without_turns_and_a_vanished_thread_is_typed() {
        let (_d, _fake, _rx, thread) = setup(Some("th-old"));
        assert_eq!(thread["method"], "thread/resume");
        assert_eq!(thread["params"]["threadId"], "th-old");
        assert_eq!(thread["params"]["excludeTurns"], true);
        assert_eq!(thread["params"]["approvalPolicy"], "on-request");
        let (d, _fake, _rx, _) = connect_with(Some("th-gone"), |f, t| {
            f.send(json!({"jsonrpc": "2.0", "id": t["id"],
                "error": {"code": -32603, "message": "no rollout found for thread id th-gone"}}))
        });
        assert!(matches!(d, Err(DriverError::NativeGone(ref m)) if m.contains("th-gone")), "{:?}", d.err());
    }

    #[test]
    fn send_starts_a_turn_with_the_policy_then_steers_the_running_one() {
        let (d, mut fake, rx, _) = setup(None);
        d.send("five steps").unwrap();
        let start = fake.expect("turn/start");
        let p = &start["params"];
        assert_eq!((p["input"][0]["text"].as_str(), p["threadId"].as_str()), (Some("five steps"), Some("th-1")));
        assert_eq!((p["approvalPolicy"].as_str(), p["approvalsReviewer"].as_str()), (Some("on-request"), Some("user")));
        assert_eq!(p["sandboxPolicy"]["type"], "workspaceWrite");
        let client_id = p["clientUserMessageId"].as_str().unwrap().to_string();
        // The turn id comes from the response, before any notification.
        fake.reply(&start, json!({"turn": {"id": "tu-1", "items": [], "status": "inProgress"}}));
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Status { .. }));
        assert_eq!(
            ev,
            DriverEvent::Status {
                to: Status::Working,
                cause: "codex:turn/start".into(),
                turn_open: true,
                detail: None
            }
        );
        fake.notify("item/started", json!({"threadId": "th-1", "turnId": "tu-1",
            "item": {"type": "userMessage", "id": "u1", "clientId": client_id, "content": [{"type": "text", "text": "five steps"}]}}));
        let ok = next_event(&rx, |e| matches!(e, DriverEvent::Step(_)));
        assert_eq!(ok, DriverEvent::Step(json!({"type": "message_confirmed", "text": "five steps"})));
        d.send("stop, say BANANA").unwrap();
        let steer = fake.expect("turn/steer");
        assert_eq!(steer["params"]["expectedTurnId"], "tu-1");
        fake.reply(&steer, json!({"turnId": "tu-1"}));
        d.interrupt().unwrap();
        let int = fake.expect("turn/interrupt");
        assert_eq!(int["params"], json!({"threadId": "th-1", "turnId": "tu-1"}));
    }

    #[test]
    fn a_steer_rejected_after_its_turn_closed_falls_back_but_a_timeout_never_does() {
        let (d, mut fake, rx, _) = setup(None);
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
        next_event(&rx, status_to(Status::Working));
        d.send("more").unwrap();
        let steer = fake.expect("turn/steer");
        fake.notify("turn/completed", json!({"threadId": "th-1", "turn": {"id": "tu-1", "status": "completed"}}));
        next_event(&rx, status_to(Status::Idle));
        fake.send(json!({"jsonrpc": "2.0", "id": steer["id"], "error": {"code": -32600, "message": "no active turn"}}));
        let start = fake.expect("turn/start");
        assert_eq!(start["params"]["input"][0]["text"], "more");
        fake.reply(&start, json!({"turn": {"id": "tu-2"}}));
        next_event(&rx, status_to(Status::Working));
        // A steer rejected while the turn is still open goes back to the hub's queue.
        d.send("not now").unwrap();
        let steer = fake.expect("turn/steer");
        fake.send(json!({"jsonrpc": "2.0", "id": steer["id"], "error": {"code": -32600, "message": "activeTurnNotSteerable"}}));
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::DeliveryAborted { .. }));
        assert!(matches!(ev, DriverEvent::DeliveryAborted { ref text, .. } if text == "not now"));
        // No answer at all: reported, never re-sent as a new turn.
        d.send("slow").unwrap();
        fake.expect("turn/steer");
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Step(v) if v["type"] == "send_unconfirmed"));
        assert!(matches!(ev, DriverEvent::Step(v) if v["text"] == "slow"));
        fake.w.set_nonblocking(true).unwrap();
        fake.r.get_ref().set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut line = String::new();
        assert!(fake.r.read_line(&mut line).is_err() || line.is_empty(), "nothing more was sent: {line}");
    }

    #[test]
    fn approvals_become_cards_answered_once_and_are_withdrawn_when_resolved() {
        let (d, mut fake, rx, _) = setup(None);
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
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
        let ev = next_event(&rx, status_to(Status::NeedsYou));
        assert!(matches!(ev, DriverEvent::Status { turn_open: true, .. }));
        d.answer_permission("41", true).unwrap();
        assert_eq!(fake.next(), json!({"jsonrpc": "2.0", "id": 41, "result": {"decision": "accept"}}));
        assert!(matches!(d.answer_permission("41", true), Err(DriverError::NoDialog(_))));
        next_event(&rx, status_to(Status::Working));
        // Request id 0, then resolved by the server (e.g. answered elsewhere).
        fake.send(json!({"jsonrpc": "2.0", "id": 0, "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "i2", "startedAtMs": 1, "command": "ls"}}));
        next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { key, .. } if key == "0"));
        fake.notify("serverRequest/resolved", json!({"threadId": "th-1", "requestId": 0}));
        assert_eq!(
            next_event(&rx, |e| matches!(e, DriverEvent::PermissionResolved { .. })),
            DriverEvent::PermissionResolved { key: "0".into() }
        );
        next_event(&rx, status_to(Status::Working));
        assert!(matches!(d.answer_permission("0", false), Err(DriverError::NoDialog(_))));
        // A request still open when its turn ends is withdrawn too.
        fake.send(json!({"jsonrpc": "2.0", "id": "r7", "method": "item/fileChange/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "f1", "startedAtMs": 1}}));
        next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { .. }));
        fake.notify("turn/completed", json!({"threadId": "th-1", "turn": {"id": "tu-1", "status": "completed"}}));
        assert_eq!(
            next_event(&rx, |e| matches!(e, DriverEvent::PermissionResolved { .. })),
            DriverEvent::PermissionResolved { key: "r7".into() }
        );
        next_event(&rx, status_to(Status::Idle));
    }

    #[test]
    fn sub_agent_and_file_change_cards_say_what_they_are() {
        let (_d, mut fake, rx, _) = setup(None);
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
        fake.send(json!({"jsonrpc": "2.0", "id": 1, "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "th-child", "turnId": "tc", "itemId": "i", "startedAtMs": 1, "command": "curl x"}}));
        let card = next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { .. }));
        assert!(matches!(card, DriverEvent::PermissionRequested { ref tool, .. } if tool == "command (sub-agent)"));
        fake.notify("item/started", json!({"threadId": "th-1", "turnId": "tu-1", "startedAtMs": 1, "item": {
            "type": "fileChange", "id": "f1", "status": "inProgress",
            "changes": [{"path": "src/a.rs", "kind": {"type": "update"}, "diff": "--- a\n+++ b\n-old\n+new\n+more\n"}]}}));
        fake.send(json!({"jsonrpc": "2.0", "id": 2, "method": "item/fileChange/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "f1", "startedAtMs": 1, "reason": "outside"}}));
        let card = next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { .. }));
        assert_eq!(
            card,
            DriverEvent::PermissionRequested {
                key: "2".into(),
                tool: "file_change".into(),
                detail: "update src/a.rs (+2 -1) — outside".into()
            }
        );
        fake.send(json!({"jsonrpc": "2.0", "id": 3, "method": "item/fileChange/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "f2", "startedAtMs": 1, "grantRoot": "/Users/u"}}));
        let card = next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { .. }));
        assert!(matches!(card, DriverEvent::PermissionRequested { ref tool, ref detail, .. }
            if tool == "file_change (session-wide grant)" && detail.contains("/Users/u for the rest of the session")));
    }

    #[test]
    fn decisions_per_request_kind_are_one_time_only() {
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
        assert_eq!(
            approval_response("applyPatchApproval", &p, false),
            json!({"decision": {"denied": {"rejection": "denied by the user in Mirante"}}})
        );
        for m in APPROVAL_METHODS {
            for allow in [true, false] {
                let d = approval_response(m, &p, allow).to_string();
                assert!(
                    !d.contains("ForSession")
                        && !d.contains("for_session")
                        && !d.contains("mendment")
                        && !d.contains("abort"),
                    "{d}"
                );
            }
        }
        let items = HashMap::new();
        assert_eq!(approval_card("execCommandApproval", &json!({"command": ["git", "push"]}), &items).1, "git push");
        let legacy = json!({"fileChanges": {"b.rs": {"type": "add", "content": "a\nb\n"}}});
        assert_eq!(approval_card("applyPatchApproval", &legacy, &items).1, "add b.rs (+2 -0)");
    }

    #[test]
    fn usage_errors_and_settling_reach_the_hub() {
        let (_d, mut fake, rx, _) = setup(None);
        fake.notify(
            "account/rateLimits/updated",
            json!({"rateLimits": {
            "primary": {"usedPercent": 42, "windowDurationMins": 300},
            "secondary": {"usedPercent": 9, "windowDurationMins": 10080}}}),
        );
        assert_eq!(
            next_event(&rx, |e| matches!(e, DriverEvent::Usage(_))),
            DriverEvent::Usage("5h 42% · week 9%".into())
        );
        fake.notify(
            "thread/tokenUsage/updated",
            json!({"threadId": "th-1", "turnId": "t", "tokenUsage": {
            "total": {"totalTokens": 31_400}, "last": {"totalTokens": 10}}}),
        );
        assert_eq!(
            next_event(&rx, |e| matches!(e, DriverEvent::Usage(_))),
            DriverEvent::Usage("5h 42% · week 9% · 31k tokens".into())
        );
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
        fake.notify(
            "error",
            json!({"threadId": "th-1", "turnId": "tu-1", "willRetry": false,
            "error": {"message": "limit", "codexErrorInfo": "usageLimitExceeded"}}),
        );
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Step(v) if v["type"] == "codex_error"));
        assert!(matches!(ev, DriverEvent::Step(v) if v["error"] == "usageLimitExceeded: limit"));
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Status { detail: Some(_), .. }));
        assert!(
            matches!(ev, DriverEvent::Status { detail: Some(ref d), .. } if d == "error: usageLimitExceeded: limit")
        );
        // A command still open after the turn settles by itself after `settle_after`.
        fake.notify(
            "item/started",
            json!({"threadId": "th-1", "item": {"type": "commandExecution", "id": "c1", "command": "sleep 9"}}),
        );
        fake.notify("turn/completed", json!({"threadId": "th-1", "turn": {"id": "tu-1", "status": "completed"}}));
        let ev = next_event(&rx, status_to(Status::Idle));
        assert!(matches!(ev, DriverEvent::Status { ref cause, .. } if cause == "mirante:settle_timeout"));
    }

    #[test]
    fn interrupting_with_no_running_turn_is_a_clear_error() {
        let (d, mut fake, _rx, _) = setup(None);
        assert!(matches!(d.interrupt(), Err(DriverError::NotNow(ref m)) if m.contains("no running turn")));
        fake.notify("turn/started", json!({"threadId": "th-child", "turn": {"id": "tc"}}));
        std::thread::sleep(Duration::from_millis(100));
        assert!(matches!(d.interrupt(), Err(DriverError::NotNow(_))), "a sub-agent's turn is not ours");
    }

    #[test]
    fn other_threads_and_unsupported_requests() {
        let (_d, mut fake, rx, _) = setup(None);
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
        next_event(&rx, status_to(Status::Working));
        // A sub-agent finishing does not end our turn.
        fake.notify("turn/completed", json!({"threadId": "th-child", "turn": {"id": "tc", "status": "completed"}}));
        fake.send(json!({"jsonrpc": "2.0", "id": 5, "method": "item/tool/requestUserInput", "params": {}}));
        let r = fake.next();
        assert_eq!((r["id"].as_i64(), r["error"]["code"].as_i64()), (Some(5), Some(-32601)));
        fake.send(json!({"jsonrpc": "2.0", "id": "e1", "method": "mcpServer/elicitation/request", "params": {}}));
        assert_eq!(fake.next(), json!({"jsonrpc": "2.0", "id": "e1", "result": {"action": "decline"}}));
        fake.w.write_all(b"panic: not json\n").unwrap();
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Step(v) if v["type"] == "codex_non_json"));
        assert!(matches!(ev, DriverEvent::Step(v) if v["line"] == "panic: not json"));
        while let Ok((_, e)) = rx.try_recv() {
            assert!(!matches!(e, DriverEvent::Status { to: Status::Idle, .. }), "{e:?}");
        }
    }

    #[test]
    fn the_stream_is_readable_starts_with_the_mode_and_reports_abandoned_commands() {
        let (d, mut fake, rx, _) = setup(None);
        let a = d.attach(24, 80).unwrap();
        assert!(!a.input);
        assert!(String::from_utf8_lossy(&a.initial).starts_with("[mirante] codex, auto-edits mode"));
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
        fake.notify(
            "item/started",
            json!({"threadId": "th-1", "item": {"type": "commandExecution", "id": "c2", "command": "sleep 3"}}),
        );
        fake.notify(
            "item/completed",
            json!({"threadId": "th-1", "item": {"type": "agentMessage", "id": "m", "text": "BANANA"}}),
        );
        fake.notify("turn/completed", json!({"threadId": "th-1", "turn": {"id": "tu-1", "status": "interrupted"}}));
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Status { turn_open: false, .. }));
        assert!(matches!(ev, DriverEvent::Status { to: Status::Idle, detail: None, .. }), "{ev:?}");
        let mut seen = String::new();
        while !seen.contains("[command abandoned]") {
            seen.push_str(&String::from_utf8_lossy(&a.output.recv_timeout(Duration::from_secs(5)).unwrap()));
        }
        assert!(seen.contains("$ sleep 3") && seen.contains("BANANA") && seen.contains("── turn interrupted ──"));
        assert!(d.screen_tail().ends_with("[command abandoned]"));
        assert!(matches!(d.input(a.id, b"x"), Err(DriverError::Unsupported(_))));
        assert!(matches!(d.trust(true), Err(DriverError::Unsupported(_))));
    }

    #[test]
    fn server_eof_withdraws_requests_and_is_an_exit() {
        let (_d, mut fake, rx, _) = setup(None);
        fake.send(json!({"jsonrpc": "2.0", "id": 0, "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "i", "startedAtMs": 1, "command": "ls"}}));
        next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { .. }));
        drop(fake);
        assert_eq!(
            next_event(&rx, |e| matches!(e, DriverEvent::PermissionResolved { .. })),
            DriverEvent::PermissionResolved { key: "0".into() }
        );
        assert_eq!(next_event(&rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { code: None });
    }

    /// A fake `codex app-server`: answers the handshake (request ids 0 and 1), then runs
    /// `rest`. It writes one line to stderr first.
    fn fake_app_server(dir: &Path, rest: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("codex");
        let script = format!(
            "#!/bin/sh\necho 'fake codex: starting' >&2\n\
             read l; printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{{}}}}'\n\
             read l\n\
             read l; printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"thread\":{{\"id\":\"th-1\"}},\"approvalPolicy\":\"on-request\",\"approvalsReviewer\":\"user\",\"sandbox\":{{\"type\":\"workspaceWrite\"}}}}}}'\n\
             {rest}\n"
        );
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn spawn_fake(dir: &Path, rest: &str) -> (Arc<CodexDriver>, Rx) {
        let bin = fake_app_server(dir, rest);
        let (tx, rx) = mpsc::channel();
        let log = dir.join("codex-stderr.log");
        let cwd = dir.to_string_lossy().into_owned();
        let d = CodexDriver::spawn(&bin, &cwd, None, &[], &log, Sink::new("s1", tx), timing()).unwrap();
        (d, rx)
    }

    #[test]
    fn stop_ends_the_app_server_and_everything_it_started() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let pids = dir.path().join("pids");
        let rest = format!(
            "sleep 60 & echo $! > {p}\nperl -e 'setpgrp(0,0); sleep 60' & echo $! >> {p}\nwhile :; do sleep 0.1; done",
            p = pids.display()
        );
        let (d, rx) = spawn_fake(dir.path(), &rest);
        let root = d.root().expect("spawned");
        assert_eq!(proctree::pgid(root.pid), Some(root.pid), "app-server leads its own process group");
        let deadline = Instant::now() + Duration::from_secs(5);
        let children: Vec<i32> = loop {
            let text = std::fs::read_to_string(&pids).unwrap_or_default();
            let v: Vec<i32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
            if v.len() == 2 && v.iter().all(|p| proctree::proc_of(*p).is_some()) {
                break v;
            }
            assert!(Instant::now() < deadline, "children never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        d.stop();
        assert!(!proctree::alive(root));
        for pid in children {
            assert!(proctree::proc_of(pid).is_none(), "{pid} survived the stop");
        }
        next_event(&rx, |e| matches!(e, DriverEvent::Exited { .. }));
    }

    #[test]
    fn an_exit_is_reported_with_its_code_and_the_stderr_tail() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let (_d, rx) = spawn_fake(dir.path(), "echo 'fatal: model not available' >&2\nexit 3");
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Step(v) if v["type"] == "codex_exited"));
        let DriverEvent::Step(v) = ev else { unreachable!() };
        assert_eq!(v["code"], 3);
        let tail = v["stderr_tail"].as_str().unwrap();
        assert!(tail.contains("fake codex: starting") && tail.contains("fatal: model not available"), "{tail}");
        assert_eq!(next_event(&rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { code: Some(3) });
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
//! Codex driver: `codex app-server` JSON-RPC over stdio (FINDINGS Task 10 names, Codex 0.167
//! schema).
//!
//! **Approval mode "auto-edits"** (the founder's decision): every thread is started or
//! resumed with `approvalPolicy: "on-request"`, `sandbox: "workspace-write"` and
//! `approvalsReviewer: "user"`. In plain words: Codex edits files and runs commands inside
//! the worktree without asking (the sandbox allows writes there and nothing else); anything
//! beyond the sandbox (network, writes outside the worktree, an unsandboxed command) is an
//! approval request that reaches the user, never an automatic reviewer. The effective
//! policy in the `thread/start` / `thread/resume` response is checked: if Codex did not
//! apply exactly this mode the boot fails. Each `turn/start` sends the policy again. Never
//! `"never"`, never `auto_review`, never a session-wide or policy-amending decision.
//!
//! - Launch hygiene: the real binary (`MIRANTE_CODEX_BIN`, else PATH without shim dirs),
//!   every Claude nested-session and `CMUX_*` variable removed, auth untouched,
//!   `RUST_LOG=warn` unless set; its own process group, so `stop` ends the whole tree.
//! - Only our thread's notifications move the status and fill the stream; a sub-agent's
//!   approval requests become cards labelled "(sub-agent)".
//! - Messages: `turn/steer` (with `expectedTurnId`) while a turn runs, else `turn/start`
//!   (whose response names the new turn). A steer rejected because the turn just closed is
//!   retried as `turn/start`; a timeout is never retried (it may have landed). Each message
//!   carries a `clientUserMessageId` and is confirmed when its `userMessage` item shows up.
//!   A steer lands at the next model step; a running command keeps running (rule 9), so
//!   "stop now" is `turn/interrupt`.
//! - Approvals arrive as server→client requests and are answered once, one-time only.
//!   `serverRequest/resolved`, the end of their turn, or the process exiting withdraws them
//!   (`DriverEvent::PermissionResolved`).
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use mirante_core::claude_launch::{env_to_scrub, KNOWN_SHIM_DIRS};
use mirante_core::model::Status;
use mirante_ipc::types::BootState;
use serde_json::{json, Value};

use super::codex_rpc::{CallError, Incoming, RpcConn};
use super::codex_status::{request_key, CodexEngine, CodexInput, SETTLE_AFTER};
use super::proctree::{self, Kill, Proc};
use super::{Attachment, Driver, DriverError, DriverEvent, Fanout, Sink};

pub const APPROVAL_METHODS: [&str; 5] = [
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "execCommandApproval",
    "applyPatchApproval",
];

pub const APPROVAL_POLICY: &str = "on-request";
pub const SANDBOX: &str = "workspace-write";
/// How the effective sandbox policy names [`SANDBOX`] in responses.
pub const SANDBOX_POLICY_TYPE: &str = "workspaceWrite";
pub const REVIEWER: &str = "user";

/// First line of every attach stream.
pub const MODE_BANNER: &str = "[mirante] codex, auto-edits mode: edits and commands inside the worktree run without \
     asking; anything beyond the sandbox asks you (approvalPolicy on-request, sandbox workspace-write, approvalsReviewer user)";

const SCROLLBACK: usize = 64 * 1024;
/// How much of the stderr log is reported when the process exits.
pub const STDERR_TAIL: u64 = 4096;
/// File-change items remembered for approval cards.
const FILE_ITEMS: usize = 64;
/// Longest card detail for a patch.
const PATCH_DETAIL: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexTiming {
    /// How long a JSON-RPC request waits for its answer.
    pub request: Duration,
    /// Commands still open after a completed turn settle after this long.
    pub settle_after: Duration,
    /// Clock for the settle timeout.
    pub tick: Duration,
    /// SIGTERM → SIGKILL grace when stopping.
    pub stop_grace: Duration,
}

impl Default for CodexTiming {
    fn default() -> Self {
        CodexTiming {
            request: Duration::from_secs(30),
            settle_after: SETTLE_AFTER,
            tick: Duration::from_secs(1),
            stop_grace: Duration::from_secs(3),
        }
    }
}

/// `thread/start` (no `resume`) or `thread/resume` parameters, in auto-edits mode.
pub fn thread_params(cwd: &str, resume: Option<&str>) -> Value {
    let mut p = json!({
        "cwd": cwd,
        "approvalPolicy": APPROVAL_POLICY,
        "sandbox": SANDBOX,
        "approvalsReviewer": REVIEWER,
    });
    if let Some(id) = resume {
        p["threadId"] = json!(id);
        p["excludeTurns"] = json!(true);
    }
    p
}

/// The response must show the mode Mirante asked for; anything else fails the boot.
pub fn check_mode(response: &Value) -> Result<(), String> {
    let policy = &response["approvalPolicy"];
    let reviewer = &response["approvalsReviewer"];
    let sandbox = &response["sandbox"]["type"];
    if policy == APPROVAL_POLICY && reviewer == REVIEWER && sandbox == SANDBOX_POLICY_TYPE {
        return Ok(());
    }
    Err(format!(
        "codex app-server did not apply Mirante's auto-edits mode (wanted approvalPolicy {APPROVAL_POLICY}, \
         sandbox {SANDBOX_POLICY_TYPE}, approvalsReviewer {REVIEWER}; got approvalPolicy {policy}, sandbox {sandbox}, \
         approvalsReviewer {reviewer}); refusing to run it"
    ))
}

/// `turn/start` parameters: the input plus the mode again (the sandbox policy exactly as
/// the thread response reported it, so the user's writable roots / network setting stay).
pub fn turn_start_params(thread: &str, text: &str, client_id: &str, sandbox_policy: &Value) -> Value {
    json!({
        "threadId": thread,
        "input": [{ "type": "text", "text": text }],
        "clientUserMessageId": client_id,
        "approvalPolicy": APPROVAL_POLICY,
        "approvalsReviewer": REVIEWER,
        "sandboxPolicy": sandbox_policy,
    })
}

/// The one-time answer to an approval request (schema: Codex 0.167 app-server). Legacy
/// requests take a `ReviewDecision`, whose "deny and continue" is `{"denied": …}` (`abort`
/// would stop the whole turn).
pub fn approval_response(method: &str, params: &Value, allow: bool) -> Value {
    match method {
        "item/permissions/requestApproval" if allow => json!({ "permissions": params["permissions"], "scope": "turn" }),
        "item/permissions/requestApproval" => json!({ "permissions": {}, "scope": "turn" }),
        "execCommandApproval" | "applyPatchApproval" if allow => json!({ "decision": "approved" }),
        "execCommandApproval" | "applyPatchApproval" => {
            json!({ "decision": { "denied": { "rejection": "denied by the user in Mirante" } } })
        }
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

/// `+added -removed` of a unified diff (header lines not counted).
fn diff_counts(diff: &str) -> (usize, usize) {
    let added = diff.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).count();
    let removed = diff.lines().filter(|l| l.starts_with('-') && !l.starts_with("---")).count();
    (added, removed)
}

fn bounded(mut s: String, max: usize) -> String {
    if s.chars().count() > max {
        s = s.chars().take(max.saturating_sub(1)).collect();
        s.push('…');
    }
    s
}

/// "update src/a.rs (+3 -1), add b.rs (+10 -0)" for a v2 `fileChange` item's `changes`.
fn changes_summary(changes: &Value) -> Option<String> {
    let parts: Vec<String> = changes
        .as_array()?
        .iter()
        .map(|c| {
            let (a, r) = diff_counts(c["diff"].as_str().unwrap_or(""));
            let kind = c["kind"]["type"].as_str().unwrap_or("change");
            format!("{kind} {} (+{a} -{r})", c["path"].as_str().unwrap_or("?"))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The same for a legacy `applyPatchApproval`'s `fileChanges` map.
fn legacy_changes_summary(changes: &Value) -> Option<String> {
    let parts: Vec<String> = changes
        .as_object()?
        .iter()
        .map(|(path, c)| {
            let kind = c["type"].as_str().unwrap_or("change");
            let (a, r) = match kind {
                "add" => (c["content"].as_str().unwrap_or("").lines().count(), 0),
                "delete" => (0, c["content"].as_str().unwrap_or("").lines().count()),
                _ => diff_counts(c["unified_diff"].as_str().unwrap_or("")),
            };
            format!("{kind} {path} (+{a} -{r})")
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// (tool, detail) for the permission card. A file change shows its patch (from the
/// `fileChange` item the request's `itemId` names); a `grantRoot` asks for write access for
/// the rest of the session and says so.
pub fn approval_card(method: &str, params: &Value, file_items: &HashMap<String, Value>) -> (String, String) {
    let reason = text_of(&params["reason"]);
    let (tool, detail) = match method {
        "item/commandExecution/requestApproval" | "execCommandApproval" => ("command", text_of(&params["command"])),
        "item/fileChange/requestApproval" | "applyPatchApproval" if params["grantRoot"].is_string() => {
            let root = params["grantRoot"].as_str().unwrap_or("");
            ("file_change (session-wide grant)", Some(format!("write access under {root} for the rest of the session")))
        }
        "item/fileChange/requestApproval" => {
            let item = params["itemId"].as_str().and_then(|id| file_items.get(id));
            ("file_change", item.and_then(|i| changes_summary(&i["changes"])).map(|s| bounded(s, PATCH_DETAIL)))
        }
        "applyPatchApproval" => {
            ("file_change", legacy_changes_summary(&params["fileChanges"]).map(|s| bounded(s, PATCH_DETAIL)))
        }
        _ => ("permissions", Some(params["permissions"].to_string()).filter(|s| s != "null")),
    };
    let detail = match (detail, reason) {
        (Some(d), Some(r)) if tool.starts_with("file_change") => format!("{d} — {r}"),
        (Some(d), _) => d,
        (None, Some(r)) => r,
        (None, None) => "(no detail given)".into(),
    };
    (tool.to_string(), detail)
}

fn window_label(mins: Option<i64>) -> String {
    match mins {
        Some(10080) => "week".into(),
        Some(m) if m >= 1440 && m % 1440 == 0 => format!("{}d", m / 1440),
        Some(m) if m >= 60 && m % 60 == 0 => format!("{}h", m / 60),
        Some(m) => format!("{m}m"),
        None => "limit".into(),
    }
}

fn tokens_label(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{}k", (n + 500) / 1_000),
        n => n.to_string(),
    }
}

/// One line for `mirante ls`: "5h 42% · week 9% · 31k tokens".
pub fn usage_line(rate_limits: Option<&Value>, token_usage: Option<&Value>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(r) = rate_limits {
        for w in [&r["primary"], &r["secondary"]] {
            if let Some(p) = w["usedPercent"].as_i64() {
                parts.push(format!("{} {p}%", window_label(w["windowDurationMins"].as_i64())));
            }
        }
    }
    if let Some(total) = token_usage.and_then(|t| t["total"]["totalTokens"].as_i64()) {
        parts.push(format!("{} tokens", tokens_label(total)));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn error_text(err: &Value) -> String {
    let info = match &err["codexErrorInfo"] {
        Value::String(s) => format!("{s}: "),
        Value::Object(o) => o.keys().next().map(|k| format!("{k}: ")).unwrap_or_default(),
        _ => String::new(),
    };
    format!("{info}{}", err["message"].as_str().unwrap_or("unknown error"))
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
        ("item/completed", Some("fileChange")) => Some(format!("  [{}]\r\n", changes_summary(&item["changes"])?)),
        ("error", _) => Some(format!(
            "[error] {}{}\r\n",
            error_text(&params["error"]),
            if params["willRetry"] == true { " (retrying)" } else { "" }
        )),
        ("thread/status/changed", _) if params["status"]["type"] == "systemError" => {
            Some("[codex reported a system error on this thread]\r\n".into())
        }
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
        ("error", _) => Some(json!({
            "type": "codex_error",
            "error": error_text(&params["error"]),
            "will_retry": params["willRetry"],
        })),
        ("turn/completed", _) if params["turn"]["status"] == "failed" => {
            Some(json!({ "type": "turn_failed", "error": error_text(&params["turn"]["error"]) }))
        }
        ("thread/status/changed", _) if params["status"]["type"] == "systemError" => {
            Some(json!({ "type": "codex_system_error" }))
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

/// The last `max` bytes of `path` (lossy UTF-8), empty when unreadable.
pub fn file_tail(path: &Path, max: u64) -> String {
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(max)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Last `n` non-blank lines.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The spawned app-server: reaped once, by whoever gets there first (the reader at EOF or
/// `stop`).
struct Process {
    child: Mutex<Option<Child>>,
    root: Option<Proc>,
    stderr_log: Option<PathBuf>,
    code: Mutex<Option<i32>>,
}

impl Process {
    /// Waits up to `wait` for the exit, then ends what is left of the tree; records the code.
    fn reap(&self, wait: Duration) -> Option<i32> {
        if let Some(mut child) = lock(&self.child).take() {
            let deadline = Instant::now() + wait;
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(20));
            }
            if let Some(root) = self.root {
                proctree::kill_tree(root, Duration::from_millis(500), &Kill);
            }
            if let Ok(status) = child.wait() {
                *lock(&self.code) = status.code();
            }
        }
        *lock(&self.code)
    }

    fn stderr_tail(&self) -> String {
        self.stderr_log.as_deref().map(|p| file_tail(p, STDERR_TAIL)).unwrap_or_default()
    }
}

struct Shared {
    engine: CodexEngine,
    thread_id: Option<String>,
    /// key -> (request id, method, params)
    approvals: HashMap<String, (Value, String, Value)>,
    /// `fileChange` items by id (bounded), for approval cards.
    file_items: HashMap<String, Value>,
    file_order: VecDeque<String>,
    fanout: Fanout,
    scrollback: Vec<u8>,
    /// Last reported (status, turn open, detail), to emit only changes.
    reported: (Status, bool, Option<String>),
    /// The effective sandbox policy from the thread response (sent again on each turn).
    sandbox_policy: Value,
    /// clientUserMessageId -> text, until its `userMessage` item shows up.
    unconfirmed: HashMap<String, String>,
    rate_limits: Option<Value>,
    token_usage: Option<Value>,
    closed: bool,
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

    fn remember_file_item(&mut self, item: &Value) {
        let Some(id) = item["id"].as_str() else { return };
        if self.file_items.insert(id.to_string(), item.clone()).is_none() {
            self.file_order.push_back(id.to_string());
        }
        while self.file_order.len() > FILE_ITEMS {
            if let Some(old) = self.file_order.pop_front() {
                self.file_items.remove(&old);
            }
        }
    }

    /// After the engine moved: withdrawn requests, abandoned commands, the status change.
    fn settle(&mut self, cause: String, out: &mut Vec<DriverEvent>) {
        for key in self.engine.take_resolved() {
            if self.approvals.remove(&key).is_some() {
                self.output(&format!("[request {key} withdrawn]\r\n"));
                out.push(DriverEvent::PermissionResolved { key });
            }
        }
        for _ in 0..self.engine.take_abandoned() {
            self.output("  [command abandoned]\r\n");
        }
        let now = (self.engine.status(), self.engine.turn_open(), self.engine.detail());
        if now != self.reported {
            self.reported = now.clone();
            out.push(DriverEvent::Status { to: now.0, cause, turn_open: now.1, detail: now.2 });
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

enum Work {
    Send(String),
    Interrupt(String),
}

pub struct CodexDriver {
    conn: Arc<RpcConn>,
    shared: Arc<Mutex<Shared>>,
    sink: Sink,
    work: Mutex<Option<mpsc::Sender<Work>>>,
    process: Option<Arc<Process>>,
    stopped: Arc<AtomicBool>,
    timing: CodexTiming,
}

struct Ctx {
    shared: Arc<Mutex<Shared>>,
    sink: Sink,
    conn: OnceLock<Arc<RpcConn>>,
    process: Option<Arc<Process>>,
}

fn on_incoming(ctx: &Ctx, incoming: Incoming) {
    let mut out = Vec::new();
    match incoming {
        Incoming::Notification { method, params } => {
            let mut s = lock(&ctx.shared);
            let item = &params["item"];
            let ours = params["threadId"].as_str().is_none_or(|t| Some(t) == s.thread_id.as_deref());
            if method == "item/started" && item["type"] == "fileChange" {
                s.remember_file_item(item);
            }
            match method.as_str() {
                "account/rateLimits/updated" => s.rate_limits = Some(params["rateLimits"].clone()),
                "thread/tokenUsage/updated" if ours => s.token_usage = Some(params["tokenUsage"].clone()),
                "serverRequest/resolved" => {
                    let key = request_key(&params["requestId"]);
                    if s.approvals.remove(&key).is_some() {
                        s.output(&format!("[request {key} resolved by codex]\r\n"));
                        out.push(DriverEvent::PermissionResolved { key });
                    }
                }
                _ => {}
            }
            if matches!(method.as_str(), "account/rateLimits/updated" | "thread/tokenUsage/updated") {
                if let Some(line) = usage_line(s.rate_limits.as_ref(), s.token_usage.as_ref()) {
                    out.push(DriverEvent::Usage(line));
                }
            }
            if ours && item["type"] == "userMessage" {
                if let Some(text) = item["clientId"].as_str().and_then(|id| s.unconfirmed.remove(id)) {
                    out.push(DriverEvent::Step(json!({ "type": "message_confirmed", "text": text })));
                }
            }
            let t = s.engine.apply(&CodexInput::Notification { method: &method, params: &params }, Instant::now());
            if ours {
                if let Some(text) = render(&method, &params) {
                    s.output(&text);
                }
                if let Some(st) = step(&method, &params) {
                    out.push(DriverEvent::Step(st));
                }
            }
            s.settle(t.map_or_else(|| format!("codex:{method}"), |t| t.cause), &mut out);
        }
        Incoming::Request { id, method, params } if APPROVAL_METHODS.contains(&method.as_str()) => {
            let key = request_key(&id);
            let mut s = lock(&ctx.shared);
            let thread = params["threadId"].as_str().or(params["conversationId"].as_str());
            let sub_agent = thread.is_some_and(|t| Some(t) != s.thread_id.as_deref());
            let (mut tool, detail) = approval_card(&method, &params, &s.file_items);
            if sub_agent {
                tool.push_str(" (sub-agent)");
            }
            s.output(&format!("[needs you] {tool}: {detail}  (mirante perms)\r\n"));
            let turn = params["turnId"].as_str().map(str::to_string);
            s.approvals.insert(key.clone(), (id, method, params));
            let t = s.engine.apply(&CodexInput::ApprovalRequested { key: key.clone(), turn }, Instant::now());
            out.push(DriverEvent::PermissionRequested { key, tool, detail });
            s.settle(t.map_or_else(|| "codex:approval_requested".into(), |t| t.cause), &mut out);
        }
        Incoming::Request { id, method, .. } => {
            if let Some(conn) = ctx.conn.get() {
                // MCP elicitations are declined (never accepted on the user's behalf); other
                // server requests (user input, tool calls, auth refresh) are not supported.
                let _ = if method == "mcpServer/elicitation/request" {
                    conn.respond(&id, json!({ "action": "decline" }))
                } else {
                    conn.respond_error(&id, -32601, "not supported by Mirante")
                };
            }
            out.push(DriverEvent::Step(json!({ "type": "server_request_declined", "method": method })));
        }
        Incoming::Invalid(line) => out.push(DriverEvent::Step(json!({ "type": "codex_non_json", "line": line }))),
        Incoming::Closed => {
            // Reap first (outside the lock): the exit code goes into the report.
            let code = ctx.process.as_ref().and_then(|p| p.reap(Duration::from_secs(2)));
            let mut s = lock(&ctx.shared);
            s.closed = true;
            s.engine.apply(&CodexInput::Closed, Instant::now());
            s.settle("codex:closed".into(), &mut out);
            out.retain(|e| !matches!(e, DriverEvent::Status { .. }));
            s.fanout = Fanout::default();
            if let Some(p) = &ctx.process {
                let tail = p.stderr_tail();
                out.push(DriverEvent::Step(json!({ "type": "codex_exited", "code": code, "stderr_tail": tail })));
            }
            out.push(DriverEvent::Exited { code });
        }
    }
    for e in out {
        ctx.sink.emit(e);
    }
}

static MESSAGES: AtomicU64 = AtomicU64::new(0);

fn client_message_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("mirante-{nanos:x}-{}", MESSAGES.fetch_add(1, Ordering::SeqCst))
}

/// Delivers one message: steer the open turn, else start one. Only a JSON-RPC rejection of
/// a steer whose turn has meanwhile closed is retried as `turn/start`; a timeout is never
/// retried (the message may have landed). A message that was certainly not delivered goes
/// back to the hub's queue (`DeliveryAborted`).
fn send_now(conn: &RpcConn, shared: &Mutex<Shared>, sink: &Sink, text: &str, timeout: Duration) {
    let client_id = client_message_id();
    let (thread, turn, sandbox) = {
        let mut s = lock(shared);
        s.unconfirmed.insert(client_id.clone(), text.to_string());
        (s.thread_id.clone().unwrap_or_default(), s.engine.turn_id().map(str::to_string), s.sandbox_policy.clone())
    };
    let start = || -> Result<(), CallError> {
        let r = conn.request("turn/start", turn_start_params(&thread, text, &client_id, &sandbox), timeout)?;
        if let Some(id) = r["turn"]["id"].as_str() {
            let mut out = Vec::new();
            {
                let mut s = lock(shared);
                let t = s.engine.apply(&CodexInput::TurnAccepted(id.to_string()), Instant::now());
                s.settle(t.map_or_else(|| "codex:turn/start".into(), |t| t.cause), &mut out);
            }
            for e in out {
                sink.emit(e);
            }
        }
        Ok(())
    };
    let r = match turn {
        Some(t) => {
            let p = json!({
                "threadId": thread,
                "expectedTurnId": t,
                "input": [{ "type": "text", "text": text }],
                "clientUserMessageId": client_id,
            });
            match conn.request("turn/steer", p, timeout) {
                Err(CallError::Rpc { message, .. }) if !lock(shared).engine.turn_open() => {
                    sink.emit(DriverEvent::Step(json!({ "type": "steer_fell_back", "error": message })));
                    start()
                }
                other => other.map(|_| ()),
            }
        }
        None => start(),
    };
    match r {
        Ok(()) => {}
        Err(CallError::Timeout(e)) => {
            sink.emit(DriverEvent::Step(json!({ "type": "send_unconfirmed", "text": text, "error": e })));
        }
        Err(e) => {
            lock(shared).unconfirmed.remove(&client_id);
            sink.emit(DriverEvent::Step(json!({ "type": "send_failed", "error": e.to_string() })));
            sink.emit(DriverEvent::DeliveryAborted { text: text.to_string(), reason: e.to_string() });
        }
    }
}

impl CodexDriver {
    /// Spawns `codex app-server` in `cwd` in its own process group, stderr appended to
    /// `stderr_log`, and connects (see [`CodexDriver::connect`]).
    pub fn spawn(
        bin: &Path,
        cwd: &str,
        resume: Option<&str>,
        env_remove: &[String],
        stderr_log: &Path,
        sink: Sink,
        timing: CodexTiming,
    ) -> Result<Arc<CodexDriver>, DriverError> {
        let stderr = std::fs::OpenOptions::new().create(true).append(true).open(stderr_log)?;
        let mut cmd = Command::new(bin);
        cmd.arg("app-server")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .process_group(0);
        for k in env_remove {
            cmd.env_remove(k);
        }
        if std::env::var_os("RUST_LOG").is_none() {
            cmd.env("RUST_LOG", "warn");
        }
        let mut child = cmd.spawn()?;
        let root = proctree::proc_of(child.id() as i32);
        let stdin = child.stdin.take().ok_or_else(|| DriverError::Failed("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| DriverError::Failed("no stdout".into()))?;
        let process = Process {
            child: Mutex::new(Some(child)),
            root,
            stderr_log: Some(stderr_log.to_path_buf()),
            code: Mutex::default(),
        };
        CodexDriver::connect_process(BufReader::new(stdout), stdin, Some(Arc::new(process)), cwd, resume, sink, timing)
    }

    /// Handshake over any stream pair (tests use a socket pair): `initialize`,
    /// `initialized`, then `thread/start` or `thread/resume` in auto-edits mode, whose
    /// effective policy is checked. Emits `NativeId` and `Boot(Ready)`.
    pub fn connect(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        cwd: &str,
        resume: Option<&str>,
        sink: Sink,
        timing: CodexTiming,
    ) -> Result<Arc<CodexDriver>, DriverError> {
        CodexDriver::connect_process(reader, writer, None, cwd, resume, sink, timing)
    }

    fn connect_process(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        process: Option<Arc<Process>>,
        cwd: &str,
        resume: Option<&str>,
        sink: Sink,
        timing: CodexTiming,
    ) -> Result<Arc<CodexDriver>, DriverError> {
        let shared = Arc::new(Mutex::new(Shared {
            engine: CodexEngine::new(timing.settle_after),
            thread_id: None,
            approvals: HashMap::new(),
            file_items: HashMap::new(),
            file_order: VecDeque::new(),
            fanout: Fanout::default(),
            scrollback: Vec::new(),
            reported: (Status::Idle, false, None),
            sandbox_policy: Value::Null,
            unconfirmed: HashMap::new(),
            rate_limits: None,
            token_usage: None,
            closed: false,
        }));
        let ctx = Arc::new(Ctx {
            shared: shared.clone(),
            sink: sink.clone(),
            conn: OnceLock::new(),
            process: process.clone(),
        });
        let c2 = ctx.clone();
        let conn = RpcConn::start(reader, writer, move |i| on_incoming(&c2, i));
        let _ = ctx.conn.set(conn.clone());
        let (tx, rx) = mpsc::channel();
        // From here on a failure drops the driver, which ends the process tree.
        let driver = Arc::new(CodexDriver {
            conn: conn.clone(),
            shared: shared.clone(),
            sink: sink.clone(),
            work: Mutex::new(Some(tx)),
            process,
            stopped: Arc::default(),
            timing,
        });
        let fail = |e: CallError| DriverError::Failed(format!("codex app-server: {e}"));
        let info =
            json!({ "clientInfo": { "name": "mirante", "title": "Mirante", "version": env!("CARGO_PKG_VERSION") } });
        conn.request("initialize", info, timing.request).map_err(fail)?;
        conn.notify("initialized", Value::Null)?;
        let method = if resume.is_some() { "thread/resume" } else { "thread/start" };
        let result = match conn.request(method, thread_params(cwd, resume), timing.request) {
            Ok(r) => r,
            Err(CallError::Rpc { message, .. }) if resume.is_some() && message.contains("no rollout found") => {
                return Err(DriverError::NativeGone(format!("codex thread {}: {message}", resume.unwrap_or_default())));
            }
            Err(e) => return Err(fail(e)),
        };
        check_mode(&result).map_err(DriverError::Failed)?;
        let thread_id = result["thread"]["id"]
            .as_str()
            .ok_or_else(|| DriverError::Failed(format!("{method} returned no thread id: {result}")))?
            .to_string();
        {
            let mut s = lock(&shared);
            s.thread_id = Some(thread_id.clone());
            s.engine.set_thread(&thread_id);
            s.sandbox_policy = result["sandbox"].clone();
        }
        sink.emit(DriverEvent::NativeId(thread_id));
        sink.emit(DriverEvent::Boot(BootState::Ready));
        let (wsh, wsink, wconn) = (shared.clone(), sink.clone(), conn.clone());
        std::thread::spawn(move || {
            for w in rx {
                match w {
                    Work::Send(text) => send_now(&wconn, &wsh, &wsink, &text, timing.request),
                    Work::Interrupt(turn) => {
                        let thread = lock(&wsh).thread_id.clone();
                        let p = json!({ "threadId": thread, "turnId": turn });
                        if let Err(e) = wconn.request("turn/interrupt", p, timing.request) {
                            wsink
                                .emit(DriverEvent::Step(json!({ "type": "interrupt_failed", "error": e.to_string() })));
                        }
                    }
                }
            }
        });
        let (weak, tsink, stopped): (Weak<Mutex<Shared>>, Sink, Arc<AtomicBool>) =
            (Arc::downgrade(&shared), sink.clone(), driver.stopped.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(timing.tick);
            let Some(shared) = weak.upgrade() else { return };
            let mut out = Vec::new();
            {
                let mut s = lock(&shared);
                if s.closed || stopped.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(t) = s.engine.apply(&CodexInput::Tick, Instant::now()) {
                    s.settle(t.cause, &mut out);
                }
            }
            for e in out {
                tsink.emit(e);
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

    /// Ends the whole process tree (SIGTERM to its group and descendants, SIGKILL after
    /// `grace`), then closes stdin and reaps.
    fn end_process(&self, grace: Duration) {
        self.stopped.store(true, Ordering::SeqCst);
        lock(&self.work).take();
        if let Some(p) = &self.process {
            if let Some(root) = p.root {
                proctree::kill_tree(root, grace, &Kill);
            }
        }
        self.conn.close();
        if let Some(p) = &self.process {
            p.reap(Duration::ZERO);
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
        let turn = lock(&self.shared).engine.turn_id().map(str::to_string);
        let turn = turn.ok_or_else(|| DriverError::NotNow("no running turn to interrupt".into()))?;
        self.queue(Work::Interrupt(turn))
    }

    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError> {
        let mut s = lock(&self.shared);
        let (id, method, params) =
            s.approvals.remove(key).ok_or_else(|| DriverError::NoDialog(format!("no pending approval {key}")))?;
        // Queued for the writer thread: never blocks on the child's stdin.
        if let Err(e) = self.conn.respond(&id, approval_response(&method, &params, allow)) {
            s.approvals.insert(key.to_string(), (id, method, params));
            return Err(DriverError::Failed(format!("answer not sent: {e}")));
        }
        s.output(&format!("[{}]\r\n", if allow { "allowed once" } else { "denied" }));
        let t = s.engine.apply(&CodexInput::ApprovalAnswered(key.to_string()), Instant::now());
        let mut out = Vec::new();
        s.settle(t.map_or_else(|| "mirante:approval_answered".into(), |t| t.cause), &mut out);
        drop(s);
        for e in out {
            self.sink.emit(e);
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
        let mut initial = format!("{MODE_BANNER}\r\n").into_bytes();
        initial.extend_from_slice(&s.scrollback);
        Ok(Attachment { id, initial, output, input: false })
    }

    fn input(&self, _attachment: u64, _bytes: &[u8]) -> Result<(), DriverError> {
        Err(DriverError::Unsupported("typing into a Codex session (use mirante say)"))
    }

    fn detach(&self, attachment: u64) {
        lock(&self.shared).fanout.remove(attachment);
    }

    fn screen_tail(&self) -> String {
        let s = lock(&self.shared);
        tail_lines(&String::from_utf8_lossy(&s.scrollback).replace("\r\n", "\n"), 20)
    }

    fn stop(&self) {
        self.end_process(self.timing.stop_grace);
    }

    fn root(&self) -> Option<Proc> {
        self.process.as_ref().and_then(|p| p.root)
    }
}
```

Replace `crates/mirante-daemon/src/driver/real.rs` with (Task 15's Claude launch and its tests unchanged, plus the Codex launch):
```rust
//! The production driver factory: the real `claude` in a PTY with Mirante's scoped hook
//! settings, or the real `codex app-server` in auto-edits mode (its stderr in a private log
//! under `~/.mirante/sessions/<task>/`). Binaries: `MIRANTE_CLAUDE_BIN` /
//! `MIRANTE_CODEX_BIN`, else PATH without wrapper shims. The hook command:
//! `MIRANTE_HOOK_BIN`, else `mirante-hook` next to the daemon executable.
//!
//! Claude's launch comes from Plan 2a's `claude_launch::launch_spec` (real binary, env
//! scrub that never touches auth, `--permission-mode manual`, additive `--settings`, the
//! agent terminal variables, `MIRANTE_TASK_ID` and `MIRANTE_SESSION_ID` = this Mirante
//! session, which the hook echoes back and the hub fences on). Added here: `MIRANTE_HOME`
//! (the hook must reach this daemon) and, on a fresh start, `--session-id <uuid>`, so the
//! native id is known before Claude prints anything.
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mirante_core::claude_launch::{launch_spec, resolve_claude_bin_from_env, settings_json, LaunchInput};
use mirante_core::model::AgentKind;
use mirante_ipc::paths::HOME_ENV;

use super::claude::{ClaudeDriver, ClaudeTiming};
use super::codex::{codex_env_to_scrub, resolve_codex_bin, CodexDriver, CodexTiming};
use super::pty::PtySpec;
use super::{Driver, DriverError, DriverEvent, DriverFactory, Sink, SpawnRequest};

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

/// A random (version 4) UUID, lowercase `8-4-4-4-12`, from `/dev/urandom`.
pub fn new_uuid_v4() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

fn parent_env() -> Vec<String> {
    std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect()
}

/// The PTY launch for `req`, and the native session id pinned for a fresh start
/// (`None` when resuming: the id to resume is `req.resume`).
pub fn claude_spec(
    req: &SpawnRequest,
    claude: &Path,
    settings: &Path,
    size: (u16, u16),
    parent_env: Vec<String>,
) -> std::io::Result<(PtySpec, Option<String>)> {
    let spec = launch_spec(LaunchInput {
        claude_bin: claude,
        worktree: Path::new(&req.task.worktree_path),
        settings_path: settings,
        task_id: &req.task.id,
        session_id: &req.session_id,
        resume: req.resume.as_deref(),
        parent_env,
    });
    let mut args = spec.args;
    let pinned = match req.resume {
        Some(_) => None,
        None => {
            let id = new_uuid_v4()?;
            args.push("--session-id".to_string());
            args.push(id.clone());
            Some(id)
        }
    };
    let mut env_set = spec.env_set;
    // The hook (a child of claude) must reach this daemon, not one under another home.
    env_set.push((HOME_ENV.to_string(), req.paths.home().to_string_lossy().into_owned()));
    let pty = PtySpec {
        program: spec.program,
        args,
        cwd: spec.cwd,
        env_remove: spec.env_remove,
        env_set,
        rows: size.0,
        cols: size.1,
    };
    Ok((pty, pinned))
}

impl RealFactory {
    fn claude(&self, req: &SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        let claude = resolve_claude_bin_from_env().map_err(|e| DriverError::Failed(e.to_string()))?;
        let hook = hook_bin()?;
        let settings = req.paths.session_dir(&req.task.id).join("claude-settings.json");
        write_private(&settings, &settings_json(&hook.to_string_lossy(), &[]).to_string())?;
        let (pty, pinned) = claude_spec(req, &claude, &settings, (self.rows, self.cols), parent_env())?;
        if let Some(id) = pinned {
            // Known before Claude starts: a crash during boot can still be resumed.
            sink.emit(DriverEvent::NativeId(id));
        }
        Ok(ClaudeDriver::spawn(pty, sink, ClaudeTiming::default(), req.resume.is_some())?)
    }

    fn codex(&self, req: &SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        let over = std::env::var("MIRANTE_CODEX_BIN").ok();
        let bin = resolve_codex_bin(over.as_deref(), std::env::var("PATH").ok().as_deref())?;
        let log = req.paths.session_dir(&req.task.id).join("codex-stderr.log");
        write_private(&log, "")?;
        let env_remove = codex_env_to_scrub(parent_env());
        let d = CodexDriver::spawn(
            &bin,
            &req.task.worktree_path,
            req.resume.as_deref(),
            &env_remove,
            &log,
            sink,
            CodexTiming::default(),
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
    use mirante_core::model::{Lifecycle, Status, Task};
    use mirante_ipc::test_dir::TestDir;
    use mirante_ipc::Paths;

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

    #[test]
    fn uuids_are_random_version_4() {
        let (a, b) = (new_uuid_v4().unwrap(), new_uuid_v4().unwrap());
        assert_ne!(a, b);
        let parts: Vec<usize> = a.split('-').map(str::len).collect();
        assert_eq!(parts, vec![8, 4, 4, 4, 12]);
        assert_eq!(&a[14..15], "4");
        assert!("89ab".contains(&a[19..20]), "{a}");
        assert!(a.chars().all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    fn request(resume: Option<&str>) -> SpawnRequest {
        SpawnRequest {
            task: Task {
                id: "task-1".into(),
                title: "T".into(),
                repo_path: "/r".into(),
                worktree_path: "/w/tree".into(),
                branch: "feat".into(),
                agent: AgentKind::Claude,
                goal: "g".into(),
                notes: String::new(),
                lifecycle: Lifecycle::Active,
                status: Status::Idle,
                created_at: 0,
                updated_at: 0,
            },
            session_id: "sess-42".into(),
            resume: resume.map(String::from),
            paths: Paths::new(Path::new("/h")),
        }
    }

    fn env<'a>(spec: &'a PtySpec, key: &str) -> Option<&'a str> {
        spec.env_set.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_fresh_launch_pins_the_session_id_and_stamps_the_mirante_session() {
        let parent = vec!["CLAUDECODE".to_string(), "ANTHROPIC_API_KEY".to_string()];
        let (spec, pinned) =
            claude_spec(&request(None), Path::new("/bin/claude"), Path::new("/h/s.json"), (40, 120), parent).unwrap();
        let id = pinned.expect("fresh start pins an id");
        let args = spec.args.join(" ");
        assert!(args.contains(&format!("--session-id {id}")), "{args}");
        assert!(args.contains("--permission-mode manual") && args.contains("--settings /h/s.json"), "{args}");
        assert!(!args.contains("--resume") && !args.contains("dangerously"), "{args}");
        assert_eq!(env(&spec, "MIRANTE_SESSION_ID"), Some("sess-42"));
        assert_eq!(env(&spec, "MIRANTE_TASK_ID"), Some("task-1"));
        assert_eq!(env(&spec, "MIRANTE_HOME"), Some("/h"));
        assert_eq!(env(&spec, "TERM"), Some("xterm-256color"));
        assert!(spec.env_remove.contains(&"CLAUDECODE".to_string()));
        assert!(!spec.env_remove.iter().any(|k| k.starts_with("ANTHROPIC_")));
        assert_eq!((spec.cwd.as_path(), spec.rows, spec.cols), (Path::new("/w/tree"), 40, 120));
    }

    #[test]
    fn a_resume_passes_the_native_id_and_pins_nothing() {
        let (spec, pinned) =
            claude_spec(&request(Some("nat-1")), Path::new("/bin/claude"), Path::new("/h/s.json"), (40, 120), vec![])
                .unwrap();
        assert_eq!(pinned, None);
        let args = spec.args.join(" ");
        assert!(args.contains("--resume nat-1") && !args.contains("--session-id"), "{args}");
        assert_eq!(env(&spec, "MIRANTE_SESSION_ID"), Some("sess-42"));
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 163 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

`cargo clippy -p mirante-daemon --all-targets` prints no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon/src/driver/mod.rs crates/mirante-daemon/src/driver/codex.rs crates/mirante-daemon/src/driver/real.rs
git commit -F - <<'EOF'
feat(daemon): codex app-server driver in auto-edits mode with checked policy, withdrawn approvals, process-group stop

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

## Phase 2b-iv — Memory guard

### Task 18: Memory guard: sampling and policy

**Files:**
- Create: `crates/mirante-daemon/src/memguard/mod.rs`
- Create: `crates/mirante-daemon/src/memguard/sampler.rs`
- Create: `crates/mirante-daemon/src/memguard/policy.rs`
- Create: `crates/mirante-daemon/src/memguard/orphans.rs`
- Modify: `crates/mirante-daemon/src/lib.rs`
- Create: `docs/dogfood/memory-stress.md`

A handful of agents (each a Node process plus its tools, dev servers and test runners) can take a 16 GB Mac into swap. This task builds the measuring and the deciding, as a module the hub does not use yet (Task 19 wires it in: the sampling thread, hibernate / wake, `mirante doctor`, the CLI columns). Facts measured on the founder's Mac, and what follows from them:

- **No hard limits on macOS.** `setrlimit(RLIMIT_AS / RLIMIT_DATA)` is not enforced there (`EINVAL` or no effect), so Mirante measures and acts instead of limiting. Linux could use a cgroup: `Config::scope_prefix` builds a `systemd-run --user --scope --quiet -p MemoryHigh=<budget> --` argv prefix, off by default (`systemd_scope: false`); nothing executes it in this plan.
- **What a process weighs.** macOS: `phys_footprint` from `proc_pid_rusage(pid, RUSAGE_INFO_V4)` (the number Activity Monitor shows; RSS undercounts compressed and swapped memory). Linux: `Pss` from `/proc/<pid>/smaps_rollup`, else `VmRSS`. A session weighs its whole process tree (Task 13's `proctree::descendants`, pid + start time).
- **What the machine says.** macOS: `hw.memsize`, `vm.swapusage` (`xsw_usage`), the kernel's own pressure level `kern.memorystatus_vm_pressure_level` (1 normal, 2 warn, 4 critical), and a conservative "free" = (`vm.page_free_count` + `vm.page_speculative_count` + `vm.page_purgeable_count`) × `hw.pagesize` (sysctl only, no Mach host calls; the inactive file cache is not counted, so "free" reads low — it is only used as a trend). Linux: `/proc/meminfo` (`MemTotal`, `MemAvailable`, swap) and PSI `/proc/pressure/memory` (`some avg10` ≥ 10 → warn, ≥ 40 → critical).
- **Budget and levels.** Per session `min(4 GiB, 25 % of RAM)` (configurable): `Soft` at 80 %, `Hard` at 100 %, each warned once when entered. Machine level: the kernel's own measure decides when there is one (macOS `memorystatus` level, Linux PSI; `SystemSample::kernel_signal`): warn → `Elevated`, critical → `Critical`. Swap and free are shown but not used then, because macOS keeps swap allocated and free pages near zero (file cache) on a machine under no pressure — measured on the founder's Mac while writing this task: 90 % swap used, ~2 % "free", kernel level normal; a swap rule paused every dispatch. Without a kernel signal (Linux without PSI): `Elevated` when swap > 75 % with under 10 % free; `Critical` when swap > 90 % with under 5 % free and falling. Up at once; down only after 3 calm samples below a 5-point margin (70 % / 85 %), so the level does not flap. Sampling every 2 s, every 500 ms while not `Normal` (`Policy::next_interval`).
- **The ladder, and what is never automatic.** `Elevated` → `Pause(true)` (no new dispatches, no queued deliveries) and an offer to hibernate the largest idle session (the user confirms; at most one offer per 60 s). `Critical` sustained 10 s → `AutoHibernate` of the largest IDLE session, at most one per 60 s. A session that is working (streaming turn, running tool, background work) or has a pending permission request is never hibernated automatically; when only such sessions are left the policy returns `Refused` with the reason ("a permission request is pending: never hibernated automatically"), also at most once per 60 s. Leak detector: an idle session growing faster than 200 MB/min over (at least half of) a 5-minute window → `Leak` plus an offer, once until the growth stops. Idle hibernation (`idle_hibernate`, e.g. 60 min) is off by default and never touches a session with a pending permission. Every action is returned to the hub, which logs it as an event; the policy itself signals nothing.
- **Orphans.** Processes re-parented to pid 1, owned by us, whose cwd or command line points into the worktree root (macOS: `proc_listallpids`, `PROC_PIDTBSDINFO`, `PROC_PIDVNODEPATHINFO`, `KERN_PROCARGS2`; Linux: `/proc`). Never matched by process name, never killed automatically: `kill_orphan` is for an explicit user request, re-verifies the start time and ends only that process tree.

The policy is pure (the clock is a parameter), so every rule is tested without sleeping or allocating; the samplers and the orphan sweep are tested against real small processes (`sleep`, `sh`), never against a memory hog. The manual stress procedure is `docs/dogfood/memory-stress.md` (warns to close apps first).

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/lib.rs`: add the module after `pub mod hub;`:
```rust
pub mod memguard;
```

`crates/mirante-daemon/src/memguard/mod.rs` (module list only for now):
```rust
pub mod orphans;
pub mod policy;
pub mod sampler;
```

`crates/mirante-daemon/src/memguard/sampler.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::proctree::proc_of;

    #[test]
    fn our_own_tree_has_a_sane_footprint() {
        let me = proc_of(std::process::id() as i32).unwrap();
        let s = os_sampler().tree(me).unwrap();
        // A test binary: more than 1 MiB, far less than 64 GiB.
        assert!(s.bytes > 1 << 20 && s.bytes < 64 << 30, "{s:?}");
        assert!(s.procs >= 1);
    }

    #[test]
    fn a_tree_weighs_at_least_its_root() {
        let mut child =
            std::process::Command::new("/bin/sh").args(["-c", "sleep 30 & sleep 30 & wait"]).spawn().unwrap();
        let root = proc_of(child.id() as i32).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while descendants(root).len() < 2 {
            assert!(std::time::Instant::now() < deadline, "children never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let sampler = os_sampler();
        let tree = sampler.tree(root).unwrap();
        let alone = process_bytes(root.pid).unwrap();
        assert!(tree.procs >= 3, "{tree:?}");
        assert!(tree.bytes > alone, "{tree:?} vs root alone {alone}");
        let r =
            crate::driver::proctree::kill_tree(root, std::time::Duration::from_secs(2), &crate::driver::proctree::Kill);
        assert!(r.survivors.is_empty());
        let _ = child.wait();
        assert_eq!(sampler.tree(root), None, "a gone root has no sample");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_sample_matches_the_kernel() {
        let s = MacSampler.system();
        assert_eq!(Some(s.total), mac::sysctl_int("hw.memsize"));
        assert!(s.free > 0 && s.free < s.total, "{s:?}");
        assert!(s.swap_used <= s.swap_total || s.swap_total == 0, "{s:?}");
        let raw = mac::sysctl_int("kern.memorystatus_vm_pressure_level").unwrap();
        assert!([1, 2, 4].contains(&raw), "pressure level {raw}");
        assert!(s.kernel_signal);
    }

    #[test]
    fn the_fake_returns_what_it_was_given() {
        let f = FakeSampler::default();
        let p = Proc { pid: 42, start: 1 };
        assert_eq!(f.tree(p), None);
        f.set_tree(42, 7);
        assert_eq!(f.tree(p), Some(TreeSample { bytes: 7, procs: 1 }));
        f.set_system(SystemSample { total: 100, swap_used: 3, swap_total: 4, ..Default::default() });
        assert_eq!(f.system().swap_pct(), 75);
        assert_eq!(SystemSample::default().swap_pct(), 0);
    }
}
```

`crates/mirante-daemon/src/memguard/policy.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    fn cfg() -> Config {
        Config::for_ram(16 * GB)
    }

    fn sys(pressure: Pressure, swap_pct: u64, free: u64) -> SystemSample {
        SystemSample {
            total: 16 * GB,
            free,
            swap_used: swap_pct * GB / 100,
            swap_total: GB,
            pressure,
            kernel_signal: false,
        }
    }

    fn normal() -> SystemSample {
        sys(Pressure::Normal, 0, 4 * GB)
    }

    fn s(task: &str, idle: bool, pending: bool, bytes: u64) -> (SessionInfo, Option<TreeSample>) {
        let info =
            SessionInfo { task_id: task.into(), root: None, idle, pending_permission: pending, idle_since: None };
        (info, Some(TreeSample { bytes, procs: 1 }))
    }

    fn kinds(a: &[Action]) -> Vec<String> {
        a.iter()
            .map(|a| match a {
                Action::Warn { task_id, state, .. } => format!("warn:{task_id}:{}", state.as_str()),
                Action::Pause(p) => format!("pause:{p}"),
                Action::OfferHibernate { task_id, .. } => format!("offer:{task_id}"),
                Action::AutoHibernate { task_id, .. } => format!("auto:{task_id}"),
                Action::Leak { task_id, .. } => format!("leak:{task_id}"),
                Action::IdleHibernate { task_id } => format!("idle:{task_id}"),
                Action::LevelChanged { to, .. } => format!("level:{}", to.as_str()),
                Action::Refused { task_id, .. } => format!("refused:{task_id}"),
            })
            .collect()
    }

    #[test]
    fn defaults_follow_the_ram_and_leave_idle_hibernation_off() {
        assert_eq!(Config::for_ram(16 * GB).budget, 4 * GB);
        assert_eq!(Config::for_ram(8 * GB).budget, 2 * GB);
        let c = cfg();
        assert_eq!((c.soft_pct, c.hard_pct, c.idle_hibernate, c.systemd_scope), (80, 100, None, false));
        assert_eq!(c.scope_prefix(), None);
        let on = Config { systemd_scope: true, budget: 1000, ..c };
        assert_eq!(
            on.scope_prefix().unwrap(),
            ["systemd-run", "--user", "--scope", "--quiet", "-p", "MemoryHigh=1000", "--"]
        );
    }

    #[test]
    fn soft_and_hard_are_reported_once_per_state() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        assert!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, GB)])).is_empty());
        assert_eq!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, 3 * GB + GB / 2)])), ["warn:a:soft"]);
        assert!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, 3 * GB + GB / 2)])).is_empty());
        assert_eq!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, 5 * GB)])), ["warn:a:hard"]);
        let m = p.session("a").unwrap();
        assert_eq!((m.bytes, m.peak, m.budget, m.state), (5 * GB, 5 * GB, 4 * GB, MemState::Hard));
        p.tick(t0, &normal(), &[s("a", false, false, GB)]);
        assert_eq!(p.session("a").unwrap().peak, 5 * GB);
        assert_eq!(kinds(&p.tick(t0, &normal(), &[s("a", false, false, 3 * GB + GB / 2)])), ["warn:a:soft"]);
        p.forget("a");
        assert_eq!(p.session("a"), None);
    }

    #[test]
    fn the_level_steps_up_at_once_and_down_only_after_calm_samples() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        assert_eq!(p.next_interval(), Duration::from_secs(2));
        assert_eq!(kinds(&p.tick(t0, &sys(Pressure::Warn, 0, GB), &[])), ["level:elevated", "pause:true"]);
        assert_eq!(p.next_interval(), Duration::from_millis(500));
        // Swap at 72 %: below the 75 % entry threshold but above the 70 % exit margin.
        for _ in 0..5 {
            assert!(kinds(&p.tick(t0, &sys(Pressure::Normal, 72, GB), &[])).is_empty());
        }
        assert_eq!(p.level(), Level::Elevated);
        assert!(kinds(&p.tick(t0, &normal(), &[])).is_empty());
        assert!(kinds(&p.tick(t0, &normal(), &[])).is_empty());
        assert_eq!(kinds(&p.tick(t0, &normal(), &[])), ["level:normal", "pause:false"]);
        // Swap over 90 % with little free memory: elevated; falling under 5 %: critical.
        p.tick(t0, &sys(Pressure::Normal, 95, 3 * GB / 2), &[]);
        assert_eq!(p.level(), Level::Elevated);
        assert_eq!(kinds(&p.tick(t0, &sys(Pressure::Normal, 95, GB / 2), &[])), ["level:critical"]);
    }

    /// Measured on the founder's Mac: swap 90 % used, 2 % "free" (file cache), kernel level
    /// normal. With the kernel's own level, that is normal: nothing is paused.
    #[test]
    fn the_kernels_level_wins_over_swap_and_free() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let mac = SystemSample { kernel_signal: true, ..sys(Pressure::Normal, 90, GB / 3) };
        for i in 0..5 {
            assert!(kinds(&p.tick(t0 + Duration::from_secs(i), &mac, &[])).is_empty());
        }
        assert_eq!(p.level(), Level::Normal);
        let warn = SystemSample { kernel_signal: true, ..sys(Pressure::Warn, 0, 8 * GB) };
        assert_eq!(kinds(&p.tick(t0 + Duration::from_secs(9), &warn, &[])), ["level:elevated", "pause:true"]);
        // Without a kernel signal the same swap counts only with little free memory.
        let mut q = Policy::new(cfg());
        assert!(kinds(&q.tick(t0, &sys(Pressure::Normal, 92, 4 * GB), &[])).is_empty());
    }

    #[test]
    fn pressure_pauses_and_offers_the_largest_idle_session_once_per_cooldown() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let sessions = [s("busy", false, false, 3 * GB), s("small", true, false, GB / 2), s("big", true, false, GB)];
        let a = kinds(&p.tick(t0, &sys(Pressure::Warn, 0, GB), &sessions));
        assert_eq!(a, ["level:elevated", "pause:true", "offer:big"]);
        assert!(kinds(&p.tick(t0 + Duration::from_secs(30), &sys(Pressure::Warn, 0, GB), &sessions)).is_empty());
        assert_eq!(kinds(&p.tick(t0 + Duration::from_secs(61), &sys(Pressure::Warn, 0, GB), &sessions)), ["offer:big"]);
    }

    #[test]
    fn sustained_critical_hibernates_only_an_idle_session_without_a_pending_permission() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let crit = sys(Pressure::Critical, 0, GB);
        let sessions = [s("busy", false, false, 3 * GB), s("asking", true, true, 2 * GB), s("idle", true, false, GB)];
        // Not sustained yet: only the offer.
        assert_eq!(kinds(&p.tick(t0, &crit, &sessions)), ["level:critical", "pause:true", "offer:idle"]);
        assert!(kinds(&p.tick(t0 + Duration::from_secs(5), &crit, &sessions)).is_empty());
        assert_eq!(kinds(&p.tick(t0 + Duration::from_secs(10), &crit, &sessions)), ["auto:idle"]);
        // One automatic action per cooldown.
        assert!(kinds(&p.tick(t0 + Duration::from_secs(40), &crit, &sessions)).is_empty());
        let later = t0 + Duration::from_secs(71);
        assert_eq!(kinds(&p.tick(later, &crit, &sessions)), ["auto:idle"]);
    }

    #[test]
    fn a_pending_permission_or_a_busy_agent_is_refused_explicitly() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let crit = sys(Pressure::Critical, 0, GB);
        let asking = [s("busy", false, false, 3 * GB), s("asking", true, true, GB)];
        p.tick(t0, &crit, &asking);
        let a = p.tick(t0 + Duration::from_secs(10), &crit, &asking);
        assert_eq!(kinds(&a), ["refused:asking"]);
        let Action::Refused { reason, .. } = &a[0] else { unreachable!() };
        assert!(reason.contains("permission request is pending"), "{reason}");
        // Refusals are not repeated within the cooldown.
        assert!(kinds(&p.tick(t0 + Duration::from_secs(20), &crit, &asking)).is_empty());
        let mut q = Policy::new(cfg());
        let busy = [s("busy", false, false, 3 * GB)];
        q.tick(t0, &crit, &busy);
        let a = q.tick(t0 + Duration::from_secs(10), &crit, &busy);
        assert_eq!(kinds(&a), ["refused:busy"]);
        assert!(matches!(&a[0], Action::Refused { reason, .. } if reason.contains("working")));
    }

    #[test]
    fn an_idle_session_that_keeps_growing_is_a_leak() {
        let mut p = Policy::new(cfg());
        let t0 = Instant::now();
        let mut out = Vec::new();
        for m in 0..=3u64 {
            // 300 MB per minute, idle.
            out.extend(p.tick(t0 + Duration::from_secs(60 * m), &normal(), &[s("a", true, false, GB + m * 300 * MIB)]));
        }
        assert_eq!(kinds(&out), ["leak:a", "offer:a"]);
        assert!(matches!(&out[0], Action::Leak { mb_per_min: 300, .. }));
        // Reported once; growth while working is not a leak.
        let mut q = Policy::new(cfg());
        let mut out = Vec::new();
        for m in 0..=3u64 {
            out.extend(q.tick(
                t0 + Duration::from_secs(60 * m),
                &normal(),
                &[s("a", false, false, GB + m * 300 * MIB)],
            ));
        }
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn idle_hibernation_happens_only_when_configured() {
        let t0 = Instant::now();
        let idle = |since: Instant| {
            let (mut info, sample) = s("a", true, false, GB);
            info.idle_since = Some(since);
            [(info, sample)]
        };
        let mut off = Policy::new(cfg());
        assert!(off.tick(t0 + Duration::from_secs(7200), &normal(), &idle(t0)).is_empty());
        let mut on = Policy::new(Config { idle_hibernate: Some(Duration::from_secs(3600)), ..cfg() });
        assert!(on.tick(t0 + Duration::from_secs(3599), &normal(), &idle(t0)).is_empty());
        assert_eq!(kinds(&on.tick(t0 + Duration::from_secs(3600), &normal(), &idle(t0))), ["idle:a"]);
        assert!(on.tick(t0 + Duration::from_secs(3700), &normal(), &idle(t0)).is_empty(), "once per idle period");
        // Never with a pending permission.
        let (mut info, sample) = s("b", true, true, GB);
        info.idle_since = Some(t0);
        assert!(on.tick(t0 + Duration::from_secs(9000), &normal(), &[(info, sample)]).is_empty());
    }
}
```

`crates/mirante-daemon/src/memguard/orphans.rs` (only the test module for now):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;

    use mirante_ipc::test_dir::TestDir;

    use crate::driver::proctree::Kill;

    struct Recorder(Mutex<Vec<(i32, i32, bool)>>);

    impl Signaler for Recorder {
        fn signal(&self, pid: i32, sig: i32) -> bool {
            self.0.lock().unwrap().push((pid, sig, false));
            Kill.signal(pid, sig)
        }
        fn signal_group(&self, pgid: i32, sig: i32) -> bool {
            self.0.lock().unwrap().push((pgid, sig, true));
            Kill.signal_group(pgid, sig)
        }
    }

    /// Starts `inner` detached (its parent shell exits, so it is re-parented to pid 1) and
    /// returns it once it is an orphan.
    fn detached(inner: &str, dir: &Path, name: &str) -> Proc {
        let pidfile = dir.join(name);
        let script = format!("( {inner} ) >/dev/null 2>&1 & echo $! > '{}'", pidfile.display());
        assert!(std::process::Command::new("/bin/sh").args(["-c", &script]).status().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let pid = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<i32>().ok());
            if let Some(p) = pid.and_then(proc_of) {
                if sys::owner(p.pid).is_some_and(|(ppid, _)| ppid == 1) {
                    return p;
                }
            }
            assert!(Instant::now() < deadline, "{name} never became an orphan");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Kills what a test started, even when an assertion failed first.
    struct Cleanup(Vec<Proc>);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            for p in &self.0 {
                kill_tree(*p, Duration::from_millis(500), &Kill);
            }
        }
    }

    #[test]
    fn orphans_are_found_by_cwd_or_command_line_inside_the_worktree_root_only() {
        let d = TestDir::new();
        let root = d.path().join("worktrees");
        std::fs::create_dir_all(root.join("app/feat")).unwrap();
        let in_cwd = detached(&format!("cd '{}' && exec sleep 30", root.join("app/feat").display()), d.path(), "a");
        let in_argv =
            detached(&format!("cd / && exec sh -c 'sleep 30; :' '{}'", root.join("app/x").display()), d.path(), "b");
        let outside = detached("cd / && exec sleep 31", d.path(), "c");
        let _cleanup = Cleanup(vec![in_cwd, in_argv, outside]);
        let found = find_orphans(&root);
        let pids: Vec<i32> = found.iter().map(|o| o.proc.pid).collect();
        assert!(pids.contains(&in_cwd.pid), "{found:?}");
        assert!(pids.contains(&in_argv.pid), "{found:?}");
        assert!(!pids.contains(&outside.pid), "{found:?}");
        let by_cwd = found.iter().find(|o| o.proc.pid == in_cwd.pid).unwrap();
        assert_eq!(by_cwd.ppid, 1);
        assert!(by_cwd.cwd.as_ref().unwrap().ends_with("worktrees/app/feat"), "{by_cwd:?}");
        let by_argv = found.iter().find(|o| o.proc.pid == in_argv.pid).unwrap();
        assert!(by_argv.argv.contains("app/x"), "{by_argv:?}");
    }

    #[test]
    fn killing_an_orphan_signals_only_that_process_and_never_a_stale_one() {
        let d = TestDir::new();
        let root = d.path().join("worktrees");
        std::fs::create_dir_all(&root).unwrap();
        let p = detached(&format!("cd '{}' && exec sleep 30", root.display()), d.path(), "a");
        let _cleanup = Cleanup(vec![p]);
        let o = find_orphans(&root).into_iter().find(|o| o.proc == p).expect("found");
        // A stale entry (same pid, another start time) is never signaled.
        let stale = Orphan { proc: Proc { pid: p.pid, start: p.start.wrapping_sub(1) }, ..o.clone() };
        let rec = Recorder(Mutex::new(Vec::new()));
        assert_eq!(kill_orphan(&stale, &rec), KillReport::default());
        assert!(rec.0.lock().unwrap().is_empty());
        assert!(alive(p));
        let r = kill_orphan(&o, &rec);
        assert!(r.survivors.is_empty(), "{r:?}");
        assert!(!alive(p));
        let calls = rec.0.lock().unwrap();
        assert!(!calls.is_empty());
        assert!(calls.iter().all(|(pid, _, _)| *pid == p.pid), "{calls:?}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error[E0405]: cannot find trait `Signaler` in this scope
```

- [ ] **Step 3: Implement**

Replace `crates/mirante-daemon/src/memguard/mod.rs` with:
```rust
//! Memory guard: keep a handful of agents from taking the machine down.
//!
//! Measure and act, never limit: macOS does not enforce `RLIMIT_AS` / `RLIMIT_DATA`
//! (`setrlimit` fails with `EINVAL` or has no effect), so there are no hard limits. Instead
//! each live session's process tree is sampled ([`sampler`]: `phys_footprint` on macOS, PSS
//! on Linux) every 2 s (500 ms under pressure), the machine's pressure is read from the OS,
//! and a pure [`policy`] decides what to do:
//! - a session over 80 % / 100 % of its budget (default `min(4 GiB, 25 % of RAM)`) → a
//!   warning;
//! - the machine under pressure (`Elevated`) → new dispatches and queued deliveries pause,
//!   and the user is offered to hibernate the largest idle session;
//! - sustained `Critical` pressure → the largest IDLE session is hibernated, at most one per
//!   60 s, everything logged. Never a working session, never one with a pending permission
//!   request (the refusal is reported instead);
//! - an idle session that keeps growing → reported as a leak, with an offer;
//! - optional (off by default): hibernate sessions idle for a configured time.
//!
//! Hibernating is stopping the agent's process tree and resuming its native conversation
//! later; the hub does it (Task 19). Processes left behind by ended sessions are found by
//! [`orphans`] and only reported; killing one is the user's call.
pub mod orphans;
pub mod policy;
pub mod sampler;

pub use orphans::{find_orphans, kill_orphan, Orphan};
pub use policy::{Action, Config, Level, MemState, Policy, SessionInfo, SessionMem};
pub use sampler::{os_sampler, FakeSampler, MemSampler, Pressure, SystemSample, TreeSample};
```

Add above the test module in `crates/mirante-daemon/src/memguard/sampler.rs`:
```rust
//! Measuring memory: one agent's process tree and the whole machine.
//!
//! macOS: a process's memory is its `phys_footprint` (`proc_pid_rusage`, `RUSAGE_INFO_V4`),
//! the number Activity Monitor shows (RSS undercounts compressed and swapped pages). The
//! system side reads `hw.memsize`, `vm.swapusage`, the kernel's own pressure level
//! (`kern.memorystatus_vm_pressure_level`: 1 normal, 2 warn, 4 critical) and a conservative
//! "free" (`vm.page_free_count + vm.page_speculative_count + vm.page_purgeable_count`, times
//! `hw.pagesize`; inactive file cache is not counted).
//! Linux: `Pss` from `/proc/<pid>/smaps_rollup` (else `VmRSS` from `/proc/<pid>/status`),
//! `/proc/meminfo` (`MemTotal`, `MemAvailable`, swap) and PSI `/proc/pressure/memory`.
use std::collections::HashMap;
use std::sync::Mutex;

use crate::driver::proctree::{descendants, Proc};

/// Memory of one process tree (root plus descendants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeSample {
    pub bytes: u64,
    pub procs: usize,
}

/// The raw pressure signal of the operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pressure {
    #[default]
    Normal,
    Warn,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemSample {
    pub total: u64,
    pub free: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub pressure: Pressure,
    /// `pressure` came from the kernel's own measure (macOS memorystatus level, Linux PSI).
    /// Then swap and free are shown but not used to judge: macOS keeps swap allocated and
    /// free pages near zero (file cache) on a machine that is not under pressure.
    pub kernel_signal: bool,
}

impl SystemSample {
    /// Swap in use, in percent of the swap that exists (0 without swap).
    pub fn swap_pct(&self) -> u64 {
        (self.swap_used * 100).checked_div(self.swap_total).unwrap_or(0)
    }
}

pub trait MemSampler: Send + Sync {
    /// The tree under `root` (`None` when `root` is gone).
    fn tree(&self, root: Proc) -> Option<TreeSample>;
    fn system(&self) -> SystemSample;
}

/// One process's memory as this OS counts it (`None`: gone or not readable).
pub fn process_bytes(pid: i32) -> Option<u64> {
    #[cfg(target_os = "macos")]
    return mac::footprint(pid);
    #[cfg(target_os = "linux")]
    return linux::process_bytes(pid);
}

/// Sums `bytes_of` over `root` and its live descendants.
fn sum_tree(root: Proc, bytes_of: impl Fn(i32) -> Option<u64>) -> Option<TreeSample> {
    let mut s = TreeSample { bytes: bytes_of(root.pid)?, procs: 1 };
    for p in descendants(root) {
        if let Some(b) = bytes_of(p.pid) {
            s.bytes += b;
            s.procs += 1;
        }
    }
    Some(s)
}

#[cfg(target_os = "macos")]
pub struct MacSampler;

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_void, CString};
    use std::mem::{size_of, zeroed};

    /// `phys_footprint` of one process.
    pub fn footprint(pid: i32) -> Option<u64> {
        if pid <= 0 {
            return None;
        }
        // SAFETY: the buffer is a `rusage_info_v4`, the flavor asks for exactly that struct.
        let mut info: libc::rusage_info_v4 = unsafe { zeroed() };
        let r = unsafe {
            libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, &mut info as *mut _ as *mut libc::rusage_info_t)
        };
        (r == 0).then_some(info.ri_phys_footprint)
    }

    /// A fixed-size value from `sysctlbyname`.
    pub fn sysctl<T: Copy>(name: &str) -> Option<T> {
        let c = CString::new(name).ok()?;
        // SAFETY: `out` has the size passed; the kernel writes at most that many bytes.
        let mut out: T = unsafe { zeroed() };
        let mut len = size_of::<T>();
        let r = unsafe {
            libc::sysctlbyname(c.as_ptr(), &mut out as *mut T as *mut c_void, &mut len, std::ptr::null_mut(), 0)
        };
        (r == 0 && len == size_of::<T>()).then_some(out)
    }

    /// An integer sysctl that may be 32 or 64 bits wide.
    pub fn sysctl_int(name: &str) -> Option<u64> {
        sysctl::<u64>(name).or_else(|| sysctl::<u32>(name).map(u64::from))
    }
}

#[cfg(target_os = "macos")]
impl MemSampler for MacSampler {
    fn tree(&self, root: Proc) -> Option<TreeSample> {
        if !crate::driver::proctree::alive(root) {
            return None;
        }
        sum_tree(root, process_bytes)
    }

    fn system(&self) -> SystemSample {
        let page = mac::sysctl_int("hw.pagesize").unwrap_or(4096);
        let pages: u64 = ["vm.page_free_count", "vm.page_speculative_count", "vm.page_purgeable_count"]
            .iter()
            .filter_map(|n| mac::sysctl_int(n))
            .sum();
        let swap = mac::sysctl::<libc::xsw_usage>("vm.swapusage");
        let level = mac::sysctl_int("kern.memorystatus_vm_pressure_level");
        let pressure = match level {
            Some(4) => Pressure::Critical,
            Some(2) => Pressure::Warn,
            _ => Pressure::Normal,
        };
        SystemSample {
            total: mac::sysctl_int("hw.memsize").unwrap_or(0),
            free: pages * page,
            swap_used: swap.map_or(0, |s| s.xsu_used),
            swap_total: swap.map_or(0, |s| s.xsu_total),
            pressure,
            kernel_signal: level.is_some(),
        }
    }
}

#[cfg(target_os = "linux")]
pub struct LinuxSampler;

#[cfg(target_os = "linux")]
mod linux {
    /// The `kB` value of `key:` in a `/proc` file of `key: value kB` lines, in bytes.
    pub fn kb(text: &str, key: &str) -> Option<u64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map(|k| k * 1024)
    }

    /// Pss (proportional set size) when readable, else VmRSS.
    pub fn process_bytes(pid: i32) -> Option<u64> {
        if pid <= 0 {
            return None;
        }
        if let Some(pss) = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok().and_then(|t| kb(&t, "Pss"))
        {
            return Some(pss);
        }
        kb(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?, "VmRSS")
    }

    /// `some avg10` of PSI memory (percent of time some task stalled on memory).
    pub fn psi_some_avg10(text: &str) -> Option<f64> {
        let line = text.lines().find(|l| l.starts_with("some "))?;
        line.split_whitespace().find_map(|f| f.strip_prefix("avg10="))?.parse().ok()
    }
}

#[cfg(target_os = "linux")]
impl MemSampler for LinuxSampler {
    fn tree(&self, root: Proc) -> Option<TreeSample> {
        if !crate::driver::proctree::alive(root) {
            return None;
        }
        sum_tree(root, process_bytes)
    }

    fn system(&self) -> SystemSample {
        let mem = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let swap_total = linux::kb(&mem, "SwapTotal").unwrap_or(0);
        let psi = std::fs::read_to_string("/proc/pressure/memory").ok().and_then(|t| linux::psi_some_avg10(&t));
        let pressure = match psi {
            Some(p) if p >= 40.0 => Pressure::Critical,
            Some(p) if p >= 10.0 => Pressure::Warn,
            _ => Pressure::Normal,
        };
        SystemSample {
            total: linux::kb(&mem, "MemTotal").unwrap_or(0),
            free: linux::kb(&mem, "MemAvailable").unwrap_or(0),
            swap_used: swap_total.saturating_sub(linux::kb(&mem, "SwapFree").unwrap_or(0)),
            swap_total,
            pressure,
            kernel_signal: psi.is_some(),
        }
    }
}

/// The sampler for this operating system.
pub fn os_sampler() -> Box<dyn MemSampler> {
    #[cfg(target_os = "macos")]
    return Box::new(MacSampler);
    #[cfg(target_os = "linux")]
    return Box::new(LinuxSampler);
}

/// Test double: bytes per root pid and a fixed system sample, both settable.
#[derive(Default)]
pub struct FakeSampler {
    pub trees: Mutex<HashMap<i32, TreeSample>>,
    pub sys: Mutex<SystemSample>,
}

impl FakeSampler {
    pub fn set_tree(&self, pid: i32, bytes: u64) {
        self.trees.lock().unwrap().insert(pid, TreeSample { bytes, procs: 1 });
    }

    pub fn set_system(&self, s: SystemSample) {
        *self.sys.lock().unwrap() = s;
    }
}

impl MemSampler for FakeSampler {
    fn tree(&self, root: Proc) -> Option<TreeSample> {
        self.trees.lock().unwrap().get(&root.pid).copied()
    }

    fn system(&self) -> SystemSample {
        *self.sys.lock().unwrap()
    }
}
```

Add above the test module in `crates/mirante-daemon/src/memguard/policy.rs`:
```rust
//! The memory policy: a pure state machine fed one sample per tick, with the clock injected
//! (`now`), returning the actions the hub should take. It never touches a process itself.
//!
//! Per session: a budget (default `min(4 GiB, 25 % of RAM)`), `Soft` at 80 % and `Hard` at
//! 100 % of it, each reported once when the state is entered. For the machine: a [`Level`]
//! from the OS pressure signal and swap use, stepping up at once and down only after
//! [`CALM_SAMPLES`] calm samples below a lower threshold (hysteresis). The ladder:
//! 1. `Elevated` → pause new dispatches and queued deliveries, offer to hibernate the
//!    largest idle session (the user confirms; one offer per cooldown);
//! 2. `Critical` sustained for `critical_sustain` → hibernate the largest IDLE session
//!    automatically, one action per cooldown. Never a session that is working (turn, tool,
//!    background work) and never one with a pending permission request: when only such
//!    sessions are left, `Refused` says so instead.
//!
//! Also: an idle session that keeps growing faster than `leak_mb_per_min` over
//! `leak_window` is reported as a leak (with an offer), and, only when configured, a session
//! idle longer than `idle_hibernate` is hibernated.
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::sampler::{Pressure, SystemSample, TreeSample};
use crate::driver::proctree::Proc;

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;
/// Calm samples in a row before the level steps down.
pub const CALM_SAMPLES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Bytes one session's process tree may use.
    pub budget: u64,
    pub soft_pct: u8,
    pub hard_pct: u8,
    /// Sampling interval while the level is `Normal`.
    pub interval: Duration,
    /// Sampling interval under pressure.
    pub pressure_interval: Duration,
    /// At most one offer, and one automatic action, per cooldown.
    pub cooldown: Duration,
    /// How long `Critical` must last before a session is hibernated automatically.
    pub critical_sustain: Duration,
    /// Hibernate sessions idle this long (`None`: never; the default).
    pub idle_hibernate: Option<Duration>,
    /// An idle session growing faster than this is reported as leaking.
    pub leak_mb_per_min: u64,
    pub leak_window: Duration,
    /// Linux only: launch agents under `systemd-run --user --scope -p MemoryHigh=<budget>`.
    pub systemd_scope: bool,
}

impl Config {
    /// Defaults for a machine with `total` bytes of RAM.
    pub fn for_ram(total: u64) -> Config {
        Config {
            budget: (4 * GIB).min(total / 4).max(GIB / 4),
            soft_pct: 80,
            hard_pct: 100,
            interval: Duration::from_secs(2),
            pressure_interval: Duration::from_millis(500),
            cooldown: Duration::from_secs(60),
            critical_sustain: Duration::from_secs(10),
            idle_hibernate: None,
            leak_mb_per_min: 200,
            leak_window: Duration::from_secs(300),
            systemd_scope: false,
        }
    }

    /// The argv prefix that runs an agent in a systemd user scope with `MemoryHigh` set to
    /// the budget (the kernel then reclaims from it first). `None` unless `systemd_scope`.
    pub fn scope_prefix(&self) -> Option<Vec<String>> {
        self.systemd_scope.then(|| {
            ["systemd-run", "--user", "--scope", "--quiet", "-p"]
                .iter()
                .map(|s| s.to_string())
                .chain([format!("MemoryHigh={}", self.budget), "--".to_string()])
                .collect()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Normal,
    Elevated,
    Critical,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Normal => "normal",
            Level::Elevated => "elevated",
            Level::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemState {
    Ok,
    Soft,
    Hard,
}

impl MemState {
    pub fn as_str(self) -> &'static str {
        match self {
            MemState::Ok => "ok",
            MemState::Soft => "soft",
            MemState::Hard => "hard",
        }
    }
}

/// What the hub tells the policy about one live session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub task_id: String,
    pub root: Option<Proc>,
    /// No streaming turn, no running tool, no background work.
    pub idle: bool,
    pub pending_permission: bool,
    /// When it last became idle (`None` while busy).
    pub idle_since: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionMem {
    pub bytes: u64,
    pub peak: u64,
    pub budget: u64,
    pub state: MemState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A session entered `Soft` or `Hard`.
    Warn {
        task_id: String,
        bytes: u64,
        budget: u64,
        state: MemState,
    },
    /// Pause (true) or resume (false) new dispatches and queued deliveries.
    Pause(bool),
    /// Ask the user whether to hibernate this session.
    OfferHibernate {
        task_id: String,
        reason: String,
    },
    /// Hibernate this idle session now (sustained critical pressure).
    AutoHibernate {
        task_id: String,
        reason: String,
    },
    /// An idle session keeps growing.
    Leak {
        task_id: String,
        mb_per_min: u64,
    },
    /// Configured idle hibernation is due.
    IdleHibernate {
        task_id: String,
    },
    LevelChanged {
        from: Level,
        to: Level,
    },
    /// An automatic hibernation was due but this session may not be touched.
    Refused {
        task_id: String,
        reason: String,
    },
}

struct Track {
    mem: SessionMem,
    /// (time, bytes) while idle, newest last, for the leak detector.
    history: VecDeque<(Instant, u64)>,
    leak_reported: bool,
    /// The `idle_since` an idle hibernation was already asked for.
    idle_reported: Option<Instant>,
}

pub struct Policy {
    cfg: Config,
    level: Level,
    calm: u32,
    last_free: Option<u64>,
    critical_since: Option<Instant>,
    last_offer: Option<Instant>,
    last_action: Option<Instant>,
    last_refusal: Option<Instant>,
    sessions: HashMap<String, Track>,
}

/// The kernel's own level wins when there is one (`kernel_signal`): on macOS swap stays
/// allocated and free pages stay near zero (file cache) on a machine under no pressure —
/// measured on the founder's Mac: 90 % swap, 2 % "free", kernel level normal. Without it
/// (Linux without PSI), swap counts only together with little free memory: over
/// `elevated_pct` with under 10 % free is `Elevated`; over `critical_pct` with under 5 %
/// free and falling is `Critical`.
fn classify(s: &SystemSample, free_falling: bool, elevated_pct: u64, critical_pct: u64) -> Level {
    match s.pressure {
        Pressure::Critical => return Level::Critical,
        Pressure::Warn => return Level::Elevated,
        Pressure::Normal if s.kernel_signal => return Level::Normal,
        Pressure::Normal => {}
    }
    let swap = s.swap_pct();
    let free_pct = s.free.saturating_mul(100).checked_div(s.total).unwrap_or(100);
    let swapping = |pct: u64| s.swap_total > 0 && swap > pct;
    if swapping(critical_pct) && free_falling && free_pct < 5 {
        Level::Critical
    } else if swapping(elevated_pct) && free_pct < 10 {
        Level::Elevated
    } else {
        Level::Normal
    }
}

fn due(last: Option<Instant>, now: Instant, cooldown: Duration) -> bool {
    last.is_none_or(|t| now.saturating_duration_since(t) >= cooldown)
}

impl Policy {
    pub fn new(cfg: Config) -> Policy {
        Policy {
            cfg,
            level: Level::Normal,
            calm: 0,
            last_free: None,
            critical_since: None,
            last_offer: None,
            last_action: None,
            last_refusal: None,
            sessions: HashMap::new(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn level(&self) -> Level {
        self.level
    }

    pub fn session(&self, task_id: &str) -> Option<SessionMem> {
        self.sessions.get(task_id).map(|t| t.mem)
    }

    /// When to sample next: faster under pressure.
    pub fn next_interval(&self) -> Duration {
        if self.level == Level::Normal {
            self.cfg.interval
        } else {
            self.cfg.pressure_interval
        }
    }

    /// The session ended (or was hibernated): drop what was tracked about it.
    pub fn forget(&mut self, task_id: &str) {
        self.sessions.remove(task_id);
    }

    fn update_level(&mut self, now: Instant, s: &SystemSample, out: &mut Vec<Action>) {
        let falling = self.last_free.is_some_and(|f| s.free < f);
        self.last_free = Some(s.free);
        let up = classify(s, falling, 75, 90);
        // Stepping down needs a margin below the thresholds and several calm samples.
        let down = classify(s, falling, 70, 85);
        let from = self.level;
        if up > self.level {
            self.level = up;
            self.calm = 0;
        } else if down < self.level {
            self.calm += 1;
            if self.calm >= CALM_SAMPLES {
                self.level = down;
                self.calm = 0;
            }
        } else {
            self.calm = 0;
        }
        if self.level != from {
            out.push(Action::LevelChanged { from, to: self.level });
            if from == Level::Normal {
                out.push(Action::Pause(true));
            } else if self.level == Level::Normal {
                out.push(Action::Pause(false));
            }
        }
        match self.level {
            Level::Critical => {
                self.critical_since.get_or_insert(now);
            }
            _ => self.critical_since = None,
        }
    }

    fn update_session(&mut self, now: Instant, info: &SessionInfo, sample: TreeSample, out: &mut Vec<Action>) {
        let budget = self.cfg.budget;
        let state = if sample.bytes >= budget * u64::from(self.cfg.hard_pct) / 100 {
            MemState::Hard
        } else if sample.bytes >= budget * u64::from(self.cfg.soft_pct) / 100 {
            MemState::Soft
        } else {
            MemState::Ok
        };
        let t = self.sessions.entry(info.task_id.clone()).or_insert_with(|| Track {
            mem: SessionMem { bytes: 0, peak: 0, budget, state: MemState::Ok },
            history: VecDeque::new(),
            leak_reported: false,
            idle_reported: None,
        });
        if state != t.mem.state && state != MemState::Ok {
            out.push(Action::Warn { task_id: info.task_id.clone(), bytes: sample.bytes, budget, state });
        }
        t.mem = SessionMem { bytes: sample.bytes, peak: t.mem.peak.max(sample.bytes), budget, state };
        // Leak detector: growth while idle only (a working agent may legitimately grow).
        if !info.idle {
            t.history.clear();
            t.leak_reported = false;
        } else {
            t.history.push_back((now, sample.bytes));
            while t.history.front().is_some_and(|(at, _)| now.saturating_duration_since(*at) > self.cfg.leak_window) {
                t.history.pop_front();
            }
            let (first_at, first) = *t.history.front().expect("just pushed");
            let span = now.saturating_duration_since(first_at);
            if span >= self.cfg.leak_window / 2 && span > Duration::ZERO {
                let grown = sample.bytes.saturating_sub(first);
                let per_min = grown * 60 / span.as_secs().max(1) / MIB;
                if per_min > self.cfg.leak_mb_per_min {
                    if !t.leak_reported {
                        t.leak_reported = true;
                        out.push(Action::Leak { task_id: info.task_id.clone(), mb_per_min: per_min });
                        out.push(Action::OfferHibernate {
                            task_id: info.task_id.clone(),
                            reason: format!("idle but growing {per_min} MB/min"),
                        });
                    }
                } else {
                    t.leak_reported = false;
                }
            }
        }
        // Configured idle hibernation, once per idle period.
        if let (Some(after), Some(since), true) = (self.cfg.idle_hibernate, info.idle_since, info.idle) {
            if !info.pending_permission
                && now.saturating_duration_since(since) >= after
                && t.idle_reported != Some(since)
            {
                t.idle_reported = Some(since);
                out.push(Action::IdleHibernate { task_id: info.task_id.clone() });
            }
        }
    }

    /// One sample of the machine and of every live session; returns what to do.
    pub fn tick(
        &mut self,
        now: Instant,
        system: &SystemSample,
        sessions: &[(SessionInfo, Option<TreeSample>)],
    ) -> Vec<Action> {
        let mut out = Vec::new();
        self.update_level(now, system, &mut out);
        for (info, sample) in sessions {
            if let Some(sample) = sample {
                self.update_session(now, info, *sample, &mut out);
            }
        }
        // Candidates, largest first.
        let mut measured: Vec<(&SessionInfo, u64)> =
            sessions.iter().filter_map(|(i, s)| s.map(|s| (i, s.bytes))).collect();
        measured.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.task_id.cmp(&b.0.task_id)));
        let victim = measured.iter().find(|(i, _)| i.idle && !i.pending_permission).map(|(i, _)| i.task_id.clone());
        let sustained =
            self.critical_since.is_some_and(|t| now.saturating_duration_since(t) >= self.cfg.critical_sustain);
        let mut acted = false;
        if sustained && due(self.last_action, now, self.cfg.cooldown) {
            match &victim {
                Some(task_id) => {
                    self.last_action = Some(now);
                    acted = true;
                    out.push(Action::AutoHibernate {
                        task_id: task_id.clone(),
                        reason: format!("critical memory pressure for {} s", self.cfg.critical_sustain.as_secs()),
                    });
                }
                None if due(self.last_refusal, now, self.cfg.cooldown) => {
                    let pending = measured.iter().find(|(i, _)| i.idle && i.pending_permission);
                    let refusal = match pending {
                        Some((i, _)) => Some((
                            i.task_id.clone(),
                            "a permission request is pending: never hibernated automatically".to_string(),
                        )),
                        None => measured.first().map(|(i, _)| {
                            (
                                i.task_id.clone(),
                                "it is working (turn, tool or background work): never hibernated automatically"
                                    .to_string(),
                            )
                        }),
                    };
                    if let Some((task_id, reason)) = refusal {
                        self.last_refusal = Some(now);
                        out.push(Action::Refused { task_id, reason });
                    }
                }
                None => {}
            }
        }
        if !acted && self.level != Level::Normal && due(self.last_offer, now, self.cfg.cooldown) {
            if let Some(task_id) = victim {
                self.last_offer = Some(now);
                out.push(Action::OfferHibernate {
                    task_id,
                    reason: format!("memory pressure {}", self.level.as_str()),
                });
            }
        }
        out
    }
}
```

Add above the test module in `crates/mirante-daemon/src/memguard/orphans.rs`:
```rust
//! Orphan sweep: processes that outlived their agent. An agent's child that was re-parented
//! to init/launchd (ppid 1), runs as us, and whose working directory or command line points
//! into Mirante's worktree root is almost certainly left over from a session (a dev server, a
//! watcher). They are only REPORTED (`mirante doctor`); killing one is the user's explicit
//! choice, and even then only that process tree, re-verified by start time. Nothing is ever
//! matched or killed by process name.
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::driver::proctree::{alive, kill_tree, proc_of, KillReport, Proc, Signaler};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    pub proc: Proc,
    pub ppid: i32,
    pub cwd: Option<PathBuf>,
    pub argv: String,
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::{c_void, CStr};
    use std::mem::{size_of, zeroed};
    use std::path::PathBuf;

    pub fn all_pids() -> Vec<i32> {
        let mut cap = 4096usize;
        loop {
            let mut buf = vec![0 as libc::pid_t; cap];
            let bytes = (cap * size_of::<libc::pid_t>()) as libc::c_int;
            // SAFETY: the buffer holds `cap` pids and its size in bytes is passed.
            let n = unsafe { libc::proc_listallpids(buf.as_mut_ptr() as *mut c_void, bytes) };
            if n < 0 {
                return Vec::new();
            }
            if (n as usize) < cap {
                buf.truncate(n as usize);
                buf.retain(|p| *p > 0);
                return buf;
            }
            cap *= 4;
        }
    }

    /// (ppid, uid) of a process.
    pub fn owner(pid: i32) -> Option<(i32, u32)> {
        // SAFETY: plain C struct of the size passed.
        let mut info: libc::proc_bsdinfo = unsafe { zeroed() };
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, &mut info as *mut _ as *mut c_void, size) };
        (n == size).then_some((info.pbi_ppid as i32, info.pbi_uid))
    }

    pub fn cwd(pid: i32) -> Option<PathBuf> {
        // SAFETY: plain C struct of the size passed.
        let mut info: libc::proc_vnodepathinfo = unsafe { zeroed() };
        let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        let n = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, &mut info as *mut _ as *mut c_void, size)
        };
        if n != size {
            return None;
        }
        // SAFETY: `vip_path` is a NUL-terminated MAXPATHLEN buffer, laid out as 32x32 chars.
        let raw = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr() as *const libc::c_char) };
        let s = raw.to_str().ok()?;
        (!s.is_empty()).then(|| PathBuf::from(s))
    }

    /// The command line (`KERN_PROCARGS2`: argc, exec path, padding, then argv).
    pub fn argv(pid: i32) -> Option<String> {
        let mut max: libc::c_int = 0;
        let mut len = size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: `max` is a c_int and its size is passed.
        let r = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 2, &mut max as *mut _ as *mut c_void, &mut len, std::ptr::null_mut(), 0)
        };
        if r != 0 || max <= 0 {
            return None;
        }
        let mut buf = vec![0u8; max as usize];
        let mut len = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        // SAFETY: the buffer has `len` bytes.
        let r = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut c_void, &mut len, std::ptr::null_mut(), 0)
        };
        if r != 0 || len < 4 {
            return None;
        }
        let argc = i32::from_ne_bytes(buf[..4].try_into().ok()?).max(0) as usize;
        let mut rest = buf[4..len].split(|b| *b == 0).skip(1).skip_while(|s| s.is_empty());
        let args: Vec<String> = rest.by_ref().take(argc).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        Some(args.join(" "))
    }

    pub fn my_uid() -> u32 {
        // SAFETY: plain syscall.
        unsafe { libc::getuid() }
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use std::path::PathBuf;

    pub fn all_pids() -> Vec<i32> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok()).collect()
    }

    pub fn owner(pid: i32) -> Option<(i32, u32)> {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let field = |k: &str| status.lines().find_map(|l| l.strip_prefix(k)).and_then(|v| v.split_whitespace().next());
        Some((field("PPid:")?.parse().ok()?, field("Uid:")?.parse().ok()?))
    }

    pub fn cwd(pid: i32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    pub fn argv(pid: i32) -> Option<String> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let parts: Vec<String> =
            raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        Some(parts.join(" "))
    }

    pub fn my_uid() -> u32 {
        // SAFETY: plain syscall.
        unsafe { libc::getuid() }
    }
}

/// `root` as given and as the kernel spells it (symlinks resolved, e.g. /tmp → /private/tmp).
fn spellings(root: &Path) -> Vec<PathBuf> {
    let mut v = vec![root.to_path_buf()];
    if let Ok(c) = std::fs::canonicalize(root) {
        if c != root {
            v.push(c);
        }
    }
    v
}

/// Processes re-parented to init (ppid 1), owned by us, whose cwd or command line points
/// into `worktree_root`.
pub fn find_orphans(worktree_root: &Path) -> Vec<Orphan> {
    let roots = spellings(worktree_root);
    let me = sys::my_uid();
    let mut out = Vec::new();
    for pid in sys::all_pids() {
        let Some((ppid, uid)) = sys::owner(pid) else { continue };
        if ppid != 1 || uid != me {
            continue;
        }
        let cwd = sys::cwd(pid);
        let argv = sys::argv(pid).unwrap_or_default();
        let by_cwd = cwd.as_ref().is_some_and(|c| roots.iter().any(|r| c.starts_with(r)));
        let by_argv = roots.iter().any(|r| argv.contains(&*r.to_string_lossy()));
        if by_cwd || by_argv {
            if let Some(proc) = proc_of(pid) {
                out.push(Orphan { proc, ppid, cwd, argv });
            }
        }
    }
    out
}

/// Ends `o` and its descendants (SIGTERM, then SIGKILL after 2 s). Re-checks first that the
/// pid still names the same process (start time); a stale entry is never signaled.
pub fn kill_orphan(o: &Orphan, s: &dyn Signaler) -> KillReport {
    if !alive(o.proc) {
        return KillReport::default();
    }
    kill_tree(o.proc, Duration::from_secs(2), s)
}
```

Create `docs/dogfood/memory-stress.md`:
````markdown
# Memory guard: manual stress test

This is the only test that puts real pressure on the machine. Automated tests use a fake
sampler and never allocate on purpose.

**Before you start: save your work and close the apps you care about** (browsers, IDEs,
other agents). Pushing a Mac into critical memory pressure can make it sluggish for a
minute and macOS may kill background apps. Stop at any moment with Ctrl-C in the hog's
terminal; the memory is released at once.

## The hog

`hog.py` allocates memory in steps and touches every page (untouched pages cost nothing):

```python
# hog.py: allocate STEP_MB every 5 s up to MAX_MB, touching every page; Ctrl-C frees all.
import sys, time
max_mb = int(sys.argv[1]) if len(sys.argv) > 1 else 1024
step_mb = int(sys.argv[2]) if len(sys.argv) > 2 else 256
held = []
while sum(len(b) for b in held) < max_mb << 20:
    b = bytearray(step_mb << 20)
    for i in range(0, len(b), 4096):
        b[i] = 1
    held.append(b)
    print(f"holding {sum(len(b) for b in held) >> 20} MB", flush=True)
    time.sleep(5)
print("at the cap; Ctrl-C to free", flush=True)
time.sleep(3600)
```

Save it inside a task's worktree (so it runs in that session's process tree).

## A. Per-session budget (safe: stays below a quarter of RAM)

1. `mirante new <repo> --agent claude --branch mem-a "wait for instructions"`; keep
   `mirante watch` open in another terminal (its header shows the system pressure level).
2. In `mirante attach mem-a`, ask Claude to run `python3 hog.py 3500 256` and allow it once.
3. Expect, as it grows: `mirante ls` MEM column climbing; `memory.warn` with `soft` at 80 %
   of the budget (default min(4 GiB, 25 % of RAM)), then `hard` at 100 %. Nothing is killed:
   the session is working (a tool is running).
4. Ctrl-C the hog (Esc in attach, or `mirante interrupt mem-a`). MEM drops.

## B. System pressure (only with apps closed; stop as soon as it reacts)

1. Two idle sessions (`mem-a`, `mem-b`) plus a hog run from a plain terminal outside Mirante:
   `python3 hog.py <about 80 % of RAM in MB> 512`.
2. Expect: the `mirante watch` header goes `elevated` (the kernel pressure level reaches 2 — `sysctl kern.memorystatus_vm_pressure_level`; swap use alone does not count),
   `memory.paused` (no new dispatches or queued deliveries), and an offer to hibernate the
   largest idle session (`mirante hibernate <id>` to accept).
3. Ctrl-C the hog the moment the header shows `critical`. Only if you deliberately let
   critical last 10 s: the largest IDLE session without a pending permission is hibernated
   (`memory.auto_hibernated`), at most one per minute; `mirante wake <id>` resumes it.
4. With a permission dialog open in `mem-b`, `mem-b` is never hibernated automatically; the
   event `memory.refused` says so.

## Record

| date | scenario | peak MB | events seen | hibernated (auto / offered) | ok? |
|---|---|---|---|---|---|
| | | | | | |
````

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon`

Expected (one line per test target, in this order; each line also ends with `; finished in …`): the library has 14 more tests than after the previous task —
```
test result: ok. 178 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Run: `cargo clippy -p mirante-daemon --all-targets 2>&1 | grep -E '^(warning|error)'`

Expected: no output.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon/src/lib.rs crates/mirante-daemon/src/memguard docs/dogfood/memory-stress.md
git commit -F - <<'EOF'
feat(daemon): memory guard sampling, pressure policy and orphan sweep (not wired yet)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 19: Memory guard in the daemon and the CLI: budgets, pause, hibernate / wake, doctor

**Files:**
- Create: `crates/mirante-daemon/src/hub/memory.rs`
- Modify: `crates/mirante-daemon/src/hub/mod.rs`, `crates/mirante-daemon/src/hub/session.rs`, `crates/mirante-daemon/src/hub/messages.rs`, `crates/mirante-daemon/src/server.rs`, `crates/mirante-daemon/src/main.rs`
- Modify: `crates/mirante-ipc/src/types.rs`
- Modify: `crates/mirante-cli/src/args.rs`, `crates/mirante-cli/src/render.rs`, `crates/mirante-cli/src/main.rs`, `crates/mirante-cli/src/daemon_ctl.rs`

Task 18's policy, wired in. A guard thread (`Hub::spawn_memguard`) runs `Hub::memory_tick` every 2 s (every 500 ms under pressure): it reads the live sessions under the hub lock (root `Proc` from `Driver::root`, idle = status `Idle` + ready + no delivery starting, pending permission = the hold flag of Task 13, `idle_since`), samples the trees and the machine **without** the lock, asks the policy, and carries out its decisions:

- `Warn` → `memory.warn`; `LevelChanged` → `memory.level`; `Leak` → `memory.leak`; `OfferHibernate` → `memory.offer_hibernate` with the hint `mirante hibernate <task>` (the user confirms; nothing happens on its own).
- `Pause(true)` → new sessions are refused (`session.start` / `resume` / `wake`: CONFLICT "memory is tight") and queued messages wait (`kick` and the immediate-queue path hold them); an explicit steer still goes. `Pause(false)` → every live session is kicked again. Events `memory.paused` / `memory.resumed`.
- `AutoHibernate` (sustained critical pressure only) and `IdleHibernate` (only when `MIRANTE_IDLE_HIBERNATE_MIN` is set; off by default) → hibernate, after re-checking under the lock that the session is still idle and has no pending permission; otherwise `memory.refused` says why. **A session waiting on a permission is never hibernated, by the guard or by `mirante hibernate`.**
- **Hibernate** = record `memory.hibernated` (user) / `memory.auto_hibernated` (guard) with the session id, native id and transcript path; end the process tree (`Driver::stop`, Task 13's `kill_tree`); end the session (`mirante:hibernated`); mark the task hibernated. **Wake** (`mirante wake`) = `session.resume` of a hibernated task (Claude `--resume`, Codex `thread/resume`). The mark lives in the daemon's memory: after a daemon restart every session is ended anyway and `mirante resume` does the same.
- **Doctor** (`mirante doctor`): orphan processes (Task 18 `find_orphans` under `~/.mirante/worktrees`: parent gone, cwd or argv inside a worktree; never by name), orphan checkouts (`WorktreeManager::checkouts()` minus the tasks' worktrees; never deleted), and the memory line. `mirante doctor --kill <pid>` asks `[y/N]`, then `daemon.kill_orphan { pid, start }` re-finds the orphan with the same pid **and** start time and kills only its tree. Nothing is killed automatically.
- **Surface:** `TaskView.memory` (`bytes`, `peak`, `budget`, `state`) and `TaskView.hibernated`; `DaemonStatus.memory` (`SystemView`: level, free, swap, paused); `mirante ls` gets a `MEM` column (`1.2G/4.0G`, `!` past the soft share, `!!` past the budget) and a `hibernated: mirante wake` note; `mirante watch` shows the memory line under its header.
- **Configuration:** `MIRANTE_MEM_BUDGET_MB` (per-session budget; default min(4 GB, 25 % of RAM)) and `MIRANTE_IDLE_HIBERNATE_MIN` (idle hibernation; off unless set), read once by the daemon at start. `MIRANTE_MEMGUARD=off` disables the guard (the end-to-end tests of Task 20 set it so their results never depend on the test machine's memory).
- **Smoke-checked on the founder's Mac** (18 GB, swap 9.9 of 11 GB used, kernel level normal): `mirante doctor` printed `memory: normal · free 515M of 18.0G · swap 9.9G/11.0G` — nothing paused, as Task 18's kernel-level rule intends.

- [ ] **Step 1: Write the failing tests**

`crates/mirante-daemon/src/hub/memory.rs` (only the test module for now), and add `pub mod memory;` to `crates/mirante-daemon/src/hub/mod.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::super::testkit::Kit;
    use super::*;
    use crate::driver::proctree::{proc_of, Proc};
    use crate::driver::DriverEvent;
    use crate::memguard::{FakeSampler, Pressure};
    use mirante_core::model::AgentKind;
    use std::time::Duration;

    const MB: u64 = 1 << 20;

    fn sys(pressure: Pressure) -> SystemSample {
        SystemSample {
            total: 16 * GIB,
            free: 4 * GIB,
            swap_used: 0,
            swap_total: 4 * GIB,
            pressure,
            kernel_signal: true,
        }
    }

    fn cfg() -> Config {
        let mut c = Config::for_ram(16 * GIB);
        c.budget = 1000 * MB;
        c
    }

    fn kinds(k: &Kit, task: Option<&str>) -> Vec<String> {
        let st = k.hub.lock();
        st.store
            .events_since(0, 10_000)
            .unwrap()
            .into_iter()
            .filter(|e| task.is_none() || e.task_id.as_deref() == task)
            .map(|e| e.kind)
            .collect()
    }

    /// A started session whose fake driver reports `pid` as its root.
    fn with_root(k: &Kit, branch: &str, pid: i32) -> String {
        *k.factory.driver.root.lock().unwrap() = Some(Proc { pid, start: 1 });
        k.started(AgentKind::Claude, branch).task.id
    }

    #[test]
    fn samples_show_in_the_view_and_soft_and_hard_warn_once() {
        let k = Kit::new();
        let t = with_root(&k, "a", 101);
        let s = FakeSampler::default();
        s.set_system(sys(Pressure::Normal));
        let mut p = Policy::new(cfg());
        let now = Instant::now();
        s.set_tree(101, 500 * MB);
        k.hub.memory_tick(&mut p, &s, now);
        let v = k.hub.get_task(&t).unwrap();
        assert_eq!(v.memory, Some(MemView { bytes: 500 * MB, peak: 500 * MB, budget: 1000 * MB, state: "ok".into() }));
        s.set_tree(101, 900 * MB);
        k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(2));
        s.set_tree(101, 950 * MB);
        k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(4));
        assert_eq!(kinds(&k, Some(&t)).iter().filter(|k| *k == "memory.warn").count(), 1);
        assert_eq!(k.hub.get_task(&t).unwrap().memory.unwrap().state, "soft");
        assert_eq!(k.hub.status().memory.unwrap().level, "normal");
    }

    #[test]
    fn pressure_pauses_queued_deliveries_and_new_sessions_until_it_eases() {
        let k = Kit::new();
        let t = with_root(&k, "a", 101);
        let s = FakeSampler::default();
        s.set_tree(101, 100 * MB);
        let mut p = Policy::new(cfg());
        let now = Instant::now();
        s.set_system(sys(Pressure::Warn));
        k.hub.memory_tick(&mut p, &s, now);
        assert!(k.hub.status().memory.unwrap().paused);
        // Queued while paused: held even though the agent is idle.
        let m = mirante_ipc::types::SendMessage { task: t.clone(), text: "next".into(), mode: SendMode::Queue };
        assert_eq!(k.hub.send(m).unwrap().delivered, SendMode::Queue);
        assert!(k.calls().iter().all(|c| !c.starts_with("send:")));
        let other = k.create(AgentKind::Claude, "b").task.id;
        let r = k.hub.start_session(StartSession { task: other, prompt: None }, false);
        assert!(matches!(r, Err(HubError::Conflict(ref m)) if m.contains("memory")), "{r:?}");
        assert!(kinds(&k, None).contains(&"memory.offer_hibernate".to_string()));
        s.set_system(sys(Pressure::Normal));
        for i in 1..=5 {
            k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(2 * i));
        }
        assert!(!k.hub.status().memory.unwrap().paused);
        assert!(k.calls().contains(&"send:next".to_string()), "{:?}", k.calls());
    }

    #[test]
    fn sustained_critical_pressure_hibernates_an_idle_session_but_never_one_waiting_on_you() {
        let k = Kit::new();
        let t = with_root(&k, "a", 101);
        k.hook(&t, json!({"hook_event_name": "UserPromptSubmit"}));
        k.hook(&t, json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}));
        let s = FakeSampler::default();
        s.set_tree(101, 800 * MB);
        s.set_system(sys(Pressure::Critical));
        let mut p = Policy::new(cfg());
        let now = Instant::now();
        for i in 0..20 {
            k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(i));
        }
        assert!(k.hub.get_task(&t).unwrap().live, "a session with a pending permission is never hibernated");
        assert!(!kinds(&k, Some(&t)).contains(&"memory.auto_hibernated".to_string()));
        assert!(kinds(&k, Some(&t)).contains(&"memory.refused".to_string()));
        // Answered and idle again: now it may go.
        let id = k.pending(&t)[0].id.clone();
        k.hub.answer_permission(mirante_ipc::types::AnswerPermission { id, allow: false }).unwrap();
        k.hook(&t, json!({"hook_event_name": "Stop"}));
        for i in 20..120 {
            k.hub.memory_tick(&mut p, &s, now + Duration::from_secs(i));
        }
        let v = k.hub.get_task(&t).unwrap();
        assert!(v.hibernated && !v.live, "{v:?}");
        assert_eq!(kinds(&k, Some(&t)).iter().filter(|k| *k == "memory.auto_hibernated").count(), 1);
        assert!(k.calls().contains(&"stop".to_string()));
    }

    #[test]
    fn hibernate_and_wake_round_trip_on_the_native_conversation() {
        let k = Kit::new();
        let t = with_root(&k, "a", 101);
        k.emit(DriverEvent::NativeId("nat-7".into()));
        assert!(matches!(k.hub.wake(&t), Err(HubError::Conflict(_))));
        let v = k.hub.hibernate(&t).unwrap();
        assert!(v.hibernated && !v.live);
        let r = k.hub.wake(&t).unwrap();
        assert!(r.resumed);
        assert_eq!(k.factory.resumes.lock().unwrap().last().cloned().flatten().as_deref(), Some("nat-7"));
        assert!(!k.hub.get_task(&t).unwrap().hibernated);
        assert!(matches!(k.hub.hibernate("nope"), Err(HubError::NotFound(_))));
    }

    #[test]
    fn a_real_process_tree_is_measured() {
        use std::os::unix::process::CommandExt;
        let k = Kit::new();
        let mut child =
            std::process::Command::new("/bin/sh").args(["-c", "sleep 30 & sleep 30"]).process_group(0).spawn().unwrap();
        let root = proc_of(child.id() as i32).unwrap();
        *k.factory.driver.root.lock().unwrap() = Some(root);
        let t = k.started(AgentKind::Claude, "real").task.id;
        let sampler = memguard::os_sampler();
        let mut p = Policy::new(cfg());
        k.hub.memory_tick(&mut p, sampler.as_ref(), Instant::now());
        let m = k.hub.get_task(&t).unwrap().memory.expect("sampled");
        assert!(m.bytes > 0 && m.bytes < 1000 * MB, "{m:?}");
        let r = crate::driver::proctree::kill_tree(root, Duration::from_secs(2), &Kill);
        assert!(r.survivors.is_empty());
        let _ = child.wait();
    }

    #[test]
    fn doctor_reports_orphan_checkouts_and_refuses_to_kill_a_non_orphan() {
        let k = Kit::new();
        k.create(AgentKind::Claude, "owned");
        let stray = k.hub.paths().worktrees().join("app/stray");
        std::fs::create_dir_all(stray.join(".git")).unwrap();
        let d = k.hub.doctor().unwrap();
        assert_eq!(d.orphan_worktrees.len(), 1, "{d:?}");
        assert!(d.orphan_worktrees[0].ends_with("app/stray"));
        let me = proc_of(std::process::id() as i32).unwrap();
        let r = k.hub.kill_orphan(KillOrphan { pid: me.pid, start: me.start });
        assert!(matches!(r, Err(HubError::NotFound(_))), "{r:?}");
    }

    #[test]
    fn config_reads_the_users_overrides() {
        let c = config_from(16 * GIB, Some("2048"), Some("60"));
        assert_eq!((c.budget, c.idle_hibernate), (2048 * MB, Some(Duration::from_secs(3600))));
        let d = config_from(16 * GIB, Some("x"), None);
        assert_eq!((d.budget, d.idle_hibernate), (4 * GIB, None));
        assert_eq!(human(1536 * MB), "1.5G");
        assert_eq!(human(512 * MB), "512M");
    }

    use mirante_ipc::types::SendMode;
}
```

In `crates/mirante-ipc/src/types.rs`, the `TaskView` literal in `task_view_flattens_the_task` gets the two new fields, and `additive_fields_are_optional_on_the_wire` checks that an older `daemon.status` reply still parses:
```rust
            usage: None,
            memory: Some(MemView { bytes: 1, peak: 2, budget: 3, state: "ok".into() }),
            hibernated: false,
        };
```
```rust
        let d: DaemonStatus =
            serde_json::from_value(json!({"pid": 1, "boot_id": "b", "version": "0.1.0", "live_sessions": 0})).unwrap();
        assert_eq!(d.memory, None);
```

In `crates/mirante-cli/src/args.rs`, add to `simple_commands`:
```rust
        assert_eq!(p("hibernate feat").unwrap(), Cmd::Hibernate { task: "feat".into() });
        assert_eq!(p("wake feat --trust").unwrap(), Cmd::Wake { task: "feat".into(), trust: true });
        assert_eq!(p("doctor").unwrap(), Cmd::Doctor { kill: None });
        assert_eq!(p("doctor --kill 4242").unwrap(), Cmd::Doctor { kill: Some(4242) });
        assert_eq!(p("doctor --kill x").unwrap_err(), "--kill needs a pid, got x");
```

Replace the test module of `crates/mirante-cli/src/render.rs` with:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{PermissionStatus, Task};

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
            usage: None,
            memory: None,
            hibernated: false,
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
        assert!(lines[0].contains("MODE") && lines[1].contains("manual"));
        assert!(!t.contains('\x1b'));
        assert!(tasks_table(&[view(Status::Idle, 0, None)], true).contains("\x1b[32m"));
    }

    #[test]
    fn codex_rows_show_the_auto_edits_mode_and_usage() {
        let mut v = view(Status::Working, 0, Some("turn done, 1 command still finishing"));
        v.task.agent = AgentKind::Codex;
        v.usage = Some("5h 42% · 31k tokens".into());
        let t = tasks_table(&[v.clone()], false);
        assert!(t.contains("auto-edits"), "{t}");
        assert!(t.contains("[turn done, 1 command still finishing · 5h 42% · 31k tokens]"), "{t}");
        assert!(task_detail(&v, &[]).contains("mode:      auto-edits (edits and commands inside the worktree"));
        v.task.lifecycle = Lifecycle::Archiving;
        assert!(tasks_table(&[v], false).contains("[archiving · 5h 42% · 31k tokens]"));
    }

    #[test]
    fn memory_shows_per_task_in_watch_and_in_doctor() {
        let mut v = view(Status::Idle, 0, None);
        v.memory = Some(MemView { bytes: 3 << 30, peak: 3 << 30, budget: 4 << 30, state: "soft".into() });
        let t = tasks_table(&[v.clone()], false);
        assert!(t.lines().next().unwrap().contains("MEM") && t.contains("3.0G/4.0G!"), "{t}");
        v.memory = None;
        v.hibernated = true;
        v.live = false;
        assert!(tasks_table(&[v], false).contains("[hibernated: mirante wake]"));
        let s = SystemView {
            level: "elevated".into(),
            total: 16 << 30,
            free: 512 << 20,
            swap_used: 9 << 30,
            swap_total: 10 << 30,
            paused: true,
        };
        assert_eq!(
            memory_line(&s),
            "memory: elevated · free 512M of 16.0G · swap 9.0G/10.0G · new sessions and queued messages paused"
        );
        let d = DoctorReport {
            orphan_processes: vec![mirante_ipc::types::OrphanView {
                pid: 4242,
                start: 1,
                cwd: Some("/w/app/feat".into()),
                argv: "node server.js".into(),
            }],
            orphan_worktrees: vec![],
            memory: Some(s),
        };
        let r = doctor_report(&d);
        assert!(r.contains("pid 4242  cwd /w/app/feat  node server.js") && r.contains("mirante doctor --kill <pid>"));
        assert!(r.contains("orphan checkouts: none"));
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

In `crates/mirante-cli/src/daemon_ctl.rs`, add `memory: None,` to the `DaemonStatus` literal in `version_skew_warns_and_offers_a_restart_only_when_idle`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mirante-daemon`

Expected: FAIL to compile; the first error is
```
error: cannot find macro `json` in this scope
```

- [ ] **Step 3: Implement**

Add above the test module in `crates/mirante-daemon/src/hub/memory.rs`:
```rust
//! The memory guard in the daemon: a thread samples every live session's process tree and
//! the machine (`crate::memguard`), feeds the policy, and carries out what it decides.
//! Nothing is limited up front (macOS cannot enforce RLIMIT_AS/RLIMIT_DATA): Mirante
//! measures and acts, in this order — warn; pause new sessions and queued deliveries; offer
//! to hibernate the largest idle session (the user confirms with `mirante hibernate`);
//! only under sustained critical pressure hibernate an idle session on its own, one per
//! cooldown, never one with a pending permission and never one that is working. Every
//! step is an `memory.*` event.
//!
//! Hibernate = remember the native id and transcript, end the process tree, mark the task
//! hibernated; `mirante wake` resumes it natively (`--resume` / `thread/resume`).
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use mirante_core::model::Status;
use mirante_ipc::types::{
    BootState, DoctorReport, KillOrphan, MemView, OrphanView, StartResult, StartSession, SystemView, TaskView,
};
use serde_json::json;

use super::{Hub, HubError, Result, State};
use crate::driver::proctree::Kill;
use crate::memguard::{self, Action, Config, MemSampler, Policy, SessionInfo, SessionMem, SystemSample, TreeSample};

const GIB: u64 = 1 << 30;

/// How the hub shows memory between samples.
#[derive(Debug, Default)]
pub(crate) struct MemShared {
    pub(crate) system: Option<SystemView>,
    /// New sessions and queued deliveries wait (the policy paused them).
    pub(crate) paused: bool,
    pub(crate) sessions: std::collections::HashMap<String, SessionMem>,
    /// Tasks hibernated by this daemon (a restart ends every session anyway).
    pub(crate) hibernated: std::collections::HashSet<String>,
}

pub(crate) fn mem_view(m: &SessionMem) -> MemView {
    MemView { bytes: m.bytes, peak: m.peak, budget: m.budget, state: m.state.as_str().to_string() }
}

/// The guard's configuration for a machine with `total` bytes of RAM, with the user's
/// overrides: `MIRANTE_MEM_BUDGET_MB` (per session) and `MIRANTE_IDLE_HIBERNATE_MIN`
/// (hibernate a session idle that long; off unless set). Unparseable values are ignored.
pub fn config_from(total: u64, budget_mb: Option<&str>, idle_min: Option<&str>) -> Config {
    let mut c = Config::for_ram(total);
    if let Some(mb) = budget_mb.and_then(|s| s.trim().parse::<u64>().ok()).filter(|mb| *mb > 0) {
        c.budget = mb << 20;
    }
    if let Some(min) = idle_min.and_then(|s| s.trim().parse::<u64>().ok()).filter(|m| *m > 0) {
        c.idle_hibernate = Some(std::time::Duration::from_secs(min * 60));
    }
    c
}

fn system_view(level: &str, s: &SystemSample, paused: bool) -> SystemView {
    SystemView {
        level: level.to_string(),
        total: s.total,
        free: s.free,
        swap_used: s.swap_used,
        swap_total: s.swap_total,
        paused,
    }
}

fn orphan_view(o: &memguard::Orphan) -> OrphanView {
    OrphanView {
        pid: o.proc.pid,
        start: o.proc.start,
        cwd: o.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        argv: o.argv.clone(),
    }
}

impl State {
    /// Everything the policy needs to know about the live sessions now.
    fn mem_sessions(&self) -> Vec<SessionInfo> {
        let mut out = Vec::new();
        for (task_id, l) in &self.live {
            let status = self.store.get_task(task_id).map(|t| t.status).unwrap_or(Status::Working);
            let pending = l.hold.load(std::sync::atomic::Ordering::SeqCst);
            out.push(SessionInfo {
                task_id: task_id.clone(),
                root: l.driver.as_ref().and_then(|d| d.root()),
                idle: status == Status::Idle && l.boot == BootState::Ready && !l.delivering(),
                pending_permission: pending,
                idle_since: l.idle_since,
            });
        }
        out
    }

    /// Carries out one policy decision that needs no process to end; hibernations are
    /// returned for the caller to do without the lock.
    fn apply_mem_action(&mut self, a: Action, hibernate: &mut Vec<(String, String)>) -> Result<()> {
        match a {
            Action::Warn { task_id, bytes, budget, state } => self.event(
                Some(&task_id),
                "memory.warn",
                json!({ "bytes": bytes, "budget": budget, "state": state.as_str() }),
            ),
            Action::Pause(paused) => {
                self.mem.paused = paused;
                if let Some(v) = self.mem.system.as_mut() {
                    v.paused = paused;
                }
                let kind = if paused { "memory.paused" } else { "memory.resumed" };
                self.event(None, kind, json!({ "what": "new sessions and queued deliveries" }))?;
                if !paused {
                    let tasks: Vec<String> = self.live.keys().cloned().collect();
                    for t in tasks {
                        self.kick(&t)?;
                    }
                }
                Ok(())
            }
            Action::OfferHibernate { task_id, reason } => self.event(
                Some(&task_id),
                "memory.offer_hibernate",
                json!({ "reason": reason, "hint": "mirante hibernate <task> (mirante wake <task> resumes it)" }),
            ),
            Action::AutoHibernate { task_id, reason } => {
                hibernate.push((task_id, reason));
                Ok(())
            }
            Action::IdleHibernate { task_id } => {
                hibernate.push((task_id, "idle hibernation".into()));
                Ok(())
            }
            Action::Leak { task_id, mb_per_min } => {
                self.event(Some(&task_id), "memory.leak", json!({ "mb_per_min": mb_per_min }))
            }
            Action::LevelChanged { from, to } => {
                self.event(None, "memory.level", json!({ "from": from.as_str(), "to": to.as_str() }))
            }
            Action::Refused { task_id, reason } => {
                self.event(Some(&task_id), "memory.refused", json!({ "reason": reason }))
            }
        }
    }

    /// Checkouts under the worktree root that no non-archived task owns.
    fn orphan_checkouts(&self, all: Vec<PathBuf>) -> Result<Vec<String>> {
        let owned: Vec<PathBuf> = self
            .store
            .list_tasks(false)?
            .into_iter()
            .map(|t| std::fs::canonicalize(&t.worktree_path).unwrap_or_else(|_| PathBuf::from(&t.worktree_path)))
            .collect();
        let mut out: Vec<String> =
            all.into_iter().filter(|p| !owned.contains(p)).map(|p| p.to_string_lossy().into_owned()).collect();
        out.sort();
        Ok(out)
    }
}

impl Hub {
    /// One round of the guard: sample (without the hub lock), decide, act.
    pub fn memory_tick(&self, policy: &mut Policy, sampler: &dyn MemSampler, now: Instant) {
        let infos = self.lock().mem_sessions();
        let system = sampler.system();
        let sessions: Vec<(SessionInfo, Option<TreeSample>)> = infos
            .into_iter()
            .map(|i| {
                let s = i.root.and_then(|r| sampler.tree(r));
                (i, s)
            })
            .collect();
        let actions = policy.tick(now, &system, &sessions);
        let mut hibernate = Vec::new();
        {
            let mut st = self.lock();
            let live: Vec<String> = st.live.keys().cloned().collect();
            st.mem.sessions.retain(|t, _| live.contains(t));
            for (info, _) in &sessions {
                if let Some(m) = policy.session(&info.task_id) {
                    st.mem.sessions.insert(info.task_id.clone(), m);
                }
            }
            let paused = st.mem.paused;
            st.mem.system = Some(system_view(policy.level().as_str(), &system, paused));
            for a in actions {
                if let Err(e) = st.apply_mem_action(a, &mut hibernate) {
                    st.log_error(None, "memory guard", &e);
                }
            }
            st.publish();
        }
        for (task_id, reason) in hibernate {
            policy.forget(&task_id);
            if let Err(e) = self.hibernate_with(&task_id, Some(&reason)) {
                let mut st = self.lock();
                let _ = st.event(Some(&task_id), "memory.refused", json!({ "reason": e.to_string() }));
                st.publish();
            }
        }
    }

    /// The guard thread: [`Hub::memory_tick`] every 2 s (every 500 ms under pressure).
    pub fn spawn_memguard(self: &Arc<Self>, sampler: Box<dyn MemSampler>, cfg: Config) -> std::thread::JoinHandle<()> {
        let hub = Arc::clone(self);
        std::thread::spawn(move || {
            let mut policy = Policy::new(cfg);
            loop {
                hub.memory_tick(&mut policy, sampler.as_ref(), Instant::now());
                std::thread::sleep(policy.next_interval());
            }
        })
    }

    /// The user's `mirante hibernate`: ends the live session to free memory, keeping it
    /// resumable. Refused while a permission request waits (it would be lost).
    pub fn hibernate(&self, reference: &str) -> Result<TaskView> {
        self.hibernate_with(reference, None)
    }

    /// `auto` is the guard's reason: then the session must also be idle right now.
    fn hibernate_with(&self, reference: &str, auto: Option<&str>) -> Result<TaskView> {
        let (task_id, session_id, driver) = {
            let mut st = self.lock();
            let task = st.resolve(reference)?;
            let infos = st.mem_sessions();
            let l = st
                .live
                .get(&task.id)
                .ok_or_else(|| HubError::Conflict(format!("task {} has no live session", task.branch)))?;
            let info = infos.iter().find(|i| i.task_id == task.id);
            if info.is_some_and(|i| i.pending_permission) {
                return Err(HubError::Conflict(format!(
                    "task {} waits on a permission request: answer it first (a session waiting on you is never hibernated)",
                    task.branch
                )));
            }
            if auto.is_some() && !info.is_some_and(|i| i.idle) {
                return Err(HubError::Conflict(format!("task {} is busy: not hibernated", task.branch)));
            }
            let bytes = st.mem.sessions.get(&task.id).map(|m| m.bytes);
            let payload = json!({
                "session_id": l.session_id,
                "native_id": l.native_id,
                "transcript_path": l.transcript_path,
                "bytes": bytes,
                "auto": auto.is_some(),
                "reason": auto.unwrap_or("asked by the user"),
            });
            let out = (task.id.clone(), l.session_id.clone(), l.driver.clone());
            let kind = if auto.is_some() { "memory.auto_hibernated" } else { "memory.hibernated" };
            st.event(Some(&task.id), kind, payload)?;
            st.publish();
            out
        };
        if let Some(d) = driver {
            d.stop();
        }
        let mut st = self.lock();
        st.finish_session(&task_id, &session_id, "mirante:hibernated")?;
        st.mem.hibernated.insert(task_id.clone());
        st.mem.sessions.remove(&task_id);
        st.publish();
        let t = st.store.get_task(&task_id)?;
        st.view(t)
    }

    /// `mirante wake`: resumes a hibernated session on its native conversation.
    pub fn wake(&self, reference: &str) -> Result<StartResult> {
        let task_id = {
            let st = self.lock();
            let task = st.resolve(reference)?;
            if !st.mem.hibernated.contains(&task.id) {
                return Err(HubError::Conflict(format!(
                    "task {} is not hibernated (mirante resume {} restarts an ended session)",
                    task.branch, task.branch
                )));
            }
            task.id
        };
        self.start_session(StartSession { task: task_id, prompt: None }, true)
    }

    /// Orphan processes (parent gone, inside Mirante's worktrees), orphan checkouts, and
    /// memory. Nothing is killed or deleted here.
    pub fn doctor(&self) -> Result<DoctorReport> {
        let processes = memguard::find_orphans(&self.paths.worktrees());
        let checkouts = self.worktrees.checkouts();
        let st = self.lock();
        Ok(DoctorReport {
            orphan_processes: processes.iter().map(orphan_view).collect(),
            orphan_worktrees: st.orphan_checkouts(checkouts)?,
            memory: st.mem.system.clone(),
        })
    }

    /// Kills one orphan the user picked: it must still be an orphan of Mirante's worktrees
    /// with the same pid AND start time (never a reused pid, never by name).
    pub fn kill_orphan(&self, k: KillOrphan) -> Result<OrphanView> {
        let found = memguard::find_orphans(&self.paths.worktrees())
            .into_iter()
            .find(|o| o.proc.pid == k.pid && o.proc.start == k.start)
            .ok_or_else(|| {
                HubError::NotFound(format!("pid {} is not (or no longer) an orphan in Mirante's worktrees", k.pid))
            })?;
        let report = memguard::kill_orphan(&found, &Kill);
        let mut st = self.lock();
        let v = orphan_view(&found);
        st.event(None, "orphan_killed", json!({ "pid": v.pid, "argv": v.argv, "survivors": report.survivors.len() }))?;
        st.publish();
        if report.survivors.is_empty() {
            Ok(v)
        } else {
            Err(HubError::Conflict(format!("pid {} survived SIGKILL", k.pid)))
        }
    }
}

/// Budget shown for `n` bytes: "1.2G", "512M".
pub fn human(n: u64) -> String {
    if n >= GIB {
        format!("{:.1}G", n as f64 / GIB as f64)
    } else {
        format!("{}M", n >> 20)
    }
}
```

In `crates/mirante-daemon/src/hub/mod.rs`:

1. Add a field at the end of `struct Live` (and `idle_since: Some(Instant::now()),` in `Live::new`):
```rust
    /// Since when the status is `Idle` (the memory guard's idle hibernation and leak check).
    pub(crate) idle_since: Option<Instant>,
```
2. Add a field to `struct State` (and `mem: memory::MemShared::default(),` in `Hub::new`):
```rust
    /// The memory guard's last view, pause flag and hibernated tasks.
    pub(crate) mem: memory::MemShared,
```
3. In `State::view`, after `usage`, add:
```rust
            memory: live.and(self.mem.sessions.get(&task.id)).map(memory::mem_view),
            hibernated: self.mem.hibernated.contains(&task.id),
```
4. In `State::set_status`, replace the `if cur == Status::Idle { … }` block after `self.store.set_status(…)?;` with:
```rust
        if let Some(l) = self.live.get_mut(task_id) {
            if cur == Status::Idle {
                l.delivery_in_flight = None;
            }
            l.idle_since = (to == Status::Idle).then(Instant::now);
        }
```
5. In `Hub::status`, add `memory: st.mem.system.clone(),` after `live_sessions`.

In `crates/mirante-daemon/src/hub/session.rs`, in `State::kick` right after the early return for a session that is not ready/idle/queued:
```rust
        if self.mem.paused {
            // Memory is tight: queued messages wait (the guard kicks again when it eases).
            return Ok(());
        }
```
and in `Hub::start_session`, right after the "already has a live session" check:
```rust
            if st.mem.paused {
                return Err(HubError::Conflict(
                    "memory is tight: new sessions wait until it eases (mirante watch / mirante doctor)".into(),
                ));
            }
            st.mem.hibernated.remove(&task.id);
```

In `crates/mirante-daemon/src/hub/messages.rs` (`Hub::send`), read the flag before borrowing the live session, and hold queued messages while paused:
```rust
        // Memory is tight: queued messages wait for the guard (an explicit steer still goes).
        let paused = st.mem.paused;
```
```rust
            SendMode::Queue => ready && !paused && task.status == Status::Idle && l.queue.is_empty() && !l.delivering(),
```

In `crates/mirante-daemon/src/server.rs` (`dispatch`), after the `DAEMON_STATUS` arm:
```rust
        method::SESSION_HIBERNATE => out(hub.hibernate(&params::<TaskRef>(p)?.task)),
        method::SESSION_WAKE => out(hub.wake(&params::<TaskRef>(p)?.task)),
        method::DAEMON_DOCTOR => out(hub.doctor()),
        method::DAEMON_KILL_ORPHAN => out(hub.kill_orphan(params(p)?)),
```

In `crates/mirante-daemon/src/main.rs` (`run`), right after `let pump = hub.spawn_pump(rx);`:
```rust
    // The memory guard: measure and act (nothing is limited up front; see hub::memory).
    // `MIRANTE_MEMGUARD=off` disables it (tests that must not depend on this machine's memory).
    if std::env::var("MIRANTE_MEMGUARD").as_deref() != Ok("off") {
        let sampler = mirante_daemon::memguard::os_sampler();
        let budget = std::env::var("MIRANTE_MEM_BUDGET_MB").ok();
        let idle = std::env::var("MIRANTE_IDLE_HIBERNATE_MIN").ok();
        let cfg = mirante_daemon::hub::memory::config_from(sampler.system().total, budget.as_deref(), idle.as_deref());
        eprintln!("mirante-daemon: memory budget per session {}", mirante_daemon::hub::memory::human(cfg.budget));
        hub.spawn_memguard(sampler, cfg);
    }
```

In `crates/mirante-ipc/src/types.rs`: add the four method names to `mod method`:
```rust
    /// Ends an idle session to free memory, keeping it resumable ([`super::TaskRef`]).
    pub const SESSION_HIBERNATE: &str = "session.hibernate";
    /// Resumes a hibernated session ([`super::TaskRef`]; result: [`super::StartResult`]).
    pub const SESSION_WAKE: &str = "session.wake";
    /// Orphan processes / checkouts and memory ([`super::DoctorReport`]).
    pub const DAEMON_DOCTOR: &str = "daemon.doctor";
    /// Kills one orphan the user picked from `daemon.doctor` ([`super::KillOrphan`]).
    pub const DAEMON_KILL_ORPHAN: &str = "daemon.kill_orphan";
```
add the two fields at the end of `TaskView` and the new types right after it:
```rust
    /// Memory of the live session's process tree, once sampled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemView>,
    /// The session was hibernated to free memory (`mirante wake` resumes it).
    #[serde(default)]
    pub hibernated: bool,
}

/// One live session's memory: the summed footprint of its process tree (macOS:
/// `phys_footprint`, what Activity Monitor shows), its peak, its budget and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemView {
    pub bytes: u64,
    pub peak: u64,
    pub budget: u64,
    /// "ok", "soft" (past the soft share of the budget) or "hard" (past the budget).
    pub state: String,
}

/// The machine's memory as the daemon's guard sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemView {
    /// "normal", "elevated" or "critical".
    pub level: String,
    pub total: u64,
    pub free: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    /// New sessions and queued deliveries wait while memory is tight.
    pub paused: bool,
}

/// A process left behind by an agent: parent gone (ppid 1), working directory or command
/// line inside Mirante's worktrees. Shown by `mirante doctor`; killed only on request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanView {
    pub pid: i32,
    /// Start time; with the pid it names exactly this process.
    pub start: u64,
    pub cwd: Option<String>,
    pub argv: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub orphan_processes: Vec<OrphanView>,
    /// Checkouts under the worktree root that no task owns (never deleted by Mirante).
    pub orphan_worktrees: Vec<String>,
    pub memory: Option<SystemView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillOrphan {
    pub pid: i32,
    pub start: u64,
}
```
and the field at the end of `DaemonStatus`:
```rust
    /// The memory guard's view (absent from older daemons and before the first sample).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<SystemView>,
```

In `crates/mirante-cli/src/args.rs`: add to `USAGE` (before `archive`):
```
  hibernate <task>       end an idle agent to free memory (resumable; never one waiting on you)
  wake <task> [--trust]  resume a hibernated task on its native conversation
  doctor [--kill <pid>]  orphan processes / checkouts and memory; --kill asks, then kills one orphan
```
add the variants to `Cmd`:
```rust
    Hibernate {
        task: String,
    },
    Wake {
        task: String,
        trust: bool,
    },
    Doctor {
        kill: Option<i32>,
    },
```
and parse them after `"archive"`:
```rust
        "hibernate" => Ok(Cmd::Hibernate { task: one(&split(rest, none, none)?, "task")? }),
        "wake" => {
            let p = split(rest, none, &["--trust"])?;
            Ok(Cmd::Wake { task: one(&p, "task")?, trust: !p.bools.is_empty() })
        }
        "doctor" => {
            let p = split(rest, &["--kill"], none)?;
            if !p.positional.is_empty() {
                return Err("doctor takes no arguments".into());
            }
            let kill = match p.values.get("--kill") {
                Some(v) => Some(v.parse::<i32>().map_err(|_| format!("--kill needs a pid, got {v}"))?),
                None => None,
            };
            Ok(Cmd::Doctor { kill })
        }
```

In `crates/mirante-cli/src/render.rs`, replace everything above the test module with:
```rust
//! Plain-text tables. Colors only on a terminal (`color` = stdout is a tty).
use mirante_core::model::{AgentKind, Lifecycle, PermissionRequest, Status};
use mirante_ipc::types::{BootState, DoctorReport, MemView, PermissionView, SystemView, TaskView};

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

/// The approval mode Mirante runs each agent in (fixed per agent; never a bypass mode).
pub fn mode_label(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "manual",
        AgentKind::Codex => "auto-edits",
    }
}

/// What the mode means, for `mirante show`.
pub fn mode_help(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "Claude asks before every tool that needs permission (--permission-mode manual)",
        AgentKind::Codex => {
            "edits and commands inside the worktree run without asking (sandbox workspace-write); \
             anything beyond it asks you (approvalPolicy on-request, approvalsReviewer user)"
        }
    }
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
    let state = match (v.boot, &v.detail) {
        _ if v.hibernated => Some("hibernated: mirante wake".to_string()),
        _ if v.task.lifecycle == Lifecycle::Archiving => Some("archiving".to_string()),
        (Some(BootState::TrustPrompt), _) => Some("trust prompt: mirante resume / attach".to_string()),
        (Some(BootState::Booting), _) => Some("starting".to_string()),
        (_, Some(d)) => Some(d.clone()),
        _ => None,
    };
    state.into_iter().chain(v.usage.clone()).collect::<Vec<_>>().join(" · ")
}

pub fn tasks_table(tasks: &[TaskView], color: bool) -> String {
    if tasks.is_empty() {
        return "no tasks (mirante new <repo> --agent claude|codex --branch <name> <goal>)\n".into();
    }
    let mut out = format!(
        "{}  {}  {}  {}  {}  {}  {}  {}  {}\n",
        fit("ID", 8),
        fit("BRANCH", 22),
        fit("AGENT", 6),
        fit("MODE", 10),
        fit("STATUS", 9),
        fit("MEM", 11),
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
            "{}  {}  {}  {}  {}  {}  {}  {}  {}\n",
            fit(short(&v.task.id), 8),
            fit(&v.task.branch, 22),
            fit(v.task.agent.as_str(), 6),
            fit(mode_label(v.task.agent), 10),
            paint(v.task.status, &fit(status_label(v.task.status), 9), color),
            fit(&v.memory.as_ref().map(mem_cell).unwrap_or_default(), 11),
            fit(&needs, 5),
            fit(&queued, 2),
            title
        ));
    }
    out
}

/// Bytes as "1.5G" / "512M".
pub fn bytes(n: u64) -> String {
    const GIB: u64 = 1 << 30;
    if n >= GIB {
        format!("{:.1}G", n as f64 / GIB as f64)
    } else {
        format!("{}M", n >> 20)
    }
}

/// "1.2G/4.0G", with "!" past the soft share and "!!" past the budget.
pub fn mem_cell(m: &MemView) -> String {
    let mark = match m.state.as_str() {
        "soft" => "!",
        "hard" => "!!",
        _ => "",
    };
    format!("{}/{}{mark}", bytes(m.bytes), bytes(m.budget))
}

/// The `watch` header line about the machine's memory.
pub fn memory_line(s: &SystemView) -> String {
    let paused = if s.paused { " · new sessions and queued messages paused" } else { "" };
    format!(
        "memory: {} · free {} of {} · swap {}/{}{paused}",
        s.level,
        bytes(s.free),
        bytes(s.total),
        bytes(s.swap_used),
        bytes(s.swap_total)
    )
}

pub fn doctor_report(d: &DoctorReport) -> String {
    let mut out = String::new();
    match &d.memory {
        Some(m) => out.push_str(&format!("{}\n", memory_line(m))),
        None => out.push_str("memory: not sampled yet\n"),
    }
    if d.orphan_processes.is_empty() {
        out.push_str("orphan processes: none\n");
    } else {
        out.push_str("orphan processes (parent gone, inside Mirante's worktrees):\n");
        for o in &d.orphan_processes {
            let cwd = o.cwd.as_deref().unwrap_or("?");
            out.push_str(&format!("  pid {}  cwd {}  {}\n", o.pid, cwd, o.argv));
        }
        out.push_str("kill one (asks first): mirante doctor --kill <pid>\n");
    }
    if d.orphan_worktrees.is_empty() {
        out.push_str("orphan checkouts: none\n");
    } else {
        out.push_str("orphan checkouts (no task owns them; Mirante never deletes them):\n");
        for w in &d.orphan_worktrees {
            out.push_str(&format!("  {w}\n"));
        }
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
    out.push_str(&format!("  mode:      {} ({})\n", mode_label(t.agent), mode_help(t.agent)));
    out.push_str(&format!("  live:      {}  queued: {}\n", v.live, v.queued));
    for p in perms {
        out.push_str(&format!("  needs you: {} {} {}\n", short(&p.id), p.tool, p.detail));
    }
    out
}
```

In `crates/mirante-cli/src/main.rs`: import `DoctorReport, KillOrphan, OrphanView, SystemView` from `mirante_ipc::types`; give `board` the memory line:
```rust
fn board(tasks: &[TaskView], memory: Option<&SystemView>, seen: &mut HashMap<String, Status>, color: bool) {
    // Bell only when a task newly needs the user.
    let bell = tasks
        .iter()
        .any(|v| v.task.status == Status::NeedsYou && seen.get(&v.task.id).is_some_and(|s| *s != Status::NeedsYou));
    seen.clear();
    seen.extend(tasks.iter().map(|v| (v.task.id.clone(), v.task.status)));
    print!("\x1b[2J\x1b[H");
    println!("mirante watch  (Ctrl-C to quit)");
    match memory {
        Some(m) => println!("{}\n", render::memory_line(m)),
        None => println!(),
    }
    print!("{}", render::tasks_table(tasks, color));    // … unchanged
```
in `watch`, fetch it with each snapshot (both `board(…)` calls pass `memory(&mut q).as_ref()`):
```rust
            let memory = |q: &mut Client| q.call::<DaemonStatus>(method::DAEMON_STATUS, json!({})).ok()?.memory;
            board(&tasks, memory(&mut q).as_ref(), &mut seen, color);
```
and add the commands to `run` (before `Cmd::Archive`):
```rust
        Cmd::Hibernate { task } => {
            let v: TaskView = client(&paths)?.call(method::SESSION_HIBERNATE, TaskRef { task })?;
            println!("{} hibernated (mirante wake {} resumes it)", render::short(&v.task.id), v.task.branch);
        }
        Cmd::Wake { task, trust } => {
            let mut c = client(&paths)?;
            let r: StartResult = c.call(method::SESSION_WAKE, TaskRef { task })?;
            boot_flow(&mut c, r, trust)?;
        }
        Cmd::Doctor { kill: None } => {
            let d: DoctorReport = client(&paths)?.call(method::DAEMON_DOCTOR, json!({}))?;
            print!("{}", render::doctor_report(&d));
        }
        Cmd::Doctor { kill: Some(pid) } => {
            let mut c = client(&paths)?;
            let d: DoctorReport = c.call(method::DAEMON_DOCTOR, json!({}))?;
            let o = d.orphan_processes.into_iter().find(|o| o.pid == pid).ok_or_else(|| {
                format!("pid {pid} is not an orphan in Mirante's worktrees (mirante doctor lists them)")
            })?;
            if !ask(&format!("kill pid {} ({})? [y/N] ", o.pid, o.argv))? {
                println!("left running");
                return Ok(());
            }
            let k: OrphanView = c.call(method::DAEMON_KILL_ORPHAN, KillOrphan { pid: o.pid, start: o.start })?;
            println!("killed pid {}", k.pid);
        }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mirante-daemon -p mirante-ipc -p mirante-cli`

Expected (one line per test target, in this order; each line also ends with `; finished in …`):
```
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 185 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```
No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/mirante-daemon crates/mirante-ipc crates/mirante-cli
git commit -F - <<'EOF'
feat(daemon,cli): memory guard: budgets, pause, hibernate/wake, doctor

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

## Phase 2b-v — End to end, verification, dogfood

### Task 20: End to end with a fake `claude`; real-agent integration tests

**Files:**
- Create: `crates/mirante-cli/tests/fake-claude.sh`
- Create: `crates/mirante-cli/tests/e2e.rs`
- Create: `crates/mirante-daemon/tests/real_agents.rs`

`tests/e2e.rs` drives the real binaries (`mirante` → auto-started `mirante-daemon` → PTY → `fake-claude.sh` → `mirante-hook` → daemon) in a temp home. The fake behaves like Claude Code where the driver looks: `SessionStart` hook first (with the `--session-id` it was given, or the `--resume` id), an input prompt (`❯`) and the "manual mode on" footer, bracketed-paste input (markers stripped), one turn per line; `ask…` raises a real-looking permission dialog (PermissionRequest hook + `Do you want to proceed?` / `1. Yes` / `2. No` / `Esc to cancel` on screen) and records the one key it gets; a line with `hold` opens a turn that never ends. Every test sets `MIRANTE_MEMGUARD=off` (Task 19) so the results never depend on the test machine's memory. The tests:

- **dispatch → steer → stop → resume → archive**: status from hooks, the native id pinned with `--session-id` at the first launch and reused with `--resume`, `--permission-mode manual`, never a bypass flag, `MODE manual` in `ls`, archive refused while live.
- **bad input** is reported, not swallowed.
- **concurrent answers** (D): two `mirante allow` for one card race; both succeed (the second gets the stored row, robustness round), exactly one key reaches the agent; a later `deny` is a conflict and types nothing.
- **`kill -9` of the daemon mid-turn** (D): with a dialog pending and a message queued, the daemon is SIGKILLed; the next command starts a fresh daemon whose recovery ends the session (`ended`), expires the card (`permission_expired`) and reports the queued text (`queue_dropped`); `mirante resume` comes back on the same native id.
- **slow subscriber** (D): a client subscribes and never reads while 2000 events are produced; the daemon keeps answering (`daemon.status` < 2 s), the slow stream is cut after its bounded queue, and the client resumes with `since_seq` exactly at the next event.
- **stale hook after resume** (D): a hook stamped with the previous Mirante session (`MIRANTE_SESSION_ID` of the stopped session) is logged as `hook_stale` and changes nothing.
- **hibernate / wake / doctor** from the CLI (Task 19).

`tests/real_agents.rs` holds two `#[ignore]` tests against the real Claude Code and Codex (they need your logins and spend quota): turn → permission card answered once → file created → stop → resume → the secret is remembered (Claude); start → steer "BANANA" mid-run → turn ends → stop → resume by thread id → remembered (Codex). They run in a throwaway temp repo; the Claude one answers the trust dialog only when you set `MIRANTE_TEST_TRUST=1`. (Unchanged from the first version of this plan: the hub API they use did not change.)

- [ ] **Step 1: Write the files**

`crates/mirante-cli/tests/fake-claude.sh`:
```sh
#!/bin/sh
# Fake `claude` for the end-to-end tests. Like Claude Code it reports hooks through the
# hook command (SessionStart first), shows an input prompt with the "manual mode on"
# footer, takes bracketed-paste input, and starts and ends a turn per line. A line starting
# with "ask" raises a permission dialog (PermissionRequest hook + the dialog on screen) and
# waits for one key, appended to $FAKE_CLAUDE_LOG.keys; a line containing "hold" starts a
# turn that never ends. Its arguments are appended to $FAKE_CLAUDE_LOG.
hook() { printf '%s' "$1" | "$MIRANTE_HOOK_BIN"; }
SID=none
SRC=startup
prev=
for a in "$@"; do
  case "$prev" in
    --session-id) SID=$a ;;
    --resume) SID=$a; SRC=resume ;;
  esac
  prev=$a
done
echo "$*" >> "$FAKE_CLAUDE_LOG"
# Screen: a note, the input prompt and the footer (U+23F8 and U+276F as octal UTF-8).
prompt() { printf '\033[2J\033[H%s\r\n\342\235\257 \r\n  \342\217\270 manual mode on\r\n' "$1"; }
hook "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"$SID\",\"source\":\"$SRC\"}"
prompt "fake claude $SID"
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  hook "{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"$SID\",\"prompt\":\"typed\"}"
  case "$line" in
    ask*)
      hook "{\"hook_event_name\":\"PermissionRequest\",\"session_id\":\"$SID\",\"tool_name\":\"Bash\",\"tool_use_id\":\"tu1\",\"tool_input\":{\"command\":\"touch x\"}}"
      printf '\033[2J\033[H Bash command\r\n   touch x\r\n Do you want to proceed?\r\n \342\235\257 1. Yes\r\n   2. No\r\n Esc to cancel\r\n'
      stty raw -echo
      k=$(dd bs=1 count=1 2>/dev/null)
      stty sane
      echo "$k" >> "$FAKE_CLAUDE_LOG.keys"
      if [ "$k" = 1 ]; then
        hook "{\"hook_event_name\":\"PostToolUse\",\"session_id\":\"$SID\",\"tool_name\":\"Bash\",\"tool_use_id\":\"tu1\"}"
      fi
      prompt "answered $k"
      hook "{\"hook_event_name\":\"Stop\",\"session_id\":\"$SID\"}" ;;
    *hold*)
      prompt "holding" ;;
    *)
      prompt "echo: $line"
      hook "{\"hook_event_name\":\"Stop\",\"session_id\":\"$SID\"}" ;;
  esac
done
```

`crates/mirante-cli/tests/e2e.rs`:
```rust
//! End to end through the real binaries (`mirante`, `mirante-daemon`, `mirante-hook`) with
//! a fake `claude` (tests/fake-claude.sh): dispatch, status from hooks, steer, stop, resume
//! on the pinned native session id, permissions answered once, a daemon killed mid-turn, a
//! slow subscriber, a stale hook, hibernate/wake, archive. Everything lives in a temp dir.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use mirante_ipc::test_dir::TestDir;
use mirante_ipc::types::{method, DaemonStatus, HookEnvelope, HookParams, Snapshot, SubscribeParams, TaskView};
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

    fn cmd(&self, program: &Path) -> Command {
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake-claude.sh");
        let mut c = Command::new(program);
        c.env("MIRANTE_HOME", &self.home)
            .env("MIRANTE_DAEMON_BIN", bin("mirante-daemon"))
            .env("MIRANTE_HOOK_BIN", bin("mirante-hook"))
            .env("MIRANTE_CLAUDE_BIN", fake)
            .env("FAKE_CLAUDE_LOG", &self.log)
            // Results must not depend on this machine's memory pressure.
            .env("MIRANTE_MEMGUARD", "off");
        c
    }

    fn mirante(&self, args: &[&str]) -> Output {
        self.cmd(&bin("mirante")).args(args).stdin(Stdio::null()).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.mirante(args);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "mirante {args:?}: {text}{}", String::from_utf8_lossy(&out.stderr));
        text
    }

    fn client(&self) -> Client {
        let c = Client::connect(&Paths::new(&self.home).socket()).unwrap();
        c.set_timeout(Some(Duration::from_secs(10))).unwrap();
        c
    }

    fn subscribe(&self) -> Client {
        let mut sub = self.client();
        let _: Snapshot = sub.call(method::EVENTS_SUBSCRIBE, json!({})).unwrap();
        sub
    }

    fn task(&self, branch: &str) -> TaskView {
        self.client().call(method::TASK_GET, json!({ "task": branch })).unwrap()
    }

    /// Every event of this daemon boot so far (replayed from seq 0).
    fn events(&self) -> Vec<Value> {
        let now: Snapshot = self.client().call(method::EVENTS_SUBSCRIBE, json!({})).unwrap();
        let mut sub = self.client();
        let replay = SubscribeParams { since_seq: Some(0), boot_id: Some(now.boot_id) };
        let snap: Snapshot = sub.call(method::EVENTS_SUBSCRIBE, replay).unwrap();
        assert!(snap.resumed);
        let mut out: Vec<Value> = Vec::new();
        while out.last().and_then(|e| e["seq"].as_u64()).unwrap_or(0) < now.seq {
            out.push(sub.next_notification().unwrap().expect("replay").params);
        }
        out
    }

    fn keys(&self) -> String {
        std::fs::read_to_string(self.log.with_extension("log.keys")).unwrap_or_default()
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

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The `--session-id` the fake was started with on its first launch.
fn pinned_id(log: &Path) -> String {
    let args = std::fs::read_to_string(log).unwrap();
    let first = args.lines().next().unwrap();
    let mut it = first.split_whitespace();
    while let Some(a) = it.next() {
        if a == "--session-id" {
            return it.next().unwrap().to_string();
        }
    }
    panic!("no --session-id in {first}")
}

#[test]
fn dispatch_steer_stop_resume_archive_with_a_fake_claude() {
    let env = Env::new();
    assert!(env.ok(&["daemon", "status"]).contains("not running"));
    assert!(env.ok(&["daemon", "start"]).contains("started"));
    let mut sub = env.subscribe();

    let repo = env.repo.to_string_lossy().into_owned();
    let out = env.ok(&["new", &repo, "--agent", "claude", "--branch", "e2e", "say", "hi"]);
    assert!(out.contains("ready"), "{out}");
    let events = wait_events(&mut sub, "turn_ended", 1);
    let statuses: Vec<&str> =
        events.iter().filter(|e| e["kind"] == "status").filter_map(|e| e["payload"]["to"].as_str()).collect();
    assert!(statuses.contains(&"working"), "{statuses:?}");
    let sid = pinned_id(&env.log);
    assert!(events.iter().any(|e| e["kind"] == "native_session_id" || e["payload"].to_string().contains(&sid)));

    assert!(env.ok(&["say", "e2e", "again"]).contains("sent now"));
    wait_events(&mut sub, "turn_ended", 1);
    let ls = env.ok(&["ls"]);
    assert!(ls.lines().any(|l| l.contains("e2e") && l.contains("idle") && l.contains("manual")), "{ls}");

    assert!(env.ok(&["stop", "e2e"]).contains("ended"));
    let out = env.ok(&["resume", "e2e"]);
    assert!(out.contains("ready (resumed)"), "{out}");
    let args = std::fs::read_to_string(&env.log).unwrap();
    assert!(args.lines().last().unwrap().contains(&format!("--resume {sid}")), "{args}");
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

#[test]
fn concurrent_answers_to_one_permission_type_one_key() {
    let env = Env::new();
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "perm", "ask", "first"]);
    wait_until("the card", || env.task("perm").needs_you == 1);
    let perms = env.ok(&["perms"]);
    let id = perms.lines().nth(1).unwrap().split_whitespace().next().unwrap().to_string();
    // Two `allow`s race: both succeed (the second is the stored answer), one key is typed.
    let a = env.cmd(&bin("mirante")).args(["allow", &id]).stdout(Stdio::piped()).spawn().unwrap();
    let b = env.cmd(&bin("mirante")).args(["allow", &id]).stdout(Stdio::piped()).spawn().unwrap();
    for out in [a.wait_with_output().unwrap(), b.wait_with_output().unwrap()] {
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        assert!(String::from_utf8_lossy(&out.stdout).contains("allowed_once"));
    }
    wait_until("the key", || !env.keys().is_empty());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(env.keys(), "1\n");
    // A different answer to the answered card is a conflict, and types nothing.
    let deny = env.mirante(&["deny", &id]);
    assert!(!deny.status.success());
    assert_eq!(env.keys(), "1\n");
}

#[test]
fn a_daemon_killed_mid_turn_ends_the_session_on_restart_and_resume_works() {
    let env = Env::new();
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "crash", "ask", "now"]);
    wait_until("the card", || env.task("crash").needs_you == 1);
    assert!(env.ok(&["say", "--queue", "crash", "later"]).contains("queued"));
    let s: DaemonStatus = env.client().call(method::DAEMON_STATUS, json!({})).unwrap();
    // SAFETY: plain syscall on the daemon this test started.
    assert_eq!(unsafe { libc::kill(s.pid as i32, libc::SIGKILL) }, 0);
    wait_until("the old daemon gone", || unsafe { libc::kill(s.pid as i32, 0) } != 0);

    // Any command starts a fresh daemon, whose recovery ends the orphaned session.
    let ls = env.ok(&["ls"]);
    assert!(ls.lines().any(|l| l.contains("crash") && l.contains("ended")), "{ls}");
    let kinds: Vec<String> = env.events().iter().map(|e| e["kind"].as_str().unwrap_or("").to_string()).collect();
    for k in ["permission_expired", "queue_dropped"] {
        assert!(kinds.iter().any(|x| x == k), "{k} missing from {kinds:?}");
    }
    assert!(env.ok(&["perms"]).contains("no pending"));
    let out = env.ok(&["resume", "crash"]);
    assert!(out.contains("ready (resumed)"), "{out}");
    let sid = pinned_id(&env.log);
    assert!(std::fs::read_to_string(&env.log).unwrap().lines().last().unwrap().contains(&format!("--resume {sid}")));
}

#[test]
fn a_slow_subscriber_is_dropped_while_the_daemon_keeps_answering_then_resumes() {
    let env = Env::new();
    env.ok(&["daemon", "start"]);
    let mut slow = env.client();
    let snap: Snapshot = slow.call(method::EVENTS_SUBSCRIBE, json!({})).unwrap();
    // 2000 events (hooks for an unknown task are logged as `hook_orphan`) while `slow`
    // reads nothing.
    let mut c = env.client();
    let hook = |i: usize| HookEnvelope {
        params: HookParams { task: "nobody".into(), payload: json!({"hook_event_name": "Stop"}).to_string(), ts_ms: 0 },
        event_id: Some(format!("e{i}")),
        received_ts_ms: None,
        truncated: None,
        session: None,
    };
    for i in 0..2000 {
        let _: Value = c.call(method::HOOK_EVENT, hook(i)).unwrap();
    }
    let started = Instant::now();
    let _: DaemonStatus = env.client().call(method::DAEMON_STATUS, json!({})).unwrap();
    assert!(started.elapsed() < Duration::from_secs(2), "the daemon must not wait for a slow reader");
    // The slow stream ends after what was buffered; it then resumes where it stopped.
    let mut last = snap.seq;
    while let Ok(Some(n)) = slow.next_notification() {
        last = n.params["seq"].as_u64().unwrap();
    }
    assert!(last < snap.seq + 2000, "the stream was cut");
    let mut again = env.client();
    let resumed: Snapshot = again
        .call(method::EVENTS_SUBSCRIBE, SubscribeParams { since_seq: Some(last), boot_id: Some(snap.boot_id) })
        .unwrap();
    assert!(resumed.resumed);
    let next = again.next_notification().unwrap().unwrap();
    assert_eq!(next.params["seq"].as_u64(), Some(last + 1));
}

#[test]
fn a_stale_hook_from_before_a_resume_is_dropped() {
    let env = Env::new();
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "stale", "say", "hi"]);
    wait_until("idle", || env.task("stale").task.status.as_str() == "idle");
    let task = env.task("stale").task.id;
    let old = env
        .events()
        .into_iter()
        .find(|e| e["kind"] == "session_launch")
        .and_then(|e| e["payload"]["session_id"].as_str().map(String::from))
        .unwrap();
    env.ok(&["stop", "stale"]);
    assert!(env.ok(&["resume", "stale"]).contains("ready (resumed)"));
    // The old Claude's hook arrives late, stamped with the old Mirante session.
    let mut h = env
        .cmd(&bin("mirante-hook"))
        .env("MIRANTE_TASK_ID", &task)
        .env("MIRANTE_SESSION_ID", &old)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    h.stdin.take().unwrap().write_all(br#"{"hook_event_name":"UserPromptSubmit","prompt":"late"}"#).unwrap();
    assert!(h.wait().unwrap().success());
    wait_until("hook_stale", || env.events().iter().any(|e| e["kind"] == "hook_stale"));
    assert_eq!(env.task("stale").task.status.as_str(), "idle");
}

#[test]
fn hibernate_wake_and_doctor_from_the_cli() {
    let env = Env::new();
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "nap", "say", "hi"]);
    wait_until("idle", || env.task("nap").task.status.as_str() == "idle");
    assert!(env.ok(&["hibernate", "nap"]).contains("hibernated"));
    assert!(env.ok(&["ls"]).contains("hibernated: mirante wake"));
    assert!(env.ok(&["wake", "nap"]).contains("ready (resumed)"));
    let doctor = env.ok(&["doctor"]);
    assert!(doctor.contains("orphan processes: none") && doctor.contains("orphan checkouts: none"), "{doctor}");
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
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
```

No compiler warnings. (Replayed 4 times in a row, all green; the whole e2e file takes about 3 s.)

- [ ] **Step 4: Commit**

```bash
git add crates/mirante-cli/tests crates/mirante-daemon/tests
git commit -F - <<'EOF'
test: end to end with a fake claude (pinned id, racing answers, daemon crash, slow subscriber, stale hook, hibernate); ignored real-agent tests

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 21: Full verification, real-agent run, dogfood checklist

**Files:**
- Create: `docs/dogfood/2026-11-07-plan-2b-dogfood.md`

The automated part proves the plumbing; this task answers the product question (spec §1): does the cockpit replace cmux? The implementer runs Steps 1, 2 and 4; the founder runs Steps 3, 5 and 6.

- [ ] **Step 1: Run everything**

Run: `cargo build --workspace && cargo test --workspace 2>&1 | grep 'test result'`

Expected, in this order (mirante-cli lib, `mirante` bin, e2e, mirante-core, mirante-daemon lib, daemon bin, `daemon_signals`, real_agents, mirante-hook lib, hook bin, hook `binary`, mirante-ipc, spike-kit lib, then the spike-kit bins and doc tests), each line followed by `; finished in …`:
```
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 157 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 185 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
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
That is 464 passed and 2 ignored (14 cli + 7 e2e + 157 core + 185 daemon + 2 daemon_signals + 14 hook + 2 hook binary + 22 ipc + 61 spike-kit). If `mirante-core` gained tests after `2b8030d`, only its line differs.

- [ ] **Step 2: Lint the new crates; check that Tasks 13–21 left core and spike-kit alone**

Run: `cargo clippy -p mirante-ipc -p mirante-hook -p mirante-daemon -p mirante-cli --all-targets 2>&1 | grep -E '^(warning|error)'`

Expected: no output.

Run: `git diff --stat 2b8030d -- crates/spike-kit crates/mirante-core`

Expected: no output (`2b8030d` is the robustness round Tasks 13–21 build on; the core changes of that round, `Archiving` and schema v3, came before them).

- [ ] **Step 3: The founder runs the real-agent tests (they spend plan quota)**

```bash
cargo build --workspace
MIRANTE_TEST_TRUST=1 MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude \
  cargo test -p mirante-daemon --test real_agents -- --ignored --test-threads=1 claude
MIRANTE_CODEX_BIN=~/.nvm/versions/node/v22.14.0/bin/codex \
  cargo test -p mirante-daemon --test real_agents -- --ignored --test-threads=1 codex
```

Expected: `test claude_turn_permission_stop_and_resume ... ok` and `test codex_steer_and_resume ... ok`. If the Claude test stops at boot, read the `screen` in the assertion message. If the TUI footer, the input prompt or the trust wording changed, adjust `screen::has_manual_mode_footer` / `screen::has_input_prompt` / `screen::TRUST_YES` (Task 14); the trust wording and footer were written from the requirement, not captured from a live 2.1.x screen, so this is the first real check. Never add a time-based "ready" fallback: it would type the goal into whatever dialog is on screen.

- [ ] **Step 4: Write the dogfood checklist**

`docs/dogfood/2026-11-07-plan-2b-dogfood.md`:
````markdown
# Plan 2b dogfood: replace cmux with `mirante`

Spec §6 metrics this run feeds (plus the memory guard of Tasks 18–19): the founder uses it instead of cmux for 2 consecutive weeks; context recreated by hand after closing and reopening = 0; wrong status = 0 in 50 hand-checked transitions; mid-run steering works on both agents or the fallback is explicit. (The "3+ external developers" metric needs an installable build and is out of scope here.)

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
- [ ] `kill -9` the daemon while `mirante archive` runs; after the restart the task is `archived` (clean worktree) or back to `active` (dirty worktree, `archive_abandoned`), never stuck in `archiving`.
- [ ] Make a checkout under `~/.mirante/worktrees/` that no task owns (`git -C <repo> worktree add ~/.mirante/worktrees/<repo>/stray`): the next daemon start reports it (`worktree_orphan`) and `mirante doctor` lists it; nothing deletes it.
- [ ] Queue a message (`mirante say --queue`) during a turn, then `kill -9` the daemon: after the restart the trail has `queue_dropped` with that text.

**G. Memory**
- [ ] `mirante ls` shows a `MEM` column for live tasks (e.g. `1.1G/4.0G`); `mirante watch` shows the `memory:` line (level, free, swap). On a normal day the level stays `normal` even with swap well used (the kernel's own pressure level decides on macOS).
- [ ] `mirante hibernate <idle task>` → `hibernated`; `ls` says `hibernated: mirante wake`; `mirante wake <task>` → `ready (resumed)` and the agent remembers the conversation. `mirante hibernate` on a task with a pending permission is refused.
- [ ] `mirante doctor` → `orphan processes: none` after a normal day; start a dev server from an agent, `kill -9` the daemon, restart it: `doctor` lists the server (ppid 1, cwd in the worktree); `mirante doctor --kill <pid>` asks first, then ends it.
- [ ] Optional, apps closed first: the stress procedure in `docs/dogfood/memory-stress.md` (Task 18).

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
| Status `working / needs_you / idle / ended` only from hooks and protocol events, never the terminal (spec §2.2) | Task 7 (hooks + transcript → `StatusEngine`), Task 16 (Codex notifications of our thread only); the screen is read only for dialogs and readiness (Task 14 module doc) | `hooks_drive_status_turns_and_the_native_id`, `codex_status_events_drive_status_turns_and_detail`, `plain_turn`, `another_threads_events_never_move_our_status` |
| Notify only when a task needs the user (spec §2.2) | Task 12 `watch` rings the bell on a new `needs_you` | manual (Task 21 §B) |
| Open a task: live terminal (spec §2.3) | Tasks 10, 14, 15 (`attach`: full-state repaint incl. input modes + live bytes + typed input; terminal queries answered by the daemon and never forwarded), Task 12 CLI raw passthrough; Codex read-only stream with a mode banner (Task 17) | `attach_streams_raw_bytes_both_ways_until_the_client_closes`, `attach_repaints_with_input_modes_then_streams_and_input_reaches_the_agent`, `arbitrary_bytes_pass_through_attach_unchanged`, `queries_are_answered_while_attached_and_never_reach_the_client`, `a_query_split_across_reads_is_completed`, `the_stream_is_readable_starts_with_the_mode_and_reports_abandoned_commands` |
| Talk mid-run: steer now / queue for the turn end (spec §2.4; FINDINGS "Steer UI") | Task 8 `send`, Task 7 `kick`, Task 15 typist (bracketed paste, one message at a time), Task 17 `turn/steer` (fallback to `turn/start` only on a JSON-RPC rejection after the turn closed, never on a timeout) | `steer_goes_out_now_and_queue_waits_for_the_turn_end`, `messages_are_one_bracketed_paste_then_enter`, `two_concurrent_sends_never_interleave`, `send_starts_a_turn_with_the_policy_then_steers_the_running_one`, `a_steer_rejected_after_its_turn_closed_falls_back_but_a_timeout_never_does` |
| Steering confirmed from the transcript, not `UserPromptSubmit` (FINDINGS rule 2) | Task 7 `classify_steers`; Codex: `clientUserMessageId` confirmed by its `userMessage` item (Task 17) | `steer_verdict_comes_from_the_transcript_at_the_turn_end`, `send_starts_a_turn_with_the_policy_then_steers_the_running_one` |
| Never type into a dialog | Task 8 refusal (`NEEDS_YOU`); Task 13 hold flag (pending permission); Task 15 typist re-checks the screen and the hold flag right before the paste and right before Enter (aborted text is re-queued at the front; after the paste the line is cleared with Ctrl-U, never Enter); nothing typed before `Ready`; Task 15 hub refuses `!` / multi-line `/` up front | `steer_is_refused_while_a_dialog_is_open_but_queue_is_accepted`, `a_dialog_that_appears_before_the_keystrokes_gets_nothing`, `a_permission_pending_before_enter_clears_the_line_instead_of_submitting`, `nothing_is_typed_before_ready`, `an_aborted_delivery_goes_back_to_the_front_of_the_queue`, `a_pending_permission_holds_the_drivers_typing`, `claude_messages_the_typist_would_refuse_are_refused_up_front` |
| Bracketed paste is sanitized (CR→LF, CSI/OSC/C0/C1 stripped incl. a smuggled `ESC[201~`, tabs → 4 spaces, newlines and emoji kept); `/` = slash command typed as keys, `!` refused (Task 14 decision) | Task 14 `paste` | `controls_and_smuggled_paste_end_are_removed_newlines_and_emoji_kept`, `every_kind_of_control_is_dropped`, `slash_commands_are_keystrokes_and_bang_is_refused` |
| Permissions: persisted cards, answered once, never auto-approved (spec §2.5, §5) | Tasks 7–8 (hub; a repeated identical answer returns the stored row), 14–15 (Claude exact options), 16–17 (Codex one-time decisions; legacy deny `{"denied":…}`, never `abort`; withdrawn requests settle their own card) | `claude_permission_answer_types_the_choice_then_persists_once`, `repeating_the_same_answer_returns_the_stored_row`, `approvals_become_cards_answered_once_and_are_withdrawn_when_resolved`, `decisions_per_request_kind_are_one_time_only`, `a_resolved_codex_request_settles_only_its_own_card`, e2e `concurrent_answers_to_one_permission_type_one_key` |
| Answers given directly in the attached terminal are detected | Task 7 `settle_permissions` (PostToolUse / PostToolUseFailure → allowed, turn end → denied) | `hook_permission_request_is_persisted_and_settled_when_the_tool_runs`, `deny_in_the_terminal_is_settled_by_the_transcript_turn_end` |
| Exact option, never "always" / "auto", in the live dialog only (bottom rows + `Esc to` footer) (FINDINGS rule 6) | Task 14 `screen` | `exact_one_time_options_only`, `lookalike_options_above_the_dialog_are_ignored`, `dialogs_need_a_footer_in_the_bottom_rows`, `plan_approval_has_no_plain_yes_but_still_blocks_typing`, `permission_answer_types_the_exact_option_for_the_matching_dialog` |
| Trust dialog shown to the user, never accepted silently; accepted by the exact "Yes, I trust this folder" digit (FINDINGS rule 5) | Task 15 boot watcher (only before the first `Ready`), Task 7 `trust`, Task 12 CLI `[y/N]` | `trust_is_only_answered_when_the_prompt_is_open`, `the_trust_prompt_waits_for_the_user_then_its_exact_option_is_typed`, `the_trust_dialog_is_the_exact_option_with_a_footer`, `declining_trust_ends_the_process` |
| Ready only when safe: `SessionStart` seen + input prompt + no dialog + stable screen (footer = fast path); no time-based fallback | Task 15 `watch_boot`, Task 13 `Driver::session_started` | `ready_needs_session_start_and_a_settled_input_prompt`, `the_input_prompt_and_the_ready_fast_path`, `session_start_reaches_the_driver_and_fenced_hooks_update_the_native_id` |
| Context survives: native id pinned up front (`--session-id`), reopen resumes natively; a failed resume is reported, a fresh start is the user's explicit choice (spec §2.6, FINDINGS S4) | Task 7 resume path, Task 9 recovery, Task 13 (`ResumeFailed`, `NativeGone`, `mirante resume --fresh`), Task 15 `--session-id` / `--resume`, Task 17 `thread/resume` (`excludeTurns`) | `a_fresh_launch_pins_the_session_id_and_stamps_the_mirante_session`, `a_resume_that_exits_at_once_is_reported_never_restarted`, `a_vanished_native_conversation_fails_the_resume_with_a_typed_error`, `resume_uses_thread_resume_without_turns_and_a_vanished_thread_is_typed`, e2e `a_daemon_killed_mid_turn_ends_the_session_on_restart_and_resume_works`, `claude_turn_permission_stop_and_resume` / `codex_steer_and_resume` (ignored) |
| Hook delivery survives a down daemon; stale hooks never touch a newer session | Task 4 spool, Task 9 ingest, robustness-round fence (`MIRANTE_SESSION_ID`, set by Task 15's launch) | `daemon_down_spools_one_private_line_per_event`, `a_late_hook_from_an_earlier_session_of_the_task_is_dropped`, e2e `a_stale_hook_from_before_a_resume_is_dropped` |
| One owner of state; Unix socket 0600; peer uid; no TCP (spec §4, §5) | Task 10 | `socket_is_private_and_the_peer_is_us`, Task 10 smoke test |
| One driver interface; unsupported / not-now are typed results (spec §4) | Tasks 5 and 13 (`NotNow`, `NativeGone`), Task 17 (`trust`, `input`, interrupt without a turn) | `errors_map_to_typed_rpc_codes`, `interrupting_with_no_running_turn_is_a_clear_error` |
| Events carry `seq` + `boot_id`; clients reconcile; a slow client never stalls the daemon (spec §4) | Task 6 `subscribe` (bounded queues), Task 10 `events.subscribe { since_seq }`, Task 12 `watch` | `subscribers_get_every_event_after_the_snapshot`, `a_subscriber_that_never_reads_is_bounded_then_dropped`, e2e `a_slow_subscriber_is_dropped_while_the_daemon_keeps_answering_then_resumes` |
| State persisted on every transition (spec §4) | Every hub mutation goes through a `Store` method | `hooks_drive_status_turns_and_the_native_id`, recovery tests |
| Hooks scoped by env var; no global install (spec §4) | Task 15 per-session `--settings` file, `MIRANTE_TASK_ID`, `MIRANTE_SESSION_ID`; Task 4 hook does nothing without them | `without_a_task_id_nothing_happens`, `a_fresh_launch_pins_the_session_id_and_stamps_the_mirante_session`, e2e argument check |
| Worktree removed on archive, branch kept; crash-safe (`Archiving`) (spec §4) | Task 6 `archive` (robustness round: `Archiving` persisted first) | `archive_removes_the_worktree_keeps_the_branch_and_hides_the_task`, `archive_persists_archiving_before_removing_the_worktree`, e2e |
| Per-task lock, no orphaned lock, failures reach a final state (spec §8) | Store lock (Plan 2a) + Task 7 spawn-failure path | `start_takes_the_per_task_lock_and_boot_ready_delivers_the_first_prompt`, `spawn_failure_releases_the_lock_and_ends_the_session` |
| No orphaned agent processes: stop ends the whole tree, never a reused pid | Task 13 `proctree::kill_tree` (pid + start time, SIGTERM → SIGKILL, re-walk during the kill), Tasks 14/15/17 stop, Task 19 `doctor` | `kill_tree_ends_a_tree_that_forks_while_dying_and_ignores_sigterm`, `a_reused_pid_is_never_signaled`, `stop_ends_the_whole_tree_even_a_child_forked_during_the_kill`, `stop_ends_the_app_server_and_everything_it_started` |
| Parse errors never silently empty (spec §8) | Task 7 `hook_unparseable`, Task 10 parse errors answered, Task 16 non-JSON app-server lines reported | `hooks_for_unknown_or_ended_sessions_are_logged_not_applied`, `garbage_lines_get_a_parse_error_and_the_connection_survives`, `notifications_server_requests_and_bad_lines_reach_the_callback_and_eof_closes` |
| Real binaries, env scrub, never auth vars, `--permission-mode manual`, additive `--settings` (FINDINGS rule 4; Codex launch hygiene) | Task 15 (via Plan 2a `launch_spec`), Task 17 | `binary_resolution_skips_shims_and_env_scrub_never_touches_auth`, `a_fresh_launch_pins_the_session_id_and_stamps_the_mirante_session`, e2e argument check |
| Codex "auto-edits" = `on-request` + `workspace-write` + reviewer `user`, verified against what the thread reports and re-sent each turn; labelled in `ls` / `show` / attach | Task 17 `thread_params` + boot check, Task 13 CLI `MODE` | `handshake_starts_a_thread_in_auto_edits_mode_and_checks_it_was_applied`, `a_thread_whose_effective_mode_differs_fails_the_boot`, `codex_rows_show_the_auto_edits_mode_and_usage` |
| Codex: open commands after `turn/completed` (FINDINGS rule 8); interrupted/failed turns abandon them; settle on thread idle or a timeout | Task 16 engine, shown as `detail` | `command_finishing_after_turn_completed_keeps_working_with_detail`, `an_interrupted_or_failed_turn_abandons_its_open_commands`, `open_commands_settle_on_thread_idle_or_after_the_timeout` |
| Codex: steer lands at the next step; stop-now is interrupt (FINDINGS rule 9) | Task 17 `turn/interrupt`, Task 12 `mirante interrupt` | `send_starts_a_turn_with_the_policy_then_steers_the_running_one` |
| Codex errors, quota and token usage visible | Task 16 detail, Task 17 `Usage` → Task 13 `TaskView.usage` | `errors_become_the_detail`, `usage_errors_and_settling_reach_the_hub`, `usage_reports_show_in_the_task_view` |
| Memory: measure and act, never limit up front (macOS cannot enforce RLIMIT_AS/DATA); kernel pressure level decides; warn → pause → offer → auto-hibernate only idle, never a session waiting on you, one per cooldown; leak and idle hibernation; orphans reported, killed only on request | Tasks 18–19 | `the_kernels_level_wins_over_swap_and_free`, `sustained_critical_hibernates_only_an_idle_session_without_a_pending_permission`, `sustained_critical_pressure_hibernates_an_idle_session_but_never_one_waiting_on_you`, `pressure_pauses_queued_deliveries_and_new_sessions_until_it_eases`, `an_idle_session_that_keeps_growing_is_a_leak`, `idle_hibernation_happens_only_when_configured`, `hibernate_and_wake_round_trip_on_the_native_conversation`, `killing_an_orphan_signals_only_that_process_and_never_a_stale_one`, `a_real_process_tree_is_measured`, e2e `hibernate_wake_and_doctor_from_the_cli` |
| Resumed Claude session must not replay old turns (Plan 2a risk note) | Task 7 `follow_transcript` starts at the file's end | `transcript_path_starts_the_tailer_at_the_end_of_an_existing_file` |
| Tail loops while `more`, never while offset < length (Plan 2a API) | Task 15 tail thread | `transcript_lines_and_offsets_are_emitted_from_the_given_offset` |
| CLI commands from the brief plus `show`, `interrupt`, `resume --fresh`, `hibernate`, `wake`, `doctor` | Tasks 11–13, 19 | `simple_commands`, `new_takes_flags_anywhere_and_joins_the_goal`, e2e |
| Tests sandboxed in temp dirs; real-agent tests `#[ignore]`; e2e independent of the machine's memory (`MIRANTE_MEMGUARD=off`) | All tasks; Task 20 | every test uses `TestDir` / temp git repos; `real_agents.rs` header |
| Spec §6 metrics | Task 21 checklist | manual |

Gaps, deliberately left to Plan 3 or later (also listed under "Out of scope"): editable / reorderable queue, step feed and per-turn diff, notification suppression for the task on screen, the steer verdict shown in a UI (recorded as `message_delivery` events now), cost ceilings, token redaction in logs, explicit "from the human operator" labels on mid-run messages, a `capabilities()` method, the optional Linux `systemd-run --scope -p MemoryHigh=` wrapper (Task 18 builds the argv behind a flag that is off; nothing runs it yet), and persisting the "hibernated" mark across daemon restarts (after a restart every session is ended and `mirante resume` does the same job).

### Placeholder scan

No "TBD", "TODO", "implement later" or "similar to Task N". Every code block of Tasks 13–20 was generated from the replayed commits and checked byte for byte against them by script (new files: the test module and the code above it; replaced files: the whole file; edits: the snippet occurs verbatim in the file, except two blocks that end in an explicit `// … unchanged` marker). `RealFactory` returns a typed `Unsupported` for Codex between Tasks 15 and 17: shipped behaviour of those intermediate commits, not a placeholder.

### Type and name consistency

- Wire types crossing the socket: Task 2's types plus `TaskView.usage` (Task 13), `TaskView.memory` / `hibernated`, `MemView`, `SystemView`, `DoctorReport`, `OrphanView`, `KillOrphan`, `DaemonStatus.memory` and the methods `session.hibernate`, `session.wake`, `daemon.doctor`, `daemon.kill_orphan` (Task 19). Every addition is `#[serde(default)]`: an older client or daemon still parses.
- `Driver` (Task 5, extended in Task 13 with `session_started` and `root`, both defaulted) is implemented by `FakeDriver`, `ClaudeDriver` (Task 15) and `CodexDriver` (Task 17). `DriverEvent` variants added in Task 13 (`PermissionResolved`, `DeliveryAborted`, `ResumeFailed`, `Usage`) are all matched in `State::handle`; `DriverError::{NotNow, NativeGone}` are mapped in `From<HubError> for RpcError`.
- `Sink::{new, emit, held, hold_flag}` (Task 13): drivers read `held`, the hub sets the flag in `State::sync_holds` (run by every `publish`).
- `proctree::{Proc, proc_of, alive, pgid, children, descendants, kill_tree, Signaler, Kill, KillReport}` (Task 13) are what Tasks 14, 15, 17, 18 and 19 use to name, sample and end processes.
- `memguard::{Policy, Config, SessionInfo, SessionMem, Action, Level, MemState, MemSampler, SystemSample, TreeSample, Pressure, FakeSampler, os_sampler, find_orphans, kill_orphan, Orphan}` (Task 18) are what `hub::memory` (Task 19) uses.
- Environment variables: `MIRANTE_HOME`, `MIRANTE_TASK_ID`, `MIRANTE_SESSION_ID` (set by Task 15's launch; the fence drops hooks stamped with another session), `MIRANTE_CLAUDE_BIN`, `MIRANTE_CODEX_BIN`, `MIRANTE_HOOK_BIN`, `MIRANTE_DAEMON_BIN`, `MIRANTE_MEM_BUDGET_MB`, `MIRANTE_IDLE_HIBERNATE_MIN`, `MIRANTE_MEMGUARD` (`off` for tests), `RUST_LOG` (Codex: `warn` unless set), `MIRANTE_TEST_TRUST` (test only).

### Decisions the founder should know

1. **No async runtime.** std threads, one per connection and per agent stream.
2. **A fourth crate, `mirante-ipc`.** The daemon, CLI and hook share the wire types and framing.
3. **Attach framing.** Same socket: a JSON-RPC `attach` response, then raw bytes both ways. The daemon answers the agent's terminal queries (cursor position, device attributes, colours) itself, always, and strips them from what the attached terminal sees, so nothing is answered twice and a detached TUI never stalls.
4. **The daemon owns the agent processes.** Stopping or crashing the daemon ends every session; recovery marks them `Ended` and `mirante resume` restores the context natively. Every stop ends the agent's whole process tree.
5. **Trust UX.** `mirante new` / `resume` show Claude's trust dialog and ask `[y/N]`; `--trust` is the explicit yes up front. Accept types the digit of "Yes, I trust this folder".
6. **Queue is daemon-side.** Queued messages are reported in `queue_dropped` when a session ends, not re-sent; while memory is tight they wait.
7. **Ready needs `SessionStart` + an input prompt + no dialog + a settled screen.** No time-based fallback.
8. **Messages are one bracketed paste.** Multi-line text is sent as typed; a leading `/` is a slash command (typed as keys, one line); a leading `!` is refused (bash mode runs a shell command with no permission prompt; attach and type it yourself).
9. **Codex runs in "auto-edits" mode:** `approvalPolicy: "on-request"`, `sandbox: "workspace-write"`, `approvalsReviewer: "user"`. Edits and commands inside the worktree run without asking; anything beyond the sandbox (network, other paths, escalation) asks you. The daemon checks the thread reports exactly that, re-sends it on every turn, and never answers "for the session".
10. **A failed resume is never papered over.** Claude exiting within 3 s of `--resume`, or Codex reporting no rollout, is recorded as `resume_failed`; `mirante resume <task> --fresh` is the explicit way to start over.
11. **Memory: measure and act, never limit.** Per-session budget min(4 GB, 25 % of RAM); the kernel's own pressure level decides (swap use alone does not count: on this Mac swap sits at 90 % with the kernel at "normal"); automatic hibernation only under sustained critical pressure, only of an idle session, never one waiting on you, one per minute; orphan processes are only reported (`mirante doctor`) and killed one at a time on request.

### Risks

- **Claude TUI text is a moving target.** Permission dialogs are matched on real 2.1.285 grids; the trust dialog (exact label "Yes, I trust this folder", a footer with "Esc to" / "Enter to confirm"), the input prompt (`❯`) and the ready footer were written from the Week-1 notes and the requirements, not captured live, and are first exercised in Task 21 Step 3. A mismatch fails safe (the task stays "starting"); answering trust by digit, Ctrl-U clearing a multi-line paste, and Claude's handling of bracketed paste are likewise unconfirmed live.
- **A message that looks like a dialog.** A pasted text that itself contains a `❯ 1.` option and an "Esc to" footer would be judged a dialog by the second check and re-queued every turn (it is never typed into a real dialog, but it never goes out either).
- **Codex shapes come from the 0.167 schema, not a run:** the effective-mode check compares `sandbox.type == "workspaceWrite"`, the missing-thread error is recognised by its "no rollout found" text, legacy deny is `{"decision":{"denied":{…}}}`. Task 21 §D is the first live approval; check `~/.mirante/sessions/<task>/codex-stderr.log` (its last 4 KiB is also in the `codex_exited` step).
- **Codex terms review is still open** (FINDINGS).
- **Memory thresholds are not measured under real pressure.** The ladder is unit-tested with an injected clock and the samplers against small real processes; the Linux PSI cut-offs (10 / 40) are a choice; the manual stress procedure (`docs/dogfood/memory-stress.md`) has not been run.
- **Linux is compiled in pieces only.** `proctree` and `memguard` type-check for `x86_64-unknown-linux-gnu` in isolation (the full crate needs a Linux C toolchain for SQLite); nothing Linux-specific was run.
- **Spike settings.** After Task 1, `target/debug/mirante-hook` is the forwarding hook; re-running a Week-1 spike needs its settings pointed at `target/debug/spike-hook`.
