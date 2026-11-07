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
    /// The daemon is at its connection limit; retry shortly.
    pub const BUSY: i64 = 7;
    /// Mirante will not type this answer (Claude: inline answers are off, or the evidence
    /// that the dialog on screen is this card's is missing). Nothing was typed; the user
    /// answers it in the agent's terminal (`mirante attach <task>`).
    pub const ANSWER_IN_TERMINAL: i64 = 8;
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
