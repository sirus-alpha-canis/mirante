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
