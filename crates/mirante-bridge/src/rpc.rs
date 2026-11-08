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
    // The editable queue (spec §2.4): list, edit, remove, reorder queued messages.
    method::MESSAGE_LIST_QUEUED,
    method::MESSAGE_EDIT,
    method::MESSAGE_REMOVE,
    method::MESSAGE_REORDER,
    method::PERMISSION_LIST,
    method::PERMISSION_ANSWER,
    method::DAEMON_STATUS,
    method::SESSION_HIBERNATE,
    method::SESSION_WAKE,
    // Repo memory (DNA). Never `dna.propose`: agents propose through their MCP server and
    // people write notes in their editor; the cockpit only reviews, confirms and retires.
    method::DNA_STATUS,
    method::DNA_ENABLE,
    method::DNA_DISABLE,
    method::DNA_GRAPH,
    method::DNA_SEARCH,
    method::DNA_GET,
    method::DNA_CHECK,
    method::DNA_TASK_CONTEXT,
    method::DNA_PROPOSALS,
    method::DNA_REVIEW,
    method::DNA_NOTE_ACTION,
    // Skills. Never `skill.propose`: agents propose through their `mirante-skills` MCP
    // server; a person imports or writes one with `skill.add`. Never `skill.may_review`
    // (the CLI's check before prompting). The daemon still requires a person for
    // learn, confirm_origin, mine, add, review and action.
    method::SKILL_TENANTS,
    method::SKILL_LEARN,
    method::SKILL_LIST,
    method::SKILL_GET,
    method::SKILL_TASK_SKILLS,
    method::SKILL_CONFIRM_ORIGIN,
    method::SKILL_LINT,
    method::SKILL_MINE,
    method::SKILL_ADD,
    method::SKILL_PROPOSALS,
    method::SKILL_REVIEW,
    method::SKILL_ACTION,
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
    /// A write to the daemon did not complete in time (the daemon is stalled).
    Timeout { message: String },
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::Disconnected { message } => write!(f, "daemon not reachable: {message}"),
            BridgeError::Rpc { code, message } => write!(f, "{message} (code {code})"),
            BridgeError::Protocol { message } => write!(f, "daemon protocol: {message}"),
            BridgeError::Forbidden { method } => write!(f, "{method} is not allowed from the cockpit"),
            BridgeError::Timeout { message } => write!(f, "daemon timed out: {message}"),
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

    /// The cockpit's whole reach into the daemon, pinned: widening it must be a deliberate
    /// edit of this list (and of `apps/cockpit/src/security.test.ts`).
    #[test]
    fn the_allowlist_is_exactly_these_methods() {
        let expected = [
            "task.create",
            "task.list",
            "task.get",
            "task.archive",
            "task.events",
            "task.diff",
            "session.start",
            "session.resume",
            "session.trust",
            "session.stop",
            "session.interrupt",
            "session.resize",
            "message.send",
            "message.list_queued",
            "message.edit",
            "message.remove",
            "message.reorder",
            "permission.list",
            "permission.answer",
            "daemon.status",
            "session.hibernate",
            "session.wake",
            "dna.status",
            "dna.enable",
            "dna.disable",
            "dna.graph",
            "dna.search",
            "dna.get",
            "dna.check",
            "dna.task_context",
            "dna.proposals",
            "dna.review",
            "dna.note_action",
            "skill.tenants",
            "skill.learn",
            "skill.list",
            "skill.get",
            "skill.task_skills",
            "skill.confirm_origin",
            "skill.lint",
            "skill.mine",
            "skill.add",
            "skill.proposals",
            "skill.review",
            "skill.action",
        ];
        assert_eq!(ALLOWED, expected);
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
            method::DNA_PROPOSE,
            // Agents propose skills through their MCP server; the CLI asks may_review.
            method::SKILL_PROPOSE,
            method::SKILL_MAY_REVIEW,
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
