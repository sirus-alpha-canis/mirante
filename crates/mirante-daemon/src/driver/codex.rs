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
//!   (`DriverEvent::PermissionResolved`). The one exception is honest about itself: a
//!   `grantRoot` file-change request asks for write access under a directory for the rest
//!   of the session; its card says so, and allowing it prints "[allowed: write access under
//!   <root> for the rest of the session]" and is recorded as `allowed_session`.
//! - The effective sandbox (network access, extra writable roots, granted roots) is shown
//!   at the top of every attach stream and reported to the hub (`DriverEvent::Sandbox`,
//!   e.g. `net:off`) for `mirante ls`.
//! - `turn/interrupt` goes out from its own short-lived thread, naming the turn running
//!   when it is sent: a message send waiting on a stuck request never delays it. Codex
//!   ends the turn but not the command it was running; a command still not reported ended
//!   after a short grace is killed with the other processes the turn started below
//!   app-server (`command_killed`; limits in `reap_abandoned`).
//! - Once app-server's output ended, `send` and `interrupt` fail at once.
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use mirante_core::claude_launch::{env_to_scrub, KNOWN_SHIM_DIRS};
use mirante_core::model::Status;
use mirante_ipc::types::BootState;
use serde_json::{json, Value};

use super::codex_rpc::{CallError, Incoming, RpcConn};
use super::codex_status::{request_key, CodexEngine, CodexInput, SETTLE_AFTER};
use super::proctree::{self, Kill, Proc};
use super::pty;
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
/// The card tool of a `grantRoot` request (allowing it is recorded as `allowed_session`).
pub const GRANT_TOOL: &str = "file_change (session-wide grant)";

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
    /// How long a command an interrupted turn abandoned may take to end on its own before
    /// Mirante kills it (see `reap_abandoned`).
    pub abandon_grace: Duration,
}

