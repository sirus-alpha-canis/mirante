//! ACP (Agent Client Protocol) spike helpers: script placeholders, the steer
//! classifier, protocol-only ground-truth status, and the scripted permission answer.
//! Clean-room from the public spec at agentclientprotocol.com (protocol version 1).
//!
//! Logs are the `{ts, dir, line}` JSONL that `spike-acp` writes: `dir` is `in` for
//! client→agent and `out` for agent→client.
use serde_json::Value;

use crate::status::Status;

/// Fills `$CWD`, `$SESSION` and `$ECHO` in a script message.
/// Err when the template uses `$SESSION` but no session id was captured.
pub fn fill(template: &str, cwd: &str, session: Option<&str>, echo: &str) -> Result<String, String> {
    let mut s = template.replace("$CWD", cwd).replace("$ECHO", echo);
    if s.contains("$SESSION") {
        let v = session.ok_or("template uses $SESSION but no session id was captured")?;
        s = s.replace("$SESSION", v);
    }
    Ok(s)
}

/// The `sessionId` from the first `session/new` (or `session/load`/`resume`) result.
pub fn capture_session(log: &[Value]) -> Option<String> {
    log.iter()
        .filter(|v| v["dir"] == "out")
        .find_map(|v| v.pointer("/line/result/sessionId").and_then(Value::as_str).map(str::to_string))
}

fn is_request(v: &Value, dir: &str, method: &str) -> bool {
    v["dir"] == dir && v["line"]["method"] == method && v["line"].get("id").is_some()
}

/// A response line (no `method`) travelling in `dir` for request `id`.
fn is_response(v: &Value, dir: &str, id: &Value) -> bool {
    v["dir"] == dir && v["line"].get("method").is_none() && &v["line"]["id"] == id
}

/// What happened to a second `session/prompt` sent while the first was still open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Steer {
    /// The agent answered the second prompt with a JSON-RPC error.
    Rejected {
        code: i64,
        message: String,
    },
    /// The first prompt ran to its end; the second ran after it.
    Queued,
    /// The second prompt finished while the first was still open.
    Concurrent,
    /// The first prompt ended `cancelled` without a client `session/cancel`.
    Preempted,
    Unknown(String),
}

