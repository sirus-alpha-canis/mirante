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
        crate::paths::validate_socket_path(socket)?;
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
            match resp.id {
                Some(rid) if rid == id => {}
                // The daemon could not parse our request (or answered without an id): the
                // error is not about some other call, so surface it instead of waiting.
                None => {
                    return Err(match resp.error {
                        Some(e) => ClientError::Rpc(e),
                        None => ClientError::Protocol("response without an id".into()),
                    })
                }
                Some(_) => continue,
            }
            if let Some(e) = resp.error {
                return Err(ClientError::Rpc(e));
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
    ///
    /// Read and write timeouts are reset to `None`: an attached stream is long-lived and
    /// idle for long stretches, so a leftover `set_timeout` would kill it.
    pub fn into_raw(self) -> (BufReader<UnixStream>, UnixStream) {
        let _ = self.writer.set_read_timeout(None);
        let _ = self.writer.set_write_timeout(None);
        (self.reader, self.writer)
    }
}
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

    #[test]
    fn a_response_without_an_id_is_the_parse_error_for_our_request() {
        let d = TestDir::new();
        let sock = d.path().join("s.sock");
        let h = serve_once(&sock, |_| {
            vec![serde_json::to_string(&Response::err(None, RpcError::new(code::PARSE, "bad json"))).unwrap()]
        });
        let mut c = Client::connect(&sock).unwrap();
        match c.call::<Value>("x", json!({})) {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, code::PARSE),
            other => panic!("{other:?}"),
        }
        drop(c);
        h.join().unwrap();
    }

    #[test]
    fn an_error_for_another_id_is_not_ours() {
        let d = TestDir::new();
        let sock = d.path().join("s.sock");
        let h = serve_once(&sock, |req| {
            vec![
                serde_json::to_string(&Response::err(Some(req.id + 99), RpcError::new(code::NOT_FOUND, "other"))).unwrap(),
                serde_json::to_string(&Response::ok(req.id, json!(7))).unwrap(),
            ]
        });
        let mut c = Client::connect(&sock).unwrap();
        assert_eq!(c.call::<i64>("x", json!({})).unwrap(), 7);
        drop(c);
        h.join().unwrap();
    }

    #[test]
    fn into_raw_clears_timeouts() {
        let d = TestDir::new();
        let sock = d.path().join("s.sock");
        let _l = UnixListener::bind(&sock).unwrap();
        let c = Client::connect(&sock).unwrap();
        c.set_timeout(Some(Duration::from_secs(1))).unwrap();
        let (_r, w) = c.into_raw();
        assert_eq!(w.read_timeout().unwrap(), None);
        assert_eq!(w.write_timeout().unwrap(), None);
    }

    #[test]
    fn connect_rejects_an_overlong_socket_path() {
        let long = Path::new("/").join("a".repeat(200));
        let e = Client::connect(&long).err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }
}
