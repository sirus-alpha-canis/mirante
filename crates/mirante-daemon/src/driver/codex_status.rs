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
//!   (`serverRequest/resolved`) or voided by the end of the turn that raised them; a request
//!   that names no turn (the legacy `execCommandApproval` / `applyPatchApproval`) is voided
//!   only by a thread `systemError` or the process ending. The keys voided here are handed
//!   to the driver ([`CodexEngine::take_resolved`]). Keys are typed (`n:` number, `s:`
//!   string), so request `1` and request `"1"` never collide.
//! - Errors (`error` notifications, a failed turn's `turn.error`, a thread `systemError`)
//!   become the status detail; a "retrying" one is cleared when the turn completes.
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

/// A request key as Mirante stores it, typed: `n:<number>`, `s:<string>` (anything else,
/// which JSON-RPC does not allow, `j:<json>`). Used for every request id, including the one
/// in `serverRequest/resolved`.
pub fn request_key(id: &Value) -> String {
    match id {
        Value::Number(n) => format!("n:{n}"),
        Value::String(s) => format!("s:{s}"),
        other => format!("j:{other}"),
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

    /// `Some(turn)`: the requests that turn raised (a request without a turn is kept);
    /// `None`: every request (the thread failed or the process ended).
    fn void_approvals(&mut self, turn: Option<&str>) {
        let gone: Vec<String> = self
            .approvals
            .iter()
            .filter(|(_, t)| turn.is_none() || (t.is_some() && t.as_deref() == turn))
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
                        if let Some(id) = id.as_deref() {
                            self.void_approvals(Some(id));
                        }
                        if self.error.as_deref().is_some_and(|e| e.starts_with("retrying:")) {
                            // The retry got through (a failed turn sets its own error below).
                            self.error = None;
                        }
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
        e.apply(&ask("n:0"), t0);
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
    fn request_keys_are_typed_so_a_string_and_a_number_never_collide() {
        assert_eq!(request_key(&json!(0)), "n:0");
        assert_eq!(request_key(&json!("e1")), "s:e1");
        assert_ne!(request_key(&json!("1")), request_key(&json!(1)));
        // The resolved notification uses the same keys.
        let (mut e, t0) = engine();
        e.apply(&CodexInput::ApprovalRequested { key: request_key(&json!("7")), turn: None }, t0);
        let other = json!({"threadId": "th1", "requestId": 7});
        assert_eq!(e.apply(&n("serverRequest/resolved", &other), t0), None, "number 7 is not string \"7\"");
        let same = json!({"threadId": "th1", "requestId": "7"});
        assert_eq!(e.apply(&n("serverRequest/resolved", &same), t0).unwrap().to, Status::Idle);
    }

    #[test]
    fn legacy_requests_without_a_turn_are_voided_only_when_the_thread_or_process_ends() {
        let (mut e, t0) = engine();
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu1"}})), t0);
        // A legacy `execCommandApproval` names no turn.
        e.apply(&CodexInput::ApprovalRequested { key: "n:3".into(), turn: None }, t0);
        e.apply(&n("turn/completed", &json!({"threadId": "th1", "turn": {"id": "tu1", "status": "completed"}})), t0);
        assert!(e.take_resolved().is_empty(), "a turn ending does not void a turnless request");
        assert_eq!(e.status(), Status::NeedsYou);
        let sys = json!({"threadId": "th1", "status": {"type": "systemError"}});
        e.apply(&n("thread/status/changed", &sys), t0);
        assert_eq!(e.take_resolved(), vec!["n:3".to_string()]);
        e.apply(&CodexInput::ApprovalRequested { key: "n:4".into(), turn: None }, t0);
        e.apply(&CodexInput::Closed, t0);
        assert_eq!(e.take_resolved(), vec!["n:4".to_string()]);
    }

    #[test]
    fn a_retry_error_is_cleared_when_the_turn_completes() {
        let (mut e, t0) = engine();
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu1"}})), t0);
        let retry = json!({"threadId": "th1", "willRetry": true, "error": {"message": "stream dropped"}});
        e.apply(&n("error", &retry), t0);
        assert_eq!(e.detail().as_deref(), Some("retrying: stream dropped"));
        e.apply(&n("turn/completed", &json!({"threadId": "th1", "turn": {"id": "tu1", "status": "completed"}})), t0);
        assert_eq!(e.detail(), None, "the retry worked: nothing to show");
        // A fatal error stays until the next turn.
        e.apply(&n("turn/started", &json!({"threadId": "th1", "turn": {"id": "tu2"}})), t0);
        e.apply(&n("error", &json!({"threadId": "th1", "willRetry": false, "error": {"message": "limit"}})), t0);
        e.apply(&n("turn/completed", &json!({"threadId": "th1", "turn": {"id": "tu2", "status": "completed"}})), t0);
        assert_eq!(e.detail().as_deref(), Some("error: limit"));
    }
}