/// Classifies by file order (not timestamps): the first prompt A, the first prompt B
/// sent before A's response, and how each was answered.
pub fn classify_steer(log: &[Value]) -> Steer {
    let prompts: Vec<(usize, &Value)> = log
        .iter()
        .enumerate()
        .filter(|(_, v)| is_request(v, "in", "session/prompt"))
        .map(|(i, v)| (i, &v["line"]["id"]))
        .collect();
    let Some(&(_, a_id)) = prompts.first() else {
        return Steer::Unknown("no session/prompt".into());
    };
    let a_resp = log.iter().position(|v| is_response(v, "out", a_id));
    let Some(&(b_at, b_id)) = prompts.get(1).filter(|(i, _)| a_resp.is_none_or(|r| *i < r)) else {
        return Steer::Unknown("no second prompt while the first was open".into());
    };
    let b_resp = log.iter().position(|v| is_response(v, "out", b_id));
    if let Some(err) = b_resp.and_then(|i| log[i]["line"].get("error")) {
        return Steer::Rejected {
            code: err["code"].as_i64().unwrap_or(0),
            message: err["message"].as_str().unwrap_or("").to_string(),
        };
    }
    let (Some(a), Some(b)) = (a_resp, b_resp) else {
        return Steer::Unknown("a prompt never got a response".into());
    };
    let client_cancelled = log[b_at..a].iter().any(|v| v["dir"] == "in" && v["line"]["method"] == "session/cancel");
    if log[a]["line"]["result"]["stopReason"] == "cancelled" && !client_cancelled {
        return Steer::Preempted;
    }
    if b < a {
        Steer::Concurrent
    } else {
        Steer::Queued
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub ts_ms: u64,
    pub event: String,
    pub status: Status,
}

/// Status from protocol traffic only: a prompt sent → working; a pending
/// `session/request_permission` → needs_you; every prompt answered → idle; the
/// process gone (`dir: "exit"`) → ended. Only changes are returned.
pub fn ground_truth(log: &[Value]) -> Vec<Point> {
    let mut prompts: Vec<Value> = Vec::new();
    let mut perms: Vec<Value> = Vec::new();
    let mut current: Option<Status> = None;
    let mut out = Vec::new();
    for v in log {
        let line = &v["line"];
        let method = line["method"].as_str();
        let event = match (v["dir"].as_str(), method) {
            (Some("exit"), _) => "exit".to_string(),
            (Some("in"), Some("session/prompt")) => {
                prompts.push(line["id"].clone());
                "session/prompt".into()
            }
            (Some("out"), Some("session/request_permission")) if line.get("id").is_some() => {
                perms.push(line["id"].clone());
                "session/request_permission".into()
            }
            (Some("in"), None) if perms.contains(&line["id"]) => {
                perms.retain(|p| p != &line["id"]);
                "permission answered".into()
            }
            (Some("out"), None) if prompts.contains(&line["id"]) => {
                prompts.retain(|p| p != &line["id"]);
                format!("prompt response {}", line["result"]["stopReason"].as_str().unwrap_or("error"))
            }
            _ => continue,
        };
        let next = if event == "exit" {
            Status::Ended
        } else if !perms.is_empty() {
            Status::NeedsYou
        } else if !prompts.is_empty() {
            Status::Working
        } else {
            Status::Idle
        };
        if current != Some(next) {
            out.push(Point { ts_ms: v["ts"].as_u64().unwrap_or(0), event, status: next });
            current = Some(next);
        }
    }
    out
}

/// How the spike answers `session/request_permission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    AllowOnce,
    RejectOnce,
    Cancel,
    /// Leave it pending (the script cancels the turn).
    Hold,
}

