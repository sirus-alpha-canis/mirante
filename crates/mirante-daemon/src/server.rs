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