impl Default for CodexTiming {
    fn default() -> Self {
        CodexTiming {
            request: Duration::from_secs(30),
            settle_after: SETTLE_AFTER,
            tick: Duration::from_secs(1),
            stop_grace: Duration::from_secs(3),
            abandon_grace: Duration::from_secs(2),
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
        _ => json!({ "decision": if allow { "accept" } else { deny_decision(params).0 } }),
    }
}

/// The v2 one-time "no" for a request, and a note when it is not plain `decline`. Codex
/// 0.167 lists `availableDecisions` without `"decline"` (`["accept", {…amendment}, "cancel"]`)
/// although it accepts it; the listed deny-equivalent (`"cancel"`, which also ends the turn)
/// is sent then, so Mirante never relies on an unlisted answer. With no list, `decline`.
pub fn deny_decision(params: &Value) -> (&'static str, Option<&'static str>) {
    let Some(list) = params["availableDecisions"].as_array() else {
        return ("decline", None);
    };
    let has = |d: &str| list.iter().any(|v| v.as_str() == Some(d));
    if has("decline") {
        ("decline", None)
    } else if has("cancel") {
        ("cancel", Some("\"decline\" is not in availableDecisions: sent \"cancel\""))
    } else {
        ("decline", Some("availableDecisions lists neither \"decline\" nor \"cancel\": sent \"decline\""))
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
            let kind = c["kind"]["type"].as_str().unwrap_or("change");
            let diff = c["diff"].as_str().unwrap_or("");
            // For an add (Codex 0.167) `diff` is the new file's raw content, not a unified
            // diff; a delete is taken to be the same. A real unified diff is still counted.
            let unified = diff.lines().any(|l| l.starts_with("@@"));
            let (a, r) = match kind {
                "add" if !unified => (diff.lines().count(), 0),
                "delete" if !unified => (0, diff.lines().count()),
                _ => diff_counts(diff),
            };
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
            (GRANT_TOOL, Some(format!("write access under {root} for the rest of the session")))
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

/// `thread/resume` failed because the thread's conversation is gone. Matched on the text
/// Codex 0.167's schema implies ("no rollout found"), or on an internal error (-32603)
/// saying "not found" or naming the rollout. The exact text must be confirmed in the Task
/// 21 real run (resume a thread whose rollout file was deleted).
pub fn is_native_gone(code: i64, message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("no rollout found") || (code == -32603 && (m.contains("not found") || m.contains("rollout")))
}

/// The effective sandbox in a few characters, for `mirante ls`: `net:off`, `net:on, +1
/// root`, `net:off, +1 granted root` (`net:?` when the response does not say).
pub fn sandbox_summary(sandbox: &Value, granted: &[String]) -> String {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut parts = vec![match sandbox["networkAccess"].as_bool() {
        Some(true) => "net:on".to_string(),
        Some(false) => "net:off".to_string(),
        None => "net:?".to_string(),
    }];
    let roots = sandbox["writableRoots"].as_array().map_or(0, Vec::len);
    if roots > 0 {
        parts.push(format!("+{roots} root{}", plural(roots)));
    }
    if !granted.is_empty() {
        parts.push(format!("+{} granted root{}", granted.len(), plural(granted.len())));
    }
    parts.join(", ")
}

/// The effective sandbox in words, for the attach stream.
pub fn sandbox_banner(sandbox: &Value, granted: &[String]) -> String {
    let net = match sandbox["networkAccess"].as_bool() {
        Some(true) => "network ON",
        Some(false) => "network off",
        None => "network not reported",
    };
    let mut extra: Vec<String> = sandbox["writableRoots"]
        .as_array()
        .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    extra.extend(granted.iter().map(|g| format!("{g} (granted)")));
    let writable =
        if extra.is_empty() { "the worktree only".to_string() } else { format!("the worktree + {}", extra.join(", ")) };
    format!("[mirante] effective sandbox: {SANDBOX}, {net}, writable: {writable}")
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
/// `stop`); a concurrent caller waits for that reap and gets the same exit code.
struct Process {
    /// Taken out by the reaper: the lock is never held while it waits.
    child: Mutex<Option<Child>>,
    root: Option<Proc>,
    stderr_log: Option<PathBuf>,
    /// `Some(exit code)` once reaped.
    outcome: Mutex<Option<Option<i32>>>,
    reaped: Condvar,
    /// Processes seen below the app-server (refreshed while it runs): once it exits, its
    /// children that left the group are found by nothing else.
    snapshot: Mutex<Vec<Proc>>,
}

impl Process {
    fn new(child: Child, root: Option<Proc>, stderr_log: Option<PathBuf>) -> Process {
        Process {
            child: Mutex::new(Some(child)),
            root,
            stderr_log,
            outcome: Mutex::default(),
            reaped: Condvar::new(),
            snapshot: Mutex::default(),
        }
    }

    /// Keeps the descendant snapshot fresh and, when the app-server exits on its own, ends
    /// what it left (even while a leftover holds its stdout open, so the reader sees no EOF).
    fn watch(self: &Arc<Self>) {
        let Some(root) = self.root else { return };
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            let mut walked = Instant::now() - pty::SNAPSHOT_EVERY;
            loop {
                let Some(p) = weak.upgrade() else { return };
                if lock(&p.outcome).is_some() {
                    return;
                }
                if walked.elapsed() >= pty::SNAPSHOT_EVERY {
                    walked = Instant::now();
                    p.refresh_snapshot(root);
                }
                let gone = {
                    // Held so nobody reaps the zombie (which pins its pid and group id) meanwhile.
                    let child = lock(&p.child);
                    child.is_some() && pty::exited_unreaped(root.pid) && {
                        p.end_tree(pty::LEFTOVER_GRACE, true);
                        true
                    }
                };
                drop(p);
                if gone {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
    }

    fn refresh_snapshot(&self, root: Proc) {
        let now = proctree::descendants(root);
        let mut snap = lock(&self.snapshot);
        snap.retain(|p| proctree::alive(*p));
        for p in now {
            if !snap.contains(&p) && snap.len() < pty::SNAPSHOT_MAX {
                snap.push(p);
            }
        }
    }

    /// Ends the root's tree plus the snapshot (see `proctree::kill_tree_with`); `pinned`: the
    /// caller holds the exited, unreaped root.
    fn end_tree(&self, grace: Duration, pinned: bool) {
        let Some(root) = self.root else { return };
        let snapshot: Vec<Proc> = lock(&self.snapshot).iter().copied().filter(|p| proctree::alive(*p)).collect();
        proctree::kill_tree_with(root, &snapshot, pinned, grace, &Kill);
    }

    /// Waits up to `wait` for the exit, then ends what is left of the tree; records the code.
    fn reap(&self, wait: Duration) -> Option<i32> {
        let taken = lock(&self.child).take();
        let Some(mut child) = taken else {
            // Another caller is reaping (or did): wait for its outcome.
            let g = lock(&self.outcome);
            let limit = wait + Duration::from_secs(5);
            let (g, _) = self.reaped.wait_timeout_while(g, limit, |o| o.is_none()).unwrap_or_else(|p| p.into_inner());
            return (*g).flatten();
        };
        let deadline = Instant::now() + wait;
        match self.root {
            Some(root) => {
                // Wait without reaping: the unreaped root pins its pid and group id while
                // its leftovers are ended.
                while Instant::now() < deadline
                    && !pty::exited_unreaped(root.pid)
                    && matches!(child.try_wait(), Ok(None))
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                self.end_tree(Duration::from_millis(500), pty::exited_unreaped(root.pid));
            }
            None => {
                while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        let code = child.wait().ok().and_then(|s| s.code());
        *lock(&self.outcome) = Some(code);
        self.reaped.notify_all();
        code
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
    /// Roots the user granted write access to for the rest of the session (`grantRoot`).
    granted_roots: Vec<String>,
    /// clientUserMessageId -> text, until its `userMessage` item shows up.
    unconfirmed: HashMap<String, String>,
    rate_limits: Option<Value>,
    token_usage: Option<Value>,
    closed: bool,
    /// The app-server's descendants when the running turn started: a process not among
    /// them was started during the turn.
    turn_baseline: Option<HashSet<Proc>>,
    /// Commands an interrupted turn abandoned, set by `settle` for `on_incoming` to act on.
    reap_due: Option<Vec<String>>,
    /// Abandoned command ids Codex has not reported ended yet.
    abandoned_open: HashSet<String>,
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
        let (abandoned, interrupted) = self.engine.take_abandoned();
        for _ in &abandoned {
            self.output("  [command abandoned]\r\n");
        }
        if interrupted && !abandoned.is_empty() {
            self.reap_due = Some(abandoned);
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
    abandon_grace: Duration,
}

/// Codex's `turn/interrupt` ends the turn but not the command it was running (0.167: a
/// `sleep 30` ran to its end under app-server). After `ctx.abandon_grace`, if Codex still
/// has not reported one of the abandoned commands ended, the processes that were started
/// during the turn (descendants of app-server absent from the turn's baseline, still the
/// same process by pid AND start time) and are still running are killed, each with its own
/// tree, and `command_killed` is reported. Never app-server itself, never a process older
/// than the turn, never one started after the interrupt (a next turn's).
///
/// Limits: Codex says nothing about which process runs which command, so everything the
/// turn started below app-server goes, including a helper it launched lazily during that
/// turn (an MCP server started on first use); nothing happens without a known app-server
/// process (tests over pipes) or a baseline (a turn already running when Mirante connected).
fn reap_abandoned(ctx: &Ctx, s: &mut Shared, ids: Vec<String>) {
    let (Some(root), Some(baseline)) = (ctx.process.as_ref().and_then(|p| p.root), s.turn_baseline.take()) else {
        return;
    };
    let started: Vec<Proc> =
        proctree::descendants(root).into_iter().filter(|p| *p != root && !baseline.contains(p)).collect();
    if started.is_empty() {
        return;
    }
    s.abandoned_open.extend(ids.iter().cloned());
    let (shared, sink, grace) = (ctx.shared.clone(), ctx.sink.clone(), ctx.abandon_grace);
    std::thread::spawn(move || {
        std::thread::sleep(grace);
        let still: Vec<String> = {
            let mut s = lock(&shared);
            let still = ids.iter().filter(|id| s.abandoned_open.remove(*id)).cloned().collect();
            if s.closed {
                return;
            }
            still
        };
        if still.is_empty() {
            return;
        }
        let victims: Vec<Proc> = started.into_iter().filter(|p| p.pid != root.pid && proctree::alive(*p)).collect();
        if victims.is_empty() {
            return;
        }
        let pids: Vec<i32> = victims.iter().map(|p| p.pid).collect();
        for v in &victims {
            proctree::kill_tree(*v, Duration::from_millis(500), &Kill);
        }
        let list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ");
        lock(&shared)
            .output(&format!("  [command killed: Codex left it running after the interrupt (pid {list})]\r\n"));
        sink.emit(DriverEvent::Step(json!({ "type": "command_killed", "items": still, "pids": pids })));
    });
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
            if ours && method == "turn/started" {
                let root = ctx.process.as_ref().and_then(|p| p.root);
                s.turn_baseline = root.map(|r| proctree::descendants(r).into_iter().collect());
            }
            if method == "item/completed" && item["type"] == "commandExecution" {
                if let Some(id) = item["id"].as_str() {
                    s.abandoned_open.remove(id);
                }
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
            if let Some(ids) = s.reap_due.take() {
                reap_abandoned(ctx, &mut s, ids);
            }
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
        Incoming::Request { id, method, params } => {
            let elicitation = method == "mcpServer/elicitation/request";
            if elicitation {
                let server = params["serverName"].as_str().unwrap_or("an MCP server");
                let what = text_of(&params["message"]).map(|m| format!(": {}", bounded(m, 200))).unwrap_or_default();
                lock(&ctx.shared).output(&format!(
                    "[mcp elicitation from {server} declined automatically (Mirante never answers one for you){what}]\r\n"
                ));
            }
            if let Some(conn) = ctx.conn.get() {
                // MCP elicitations are declined (never accepted on the user's behalf); other
                // server requests (user input, tool calls, auth refresh) are not supported.
                let _ = if elicitation {
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
        // It may have landed: reported, never handed back (it would be sent twice).
        Err(CallError::Timeout(e) | CallError::AfterWrite(e)) => {
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
        let process = Arc::new(Process::new(child, root, Some(stderr_log.to_path_buf())));
        process.watch();
        CodexDriver::connect_process(BufReader::new(stdout), stdin, Some(process), cwd, resume, sink, timing)
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
            granted_roots: Vec::new(),
            unconfirmed: HashMap::new(),
            rate_limits: None,
            token_usage: None,
            closed: false,
            turn_baseline: None,
            reap_due: None,
            abandoned_open: HashSet::new(),
        }));
        let ctx = Arc::new(Ctx {
            shared: shared.clone(),
            sink: sink.clone(),
            conn: OnceLock::new(),
            process: process.clone(),
            abandon_grace: timing.abandon_grace,
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
            Err(CallError::Rpc { code, message }) if resume.is_some() && is_native_gone(code, &message) => {
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
        sink.emit(DriverEvent::Sandbox(sandbox_summary(&result["sandbox"], &[])));
        let (wsh, wsink, wconn) = (shared.clone(), sink.clone(), conn.clone());
        std::thread::spawn(move || {
            for w in rx {
                match w {
                    Work::Send(text) => send_now(&wconn, &wsh, &wsink, &text, timing.request),
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

    /// Fails at once when app-server's output ended (nothing would ever answer).
    fn ensure_open(&self) -> Result<(), DriverError> {
        if lock(&self.shared).closed || self.conn.closed() {
            return Err(DriverError::Failed("codex app-server has exited (mirante resume starts it again)".into()));
        }
        Ok(())
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
            // The child lock is held so the exit cannot be reaped while the group is signaled.
            let child = lock(&p.child);
            p.end_tree(grace, child.is_some() && p.root.is_some_and(|r| pty::exited_unreaped(r.pid)));
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
        self.ensure_open()?;
        self.queue(Work::Send(text.to_string()))
    }

    fn interrupt(&self) -> Result<(), DriverError> {
        self.ensure_open()?;
        if lock(&self.shared).engine.turn_id().is_none() {
            return Err(DriverError::NotNow("no running turn to interrupt".into()));
        }
        if self.stopped.load(Ordering::SeqCst) {
            return Err(DriverError::Failed("driver stopped".into()));
        }
        // Its own thread: a send waiting on a stuck request must not delay "stop now".
        let (conn, shared, sink, timeout) =
            (self.conn.clone(), self.shared.clone(), self.sink.clone(), self.timing.request);
        std::thread::spawn(move || {
            // The turn running now, when the request goes out.
            let (thread, turn) = {
                let s = lock(&shared);
                (s.thread_id.clone(), s.engine.turn_id().map(str::to_string))
            };
            let Some(turn) = turn else {
                sink.emit(DriverEvent::Step(
                    json!({ "type": "interrupt_skipped", "reason": "the turn already ended" }),
                ));
                return;
            };
            let p = json!({ "threadId": thread, "turnId": turn });
            if let Err(e) = conn.request("turn/interrupt", p, timeout) {
                sink.emit(DriverEvent::Step(json!({ "type": "interrupt_failed", "error": e.to_string() })));
            }
        });
        Ok(())
    }

    fn answer_permission(
        &self,
        key: &str,
        _tool: &str,
        _description: Option<&str>,
        allow: bool,
        _guard: Option<super::AnswerGuard>,
    ) -> Result<(), DriverError> {
        let mut s = lock(&self.shared);
        let (id, method, params) =
            s.approvals.remove(key).ok_or_else(|| DriverError::NoDialog(format!("no pending approval {key}")))?;
        // Queued for the writer thread: never blocks on the child's stdin.
        if let Err(e) = self.conn.respond(&id, approval_response(&method, &params, allow)) {
            s.approvals.insert(key.to_string(), (id, method, params));
            return Err(DriverError::Failed(format!("answer not sent: {e}")));
        }
        let mut out = Vec::new();
        if let (false, Some(note)) = (allow, deny_decision(&params).1) {
            out.push(DriverEvent::Step(json!({ "type": "deny_decision", "method": method, "note": note })));
        }
        match params["grantRoot"].as_str().filter(|_| allow) {
            Some(root) => {
                // Honest about what it grants: this lasts for the rest of the session.
                s.output(&format!("[allowed: write access under {root} for the rest of the session]\r\n"));
                s.granted_roots.push(root.to_string());
                out.push(DriverEvent::Sandbox(sandbox_summary(&s.sandbox_policy, &s.granted_roots)));
            }
            None if allow => s.output("[allowed once]\r\n"),
            None if deny_decision(&params).0 == "cancel" => {
                s.output("[denied: cancel, Codex's listed \"no\", also ends the turn]\r\n")
            }
            None => s.output("[denied]\r\n"),
        }
        let t = s.engine.apply(&CodexInput::ApprovalAnswered(key.to_string()), Instant::now());
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
        let mut initial =
            format!("{MODE_BANNER}\r\n{}\r\n", sandbox_banner(&s.sandbox_policy, &s.granted_roots)).into_bytes();
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
            abandon_grace: Duration::from_millis(400),
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
                key: "n:41".into(),
                tool: "command".into(),
                detail: "rm -rf build".into()
            }
        );
        let ev = next_event(&rx, status_to(Status::NeedsYou));
        assert!(matches!(ev, DriverEvent::Status { turn_open: true, .. }));
        d.answer_permission("n:41", "command", None, true, None).unwrap();
        assert_eq!(fake.next(), json!({"jsonrpc": "2.0", "id": 41, "result": {"decision": "accept"}}));
        assert!(matches!(d.answer_permission("n:41", "command", None, true, None), Err(DriverError::NoDialog(_))));
        next_event(&rx, status_to(Status::Working));
        // Request id 0, then resolved by the server (e.g. answered elsewhere).
        fake.send(json!({"jsonrpc": "2.0", "id": 0, "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "i2", "startedAtMs": 1, "command": "ls"}}));
        next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { key, .. } if key == "n:0"));
        fake.notify("serverRequest/resolved", json!({"threadId": "th-1", "requestId": 0}));
        assert_eq!(
            next_event(&rx, |e| matches!(e, DriverEvent::PermissionResolved { .. })),
            DriverEvent::PermissionResolved { key: "n:0".into() }
        );
        next_event(&rx, status_to(Status::Working));
        assert!(matches!(d.answer_permission("n:0", "command", None, false, None), Err(DriverError::NoDialog(_))));
        // A request still open when its turn ends is withdrawn too.
        fake.send(json!({"jsonrpc": "2.0", "id": "r7", "method": "item/fileChange/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "f1", "startedAtMs": 1}}));
        next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { .. }));
        fake.notify("turn/completed", json!({"threadId": "th-1", "turn": {"id": "tu-1", "status": "completed"}}));
        assert_eq!(
            next_event(&rx, |e| matches!(e, DriverEvent::PermissionResolved { .. })),
            DriverEvent::PermissionResolved { key: "s:r7".into() }
        );
        next_event(&rx, status_to(Status::Idle));
    }

    #[test]
    fn a_deny_of_the_live_command_shape_sends_the_listed_cancel_and_logs_it() {
        let (d, mut fake, rx, _) = setup(None);
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
        fake.send(json!({"jsonrpc": "2.0", "id": 1, "method": "item/commandExecution/requestApproval",
            "params": {"kind": "command", "threadId": "th-1", "turnId": "tu-1", "itemId": "exec-fa21", "startedAtMs": 1,
                "environmentId": "local", "reason": "O sandbox bloqueou o comando.", "command": "/bin/zsh -lc 'touch /tmp/x'",
                "cwd": "/w", "commandActions": [{"type": "unknown", "command": "touch /tmp/x"}],
                "proposedExecpolicyAmendment": ["touch", "/tmp/x"],
                "availableDecisions": ["accept", {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["touch", "/tmp/x"]}}, "cancel"]}}));
        next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { .. }));
        d.answer_permission("n:1", "command", None, false, None).unwrap();
        assert_eq!(fake.next(), json!({"jsonrpc": "2.0", "id": 1, "result": {"decision": "cancel"}}));
        let step = next_event(&rx, |e| matches!(e, DriverEvent::Step(v) if v["type"] == "deny_decision"));
        assert!(matches!(step, DriverEvent::Step(ref v) if v["note"].as_str().unwrap().contains("cancel")));
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
                key: "n:2".into(),
                tool: "file_change".into(),
                detail: "update src/a.rs (+2 -1) — outside".into()
            }
        );
        fake.send(json!({"jsonrpc": "2.0", "id": 3, "method": "item/fileChange/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "f2", "startedAtMs": 1, "grantRoot": "/Users/u"}}));
        let card = next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { .. }));
        assert!(matches!(card, DriverEvent::PermissionRequested { ref tool, ref detail, .. }
            if tool == GRANT_TOOL && detail.contains("/Users/u for the rest of the session")));
    }

    #[test]
    fn a_deny_follows_the_decisions_codex_lists() {
        // Live shape (Codex 0.167.0): no "decline" listed.
        let live = json!({"kind": "command", "itemId": "exec-1", "command": "/bin/zsh -lc 'touch /x'",
            "availableDecisions": ["accept", {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["touch", "/x"]}}, "cancel"]});
        let m = "item/commandExecution/requestApproval";
        assert_eq!(deny_decision(&live), ("cancel", Some("\"decline\" is not in availableDecisions: sent \"cancel\"")));
        assert_eq!(approval_response(m, &live, false), json!({"decision": "cancel"}));
        assert_eq!(approval_response(m, &live, true), json!({"decision": "accept"}));
        // Listed: used as is.
        let listed = json!({"availableDecisions": ["accept", "decline", "cancel"]});
        assert_eq!(approval_response(m, &listed, false), json!({"decision": "decline"}));
        assert_eq!(deny_decision(&listed), ("decline", None));
        // No list (older Codex, file changes): decline.
        assert_eq!(
            approval_response("item/fileChange/requestApproval", &json!({}), false),
            json!({"decision": "decline"})
        );
        assert_eq!(deny_decision(&json!({})), ("decline", None));
        // A list with neither: decline still, and said.
        let odd = json!({"availableDecisions": ["accept"]});
        assert_eq!(approval_response(m, &odd, false), json!({"decision": "decline"}));
        assert!(deny_decision(&odd).1.is_some());
    }

    #[test]
    fn an_added_file_counts_its_content_lines() {
        // Live (Codex 0.167.0): an add's `diff` is the raw content.
        let live = json!([{"path": "/Users/u/m21-out/grant.txt", "kind": {"type": "add"}, "diff": "x\n"}]);
        assert_eq!(changes_summary(&live).unwrap(), "add /Users/u/m21-out/grant.txt (+1 -0)");
        let more = json!([
            {"path": "b.rs", "kind": {"type": "add"}, "diff": "fn a() {}\n-not a removal\n+not an add header\n"},
            {"path": "c.rs", "kind": {"type": "delete"}, "diff": "one\ntwo\n"},
            {"path": "d.rs", "kind": {"type": "add"}, "diff": "@@ -0,0 +1,2 @@\n+a\n+b\n"},
            {"path": "e.rs", "kind": {"type": "update"}, "diff": "@@ -1 +1 @@\n-old\n+new\n"},
        ]);
        assert_eq!(
            changes_summary(&more).unwrap(),
            "add b.rs (+3 -0), delete c.rs (+0 -2), add d.rs (+2 -0), update e.rs (+1 -1)"
        );
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
            DriverEvent::PermissionResolved { key: "n:0".into() }
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
    fn an_app_server_that_exits_on_its_own_takes_its_background_children_along() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let pids = dir.path().join("pids");
        // The children keep the server's stdout open (no EOF for the reader); one leaves the group.
        let rest = format!(
            "sleep 60 & echo $! > {p}\nperl -e 'setpgrp(0,0); sleep 60' & echo $! >> {p}\nsleep 1\nexit 0",
            p = pids.display()
        );
        let (_d, rx) = spawn_fake(dir.path(), &rest);
        let deadline = Instant::now() + Duration::from_secs(5);
        let kids: Vec<i32> = loop {
            let text = std::fs::read_to_string(&pids).unwrap_or_default();
            let v: Vec<i32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
            if v.len() == 2 {
                break v;
            }
            assert!(Instant::now() < deadline, "children never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        next_event(&rx, |e| matches!(e, DriverEvent::Exited { .. }));
        let deadline = Instant::now() + Duration::from_secs(5);
        for pid in kids {
            while proctree::proc_of(pid).is_some() {
                assert!(Instant::now() < deadline, "{pid} survived the app-server's own exit");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// A fake app-server that runs one command for turn `tu-1` (`cmd`, started 300 ms after
    /// `turn/started`, its pid written to `pids`) and answers `turn/interrupt` with an
    /// interrupted `turn/completed`; `after` runs then. It also starts a helper before the
    /// turn (pid on the second line), which must never be touched.
    fn interrupt_script(pids: &Path, cmd: &str, after: &str) -> String {
        let p = pids.display();
        format!(
            "sleep 60 & echo $! > {p}.pre\n\
             read l; printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{\"turn\":{{\"id\":\"tu-1\"}}}}}}'\n\
             printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"turn/started\",\"params\":{{\"threadId\":\"th-1\",\"turn\":{{\"id\":\"tu-1\"}}}}}}'\n\
             sleep 0.3\n\
             {cmd} & echo $! > {p}\n\
             printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"item/started\",\"params\":{{\"threadId\":\"th-1\",\"turnId\":\"tu-1\",\"item\":{{\"type\":\"commandExecution\",\"id\":\"c1\",\"command\":\"sleep 30\"}}}}}}'\n\
             read l; printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{{}}}}'\n\
             printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"turn/completed\",\"params\":{{\"threadId\":\"th-1\",\"turn\":{{\"id\":\"tu-1\",\"status\":\"interrupted\"}}}}}}'\n\
             {after}\n\
             while :; do sleep 0.1; done"
        )
    }

    fn pid_in(file: &Path) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = std::fs::read_to_string(file).ok().and_then(|t| t.trim().parse().ok()) {
                return pid;
            }
            assert!(Instant::now() < deadline, "{} never written", file.display());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn an_interrupt_kills_the_command_codex_leaves_running_and_nothing_else() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let pids = dir.path().join("pids");
        let (d, rx) = spawn_fake(dir.path(), &interrupt_script(&pids, "sleep 30", ""));
        let root = d.root().expect("spawned");
        let pre = proctree::proc_of(pid_in(&pids.with_extension("pre"))).unwrap();
        d.send("go").unwrap();
        let cmd = proctree::proc_of(pid_in(&pids)).unwrap();
        next_event(&rx, status_to(Status::Working));
        d.interrupt().unwrap();
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Step(v) if v["type"] == "command_killed"));
        let DriverEvent::Step(v) = ev else { unreachable!() };
        assert_eq!(v["items"], json!(["c1"]));
        assert!(v["pids"].as_array().unwrap().contains(&json!(cmd.pid)), "{v}");
        assert!(!v["pids"].as_array().unwrap().contains(&json!(root.pid)), "{v}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while proctree::alive(cmd) {
            assert!(Instant::now() < deadline, "the abandoned command survived");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(proctree::alive(root), "app-server itself is never killed");
        assert!(proctree::alive(pre), "a process older than the turn is never killed");
        assert!(d.screen_tail().contains("[command killed"), "{}", d.screen_tail());
        d.stop();
    }

    #[test]
    fn a_command_that_finishes_within_the_grace_is_left_alone() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let pids = dir.path().join("pids");
        let done = "printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"method\":\"item/completed\",\"params\":{\"threadId\":\"th-1\",\"turnId\":\"tu-1\",\"item\":{\"type\":\"commandExecution\",\"id\":\"c1\",\"exitCode\":0}}}'";
        // A process of the turn that keeps running (e.g. a server the command started) is
        // not killed once Codex said the command ended.
        let (d, rx) = spawn_fake(dir.path(), &interrupt_script(&pids, "sleep 30", done));
        let root = d.root().expect("spawned");
        d.send("go").unwrap();
        let cmd = proctree::proc_of(pid_in(&pids)).unwrap();
        next_event(&rx, status_to(Status::Working));
        d.interrupt().unwrap();
        std::thread::sleep(timing().abandon_grace + Duration::from_millis(600));
        let killed = rx.try_iter().any(|(_, e)| matches!(e, DriverEvent::Step(ref v) if v["type"] == "command_killed"));
        assert!(!killed);
        assert!(proctree::alive(cmd) && proctree::alive(root));
        d.stop();
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

    fn stream(a: &Attachment, until: &str) -> String {
        let mut seen = String::new();
        while !seen.contains(until) {
            let chunk =
                a.output.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| panic!("{until:?} not in {seen:?}"));
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
        seen
    }

    #[test]
    fn an_allowed_grant_root_says_it_lasts_for_the_session() {
        let (d, mut fake, rx, _) = setup(None);
        let a = d.attach(24, 80).unwrap();
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
        fake.send(json!({"jsonrpc": "2.0", "id": 3, "method": "item/fileChange/requestApproval",
            "params": {"threadId": "th-1", "turnId": "tu-1", "itemId": "f2", "startedAtMs": 1, "grantRoot": "/Users/u/data"}}));
        next_event(&rx, |e| matches!(e, DriverEvent::PermissionRequested { tool, .. } if tool == GRANT_TOOL));
        d.answer_permission("n:3", GRANT_TOOL, None, true, None).unwrap();
        assert_eq!(fake.next(), json!({"jsonrpc": "2.0", "id": 3, "result": {"decision": "accept"}}));
        stream(&a, "[allowed: write access under /Users/u/data for the rest of the session]");
        // The effective sandbox shown everywhere now includes the granted root.
        let ev = next_event(&rx, |e| matches!(e, DriverEvent::Sandbox(s) if s.contains("granted")));
        assert_eq!(ev, DriverEvent::Sandbox("net:off, +1 granted root".into()));
    }

    #[test]
    fn native_gone_is_recognised_by_its_text_or_by_code_and_wording() {
        assert!(is_native_gone(-32600, "no rollout found for thread id x"));
        assert!(is_native_gone(-32603, "thread x not found"));
        assert!(is_native_gone(-32603, "failed to load rollout"));
        assert!(!is_native_gone(-32603, "internal error"));
        assert!(!is_native_gone(-32600, "thread not found"), "not-found wording counts only with -32603");
        let (d, _fake, _rx, _) = connect_with(Some("th-gone"), |f, t| {
            f.send(json!({"jsonrpc": "2.0", "id": t["id"], "error": {"code": -32603, "message": "thread th-gone not found"}}))
        });
        assert!(matches!(d, Err(DriverError::NativeGone(_))), "{:?}", d.err());
    }

    #[test]
    fn interrupt_is_not_delayed_by_a_hung_send() {
        let (d, mut fake, rx, _) = setup(None);
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
        next_event(&rx, status_to(Status::Working));
        d.send("more").unwrap();
        fake.expect("turn/steer"); // never answered: the send waits for its timeout
        let asked = Instant::now();
        d.interrupt().unwrap();
        let int = fake.expect("turn/interrupt");
        assert!(asked.elapsed() < Duration::from_secs(1), "the interrupt waited {:?}", asked.elapsed());
        assert_eq!(int["params"], json!({"threadId": "th-1", "turnId": "tu-1"}));
    }

    #[test]
    fn a_closed_server_fails_sends_and_interrupts_at_once() {
        let (d, fake, rx, _) = setup(None);
        drop(fake);
        next_event(&rx, |e| matches!(e, DriverEvent::Exited { .. }));
        assert!(matches!(d.send("x"), Err(DriverError::Failed(ref m)) if m.contains("exited")));
        assert!(matches!(d.interrupt(), Err(DriverError::Failed(ref m)) if m.contains("exited")));
    }

    #[test]
    fn a_send_whose_connection_ended_after_the_write_is_unconfirmed_not_requeued() {
        let (d, mut fake, rx, _) = setup(None);
        d.send("did it land?").unwrap();
        fake.expect("turn/start");
        drop(fake);
        // The send's outcome and the exit come from different threads: collect both.
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !seen.iter().any(|e| matches!(e, DriverEvent::Step(v) if v["type"] == "send_unconfirmed"))
            || !seen.iter().any(|e| matches!(e, DriverEvent::Exited { .. }))
        {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok((_, e)) => seen.push(e),
                Err(_) => break,
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        seen.extend(rx.try_iter().map(|(_, e)| e));
        assert!(seen.iter().any(|e| matches!(e, DriverEvent::Step(v) if v["type"] == "send_unconfirmed")), "{seen:?}");
        assert!(!seen.iter().any(|e| matches!(e, DriverEvent::DeliveryAborted { .. })), "{seen:?}");
    }

    #[test]
    fn reap_does_not_hold_the_child_lock_while_waiting() {
        let child = Command::new("/bin/sh").args(["-c", "sleep 0.5; exit 4"]).process_group(0).spawn().unwrap();
        let root = proctree::proc_of(child.id() as i32);
        let p = Arc::new(Process::new(child, root, None));
        let (p2, p3) = (p.clone(), p.clone());
        let first = std::thread::spawn(move || p2.reap(Duration::from_secs(3)));
        std::thread::sleep(Duration::from_millis(100));
        assert!(p.child.try_lock().is_ok(), "the child mutex is free while the reap waits");
        let second = std::thread::spawn(move || p3.reap(Duration::from_secs(3)));
        assert_eq!(first.join().unwrap(), Some(4));
        assert_eq!(second.join().unwrap(), Some(4), "a concurrent reap gets the same exit code");
    }

    #[test]
    fn a_child_turn_started_keeps_our_turn_and_the_steer_uses_it() {
        let (d, mut fake, rx, _) = setup(None);
        fake.notify("turn/started", json!({"threadId": "th-1", "turn": {"id": "tu-1"}}));
        next_event(&rx, status_to(Status::Working));
        fake.notify("turn/started", json!({"threadId": "th-child", "turn": {"id": "tc-9"}}));
        std::thread::sleep(Duration::from_millis(100));
        d.send("go on").unwrap();
        let steer = fake.expect("turn/steer");
        assert_eq!(steer["params"]["expectedTurnId"], "tu-1");
    }

    #[test]
    fn the_effective_sandbox_is_in_the_banner_and_reported() {
        let (d, _fake, rx, _) = setup(None);
        assert_eq!(next_event(&rx, |e| matches!(e, DriverEvent::Sandbox(_))), DriverEvent::Sandbox("net:off".into()));
        let a = d.attach(24, 80).unwrap();
        let banner = String::from_utf8_lossy(&a.initial).into_owned();
        assert!(
            banner.contains("effective sandbox: workspace-write, network off, writable: the worktree only"),
            "{banner}"
        );
        let (d, _fake, rx, _) = connect_with(None, |f, t| {
            let mut r = thread_ok("th-1");
            r["sandbox"] = json!({"type": "workspaceWrite", "writableRoots": ["/opt/cache"], "networkAccess": true});
            f.reply(t, r)
        });
        let d = d.unwrap();
        assert_eq!(
            next_event(&rx, |e| matches!(e, DriverEvent::Sandbox(_))),
            DriverEvent::Sandbox("net:on, +1 root".into())
        );
        let banner = String::from_utf8_lossy(&d.attach(24, 80).unwrap().initial).into_owned();
        assert!(banner.contains("network ON") && banner.contains("writable: the worktree + /opt/cache"), "{banner}");
        assert_eq!(sandbox_summary(&json!({"type": "workspaceWrite"}), &[]), "net:?");
    }

    #[test]
    fn elicitations_are_declined_and_the_stream_says_so() {
        let (d, mut fake, _rx, _) = setup(None);
        let a = d.attach(24, 80).unwrap();
        fake.send(json!({"jsonrpc": "2.0", "id": "e1", "method": "mcpServer/elicitation/request",
            "params": {"serverName": "github", "message": "Paste a token"}}));
        assert_eq!(fake.next(), json!({"jsonrpc": "2.0", "id": "e1", "result": {"action": "decline"}}));
        let seen = stream(&a, "declined");
        assert!(seen.contains("[mcp elicitation from github declined automatically"), "{seen}");
    }
}