impl std::str::FromStr for Answer {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "allow_once" => Ok(Self::AllowOnce),
            "reject_once" => Ok(Self::RejectOnce),
            "cancel" => Ok(Self::Cancel),
            "hold" => Ok(Self::Hold),
            _ => Err(format!("unknown answer {s:?} (allow_once|reject_once|cancel|hold)")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Selected(String),
    Cancelled,
}

impl Reply {
    /// The `result` of a `session/request_permission` response.
    pub fn result(&self) -> Value {
        match self {
            Reply::Selected(id) => serde_json::json!({"outcome": {"outcome": "selected", "optionId": id}}),
            Reply::Cancelled => serde_json::json!({"outcome": {"outcome": "cancelled"}}),
        }
    }
}

/// Picks the option whose `kind` is exactly `allow_once` / `reject_once`. An
/// `*_always` option is never chosen: when the once-kind is missing the reply is
/// `cancelled`. `Hold` answers nothing.
pub fn choose(options: &[Value], answer: Answer) -> Option<Reply> {
    let kind = match answer {
        Answer::AllowOnce => "allow_once",
        Answer::RejectOnce => "reject_once",
        Answer::Cancel => return Some(Reply::Cancelled),
        Answer::Hold => return None,
    };
    Some(
        options
            .iter()
            .find(|o| o["kind"] == kind)
            .and_then(|o| o["optionId"].as_str())
            .map_or(Reply::Cancelled, |id| Reply::Selected(id.to_string())),
    )
}

/// The reply of `spike-mcp-echo` (a stdio MCP server, newline-delimited JSON-RPC) to one
/// message. Notifications get none. Two tools: `echo_read` (`readOnlyHint: true`) and
/// `echo_write` (`readOnlyHint: false`); both return `ECHO:<text>`.
pub fn mcp_echo_reply(msg: &Value) -> Option<Value> {
    use serde_json::json;
    let id = msg.get("id")?.clone();
    let result = match msg["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "spike-mcp-echo", "version": "0.1.0"}
        }),
        "ping" => json!({}),
        "tools/list" => {
            let schema = json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]});
            json!({"tools": [
                {"name": "echo_read", "description": "Echo text back. Reads nothing, changes nothing.",
                 "inputSchema": schema, "annotations": {"readOnlyHint": true}},
                {"name": "echo_write", "description": "Pretend to write text somewhere (it only echoes).",
                 "inputSchema": schema, "annotations": {"readOnlyHint": false, "destructiveHint": false}}
            ]})
        }
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            if name != "echo_read" && name != "echo_write" {
                return Some(
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": format!("unknown tool {name}")}}),
                );
            }
            let text = msg["params"]["arguments"]["text"].as_str().unwrap_or("");
            json!({"content": [{"type": "text", "text": format!("ECHO:{text}")}], "isError": false})
        }
        m => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {m}")}}),
            )
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inn(ts: u64, line: Value) -> Value {
        json!({"ts": ts, "dir": "in", "line": line})
    }
    fn out(ts: u64, line: Value) -> Value {
        json!({"ts": ts, "dir": "out", "line": line})
    }
    fn prompt(id: i64, text: &str) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{"sessionId":"s1","prompt":[{"type":"text","text":text}]}})
    }
    fn done(id: i64, reason: &str) -> Value {
        json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":reason}})
    }

    #[test]
    fn fills_session_and_cwd_placeholders() {
        let t = r#"{"sessionId":"$SESSION","cwd":"$CWD","command":"$ECHO"}"#;
        assert_eq!(
            fill(t, "/tmp/acp-spike/r", Some("ses_1"), "/bin/echo-mcp").unwrap(),
            r#"{"sessionId":"ses_1","cwd":"/tmp/acp-spike/r","command":"/bin/echo-mcp"}"#
        );
        assert!(fill(r#"{"sessionId":"$SESSION"}"#, "/x", None, "/e").is_err());
        assert_eq!(fill(r#"{"cwd":"$CWD"}"#, "/x", None, "/e").unwrap(), r#"{"cwd":"/x"}"#);
    }

    #[test]
    fn classify_steer_sees_second_prompt_error_or_queue_or_concurrent() {
        // Rejected: the second prompt gets a JSON-RPC error while the first is open.
        let rejected = vec![
            inn(0, prompt(3, "five steps")),
            inn(6000, prompt(4, "stop, say BANANA")),
            out(6010, json!({"jsonrpc":"2.0","id":4,"error":{"code":-32603,"message":"busy"}})),
            out(20000, done(3, "end_turn")),
        ];
        assert_eq!(classify_steer(&rejected), Steer::Rejected { code: -32603, message: "busy".into() });

        // Queued: the second prompt is answered after the first one, which ran to its end.
        let queued = vec![
            inn(0, prompt(3, "five steps")),
            inn(6000, prompt(4, "stop, say BANANA")),
            out(20000, done(3, "end_turn")),
            out(22000, done(4, "end_turn")),
        ];
        assert_eq!(classify_steer(&queued), Steer::Queued);

        // Concurrent: the second prompt finishes while the first is still open.
        let concurrent = vec![
            inn(0, prompt(3, "five steps")),
            inn(6000, prompt(4, "stop, say BANANA")),
            out(8000, done(4, "end_turn")),
            out(20000, done(3, "end_turn")),
        ];
        assert_eq!(classify_steer(&concurrent), Steer::Concurrent);

        // Preempted: the first prompt ends `cancelled` though the client never sent session/cancel.
        let preempted = vec![
            inn(0, prompt(3, "five steps")),
            inn(6000, prompt(4, "stop, say BANANA")),
            out(6100, done(3, "cancelled")),
            out(9000, done(4, "end_turn")),
        ];
        assert_eq!(classify_steer(&preempted), Steer::Preempted);

        assert!(matches!(classify_steer(&[inn(0, prompt(3, "x"))]), Steer::Unknown(_)));
    }

    #[test]
    fn ground_truth_status_from_log() {
        let log = vec![
            inn(0, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})),
            out(10, json!({"jsonrpc":"2.0","id":1,"result":{}})),
            inn(100, prompt(3, "edit a file")),
            out(
                200,
                json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk"}}}),
            ),
            out(
                300,
                json!({"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"s1","options":[]}}),
            ),
            inn(800, json!({"jsonrpc":"2.0","id":0,"result":{"outcome":{"outcome":"selected","optionId":"once"}}})),
            out(
                900,
                json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"tool_call_update"}}}),
            ),
            out(1000, done(3, "end_turn")),
            json!({"ts": 1100, "dir": "exit", "line": {"code": 0}}),
        ];
        let got: Vec<(u64, Status)> = ground_truth(&log).into_iter().map(|p| (p.ts_ms, p.status)).collect();
        assert_eq!(
            got,
            vec![
                (100, Status::Working),
                (300, Status::NeedsYou),
                (800, Status::Working),
                (1000, Status::Idle),
                (1100, Status::Ended),
            ]
        );
    }

    #[test]
    fn ground_truth_cancel_settles_pending_permissions() {
        let log = vec![
            inn(0, prompt(3, "touch a file")),
            out(
                100,
                json!({"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{"sessionId":"s1","options":[]}}),
            ),
            inn(500, json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}})),
            inn(510, json!({"jsonrpc":"2.0","id":7,"result":{"outcome":{"outcome":"cancelled"}}})),
            out(700, done(3, "cancelled")),
        ];
        let got: Vec<Status> = ground_truth(&log).into_iter().map(|p| p.status).collect();
        assert_eq!(got, vec![Status::Working, Status::NeedsYou, Status::Working, Status::Idle]);
    }

    #[test]
    fn permission_script_picks_option_by_kind_never_always() {
        let opts = vec![
            json!({"optionId":"always","name":"Always allow","kind":"allow_always"}),
            json!({"optionId":"once","name":"Allow once","kind":"allow_once"}),
            json!({"optionId":"never","name":"Reject always","kind":"reject_always"}),
            json!({"optionId":"reject","name":"Reject","kind":"reject_once"}),
        ];
        assert_eq!(choose(&opts, Answer::AllowOnce), Some(Reply::Selected("once".into())));
        assert_eq!(choose(&opts, Answer::RejectOnce), Some(Reply::Selected("reject".into())));
        assert_eq!(choose(&opts, Answer::Cancel), Some(Reply::Cancelled));
        assert_eq!(choose(&opts, Answer::Hold), None);
        // Only "always" variants on offer: never pick them, cancel instead.
        let always_only =
            vec![json!({"optionId":"a","kind":"allow_always"}), json!({"optionId":"r","kind":"reject_always"})];
        assert_eq!(choose(&always_only, Answer::AllowOnce), Some(Reply::Cancelled));
        assert_eq!(choose(&always_only, Answer::RejectOnce), Some(Reply::Cancelled));
    }
    #[test]
    fn mcp_echo_serves_a_read_and_a_write_tool() {
        assert_eq!(mcp_echo_reply(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})), None);
        let init = mcp_echo_reply(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        let list = mcp_echo_reply(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "echo_read");
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
        assert_eq!(tools[1]["name"], "echo_write");
        assert_eq!(tools[1]["annotations"]["readOnlyHint"], false);
        let call = mcp_echo_reply(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo_write","arguments":{"text":"hi"}}})).unwrap();
        assert_eq!(call["result"]["content"][0]["text"], "ECHO:hi");
        let bad = mcp_echo_reply(&json!({"jsonrpc":"2.0","id":4,"method":"nope"})).unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }
}
