//! The daemon socket: `~/.mirante/daemon.sock`, mode 0600, and every connection's peer uid
//! must equal the daemon's. One thread per connection; newline-delimited JSON-RPC 2.0.
//! Two methods take over their connection: `events.subscribe` (a snapshot response, then
//! `event` notifications) and `attach` (a response, then raw bytes both ways until either
//! side closes).
use std::io::{self, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mirante_ipc::rpc::{code, read_line, write_line, Notification, Request, Response, RpcError};
use mirante_ipc::types::{
    method, AttachParams, AttachResult, ListParams, QueueRef, QueuedRef, SubscribeParams, TaskRef,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::hub::dna::Caller;
use crate::hub::{self, Hub};

pub type OnShutdown = Arc<dyn Fn() + Send + Sync>;

/// Per-connection limits.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Connections served at once; more are refused with `code::BUSY`.
    pub max_connections: usize,
    /// How long a new connection may take to send its first request.
    pub first_line_timeout: Duration,
    /// Write timeout on long-lived streams (events, attach): a client that stops reading
    /// is disconnected instead of blocking its server thread forever.
    pub stream_write_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_connections: 64,
            first_line_timeout: Duration::from_secs(10),
            stream_write_timeout: Duration::from_secs(5),
        }
    }
}

/// Binds the socket with mode 0600, replacing a stale socket. Call only while holding the
/// daemon lock, so a live daemon's socket is never removed. Anything at the path that is
/// not a socket (a file, a directory, a symlink) is left alone and is an error.
///
/// The socket is created under umask 0o077 (restored right after), so it is never
/// reachable by other users, not even between `bind` and `chmod`. (0o077 rather than
/// 0o177: the umask is process-wide, and a directory created by another thread in that
/// window must keep its owner `x` bit.)
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket; refusing to replace it", path.display()),
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    // SAFETY: umask only swaps the process file-mode mask.
    let old = unsafe { libc::umask(0o077) };
    let bound = UnixListener::bind(path);
    // SAFETY: as above, restoring the previous mask.
    unsafe { libc::umask(old) };
    let listener = bound?;
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
    serve_with(hub, listener, on_shutdown, Limits::default())
}

/// Releases a connection slot when the connection's thread ends.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn serve_with(hub: Arc<Hub>, listener: UnixListener, on_shutdown: OnShutdown, limits: Limits) -> io::Result<()> {
    let open = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let mut stream = match conn {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mirante-daemon: accept: {e}");
                continue;
            }
        };
        if open.fetch_add(1, Ordering::SeqCst) >= limits.max_connections {
            open.fetch_sub(1, Ordering::SeqCst);
            // Read the request first (briefly), so the client is not mid-write when the
            // connection closes and sees the BUSY answer instead of a broken pipe.
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                if let Ok(r) = stream.try_clone() {
                    let _ = read_line(&mut BufReader::new(r));
                }
                let e = RpcError::new(code::BUSY, "the daemon is at its connection limit; retry");
                let _ = write_line(&mut stream, &Response::err(None, e));
            });
            continue;
        }
        let slot = Slot(open.clone());
        let (hub, on_shutdown) = (hub.clone(), on_shutdown.clone());
        std::thread::spawn(move || {
            let _slot = slot;
            let _ = handle_with(&hub, stream, &*on_shutdown, limits);
        });
    }
    Ok(())
}

pub fn handle(hub: &Hub, stream: UnixStream, on_shutdown: &(dyn Fn() + Send + Sync)) -> io::Result<()> {
    handle_with(hub, stream, on_shutdown, Limits::default())
}

fn handle_with(
    hub: &Hub,
    stream: UnixStream,
    on_shutdown: &(dyn Fn() + Send + Sync),
    limits: Limits,
) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    if peer_uid(&stream)? != my_uid() {
        let e = RpcError::new(code::FORBIDDEN, "peer uid does not match the daemon's");
        return write_line(&mut writer, &Response::err(None, e));
    }
    // A connection that never sends its first request must not hold a slot forever. Later
    // requests may be far apart (a watch board's query connection), so only the first is timed.
    stream.set_read_timeout(Some(limits.first_line_timeout))?;
    let mut first = true;
    let mut reader = BufReader::new(stream);
    loop {
        let line = match read_line(&mut reader) {
            Ok(Some(l)) => l,
            Ok(None) => break,
            // Too long or not UTF-8: say why, then close (the stream cannot be resynced).
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let _ =
                    write_line(&mut writer, &Response::err(None, RpcError::new(code::INVALID_REQUEST, e.to_string())));
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        if std::mem::take(&mut first) {
            reader.get_ref().set_read_timeout(None)?;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_line(&mut writer, &Response::err(None, RpcError::new(code::PARSE, e.to_string())))?;
                continue;
            }
        };
        match req.method.as_str() {
            method::EVENTS_SUBSCRIBE => return subscribe(hub, req, writer, limits),
            method::ATTACH => return attach(hub, req, reader, writer, limits),
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
        method::TASK_EVENTS => out(hub.task_events(params(p)?)),
        method::TASK_DIFF => out(hub.task_diff(&params::<TaskRef>(p)?.task)),
        method::SESSION_START => out(hub.start_session(params(p)?, false)),
        method::SESSION_RESUME => out(hub.start_session(params(p)?, true)),
        method::SESSION_TRUST => out(hub.trust(params(p)?)),
        method::SESSION_STOP => out(hub.stop(&params::<TaskRef>(p)?.task)),
        method::SESSION_INTERRUPT => out(hub.interrupt(&params::<TaskRef>(p)?.task).map(|()| json!({}))),
        method::SESSION_RESIZE => out(hub.resize(params(p)?).map(|()| json!({}))),
        method::MESSAGE_SEND => out(hub.send(params(p)?)),
        method::MESSAGE_LIST_QUEUED => out(hub.list_queued(&params::<QueueRef>(p)?.task_id)),
        method::MESSAGE_EDIT => out(hub.edit_queued(params(p)?)),
        method::MESSAGE_REMOVE => out(hub.remove_queued(&params::<QueuedRef>(p)?.id)),
        method::MESSAGE_REORDER => out(hub.reorder_queued(params(p)?)),
        method::PERMISSION_LIST => out(hub.permissions()),
        method::PERMISSION_ANSWER => out(hub.answer_permission(params(p)?)),
        method::HOOK_EVENT => out(hub.apply_hook_envelope(params(p)?).map(|()| json!({}))),
        method::DAEMON_STATUS => out(Ok(hub.status())),
        method::SESSION_HIBERNATE => out(hub.hibernate(&params::<TaskRef>(p)?.task)),
        method::SESSION_WAKE => out(hub.wake(&params::<TaskRef>(p)?.task)),
        method::DAEMON_DOCTOR => out(hub.doctor()),
        method::DAEMON_KILL_ORPHAN => out(hub.kill_orphan(params(p)?)),
        method::DNA_STATUS => out(hub.dna_status(&params(p)?)),
        method::DNA_ENABLE => out(hub.dna_enable(&params(p)?, &Caller::Person)),
        method::DNA_DISABLE => out(hub.dna_disable(&params(p)?, &Caller::Person)),
        method::DNA_GRAPH => out(hub.dna_graph(&params(p)?)),
        method::DNA_SEARCH => out(hub.dna_search(params(p)?)),
        method::DNA_GET => out(hub.dna_get(params(p)?)),
        method::DNA_CHECK => out(hub.dna_check(&params(p)?)),
        other => Err(RpcError::new(code::METHOD_NOT_FOUND, format!("unknown method {other}"))),
    }
}

/// Snapshot (or resume point), the replayed events, then live events until the client
/// goes away, stops reading for [`Limits::stream_write_timeout`], or falls
/// [`hub::SUB_QUEUE`] events behind (the hub drops it; the client resumes with `since_seq`).
fn subscribe(hub: &Hub, req: Request, mut writer: UnixStream, limits: Limits) -> io::Result<()> {
    let id = req.id;
    let p: SubscribeParams = if req.params.is_null() {
        SubscribeParams::default()
    } else {
        match params(req.params) {
            Ok(p) => p,
            Err(e) => return write_line(&mut writer, &Response::err(Some(id), e)),
        }
    };
    let sub = match hub.subscribe_from(p.since_seq, p.boot_id.as_deref()) {
        Ok(x) => x,
        Err(e) => return write_line(&mut writer, &Response::err(Some(id), e.into())),
    };
    writer.set_write_timeout(Some(limits.stream_write_timeout))?;
    write_line(&mut writer, &Response::ok(id, serde_json::to_value(sub.snapshot)?))?;
    for event in sub.replay.into_iter().chain(sub.events) {
        write_line(&mut writer, &Notification::new(method::EVENT, serde_json::to_value(event)?))?;
    }
    Ok(())
}

fn attach(
    hub: &Hub,
    req: Request,
    mut reader: BufReader<UnixStream>,
    mut writer: UnixStream,
    limits: Limits,
) -> io::Result<()> {
    let p: AttachParams = match params(req.params) {
        Ok(p) => p,
        Err(e) => return write_line(&mut writer, &Response::err(Some(req.id), e)),
    };
    let (driver, a) = match hub.attach(p) {
        Ok(x) => x,
        Err(e) => return write_line(&mut writer, &Response::err(Some(req.id), e.into())),
    };
    // A client that stops reading is cut off, not left to block the output pump forever
    // (the driver's per-client queue is bounded, so the agent never waits on it).
    writer.set_write_timeout(Some(limits.stream_write_timeout))?;
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
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
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
    fn task_events_is_served_over_rpc() {
        let k = Kit::new();
        let t = k.create(AgentKind::Codex, "te").task;
        let v = dispatch(&k.hub, method::TASK_EVENTS, json!({"task": t.id, "limit": 5})).unwrap();
        assert_eq!(v[0]["kind"], "task_created");
        let e = dispatch(&k.hub, method::TASK_EVENTS, json!({})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
        let d = dispatch(&k.hub, method::TASK_DIFF, json!({"task": "te"})).unwrap();
        assert_eq!((d["base"].as_str(), d["files"].as_array().map(Vec::len)), (Some("trunk"), Some(0)));
    }

    #[test]
    fn dna_methods_are_served_and_validate_params() {
        let k = Kit::new();
        let e = dispatch(&k.hub, method::DNA_STATUS, json!({})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
        let e = dispatch(&k.hub, method::DNA_STATUS, json!({"repo": "/definitely/not/here"})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
        let plain = k.dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let e = dispatch(&k.hub, method::DNA_STATUS, json!({"repo": plain})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
        let repo = json!({"repo": k.repo});
        let st = dispatch(&k.hub, method::DNA_STATUS, repo.clone()).unwrap();
        assert_eq!(st["enabled"], false);
        assert_eq!(dispatch(&k.hub, method::DNA_ENABLE, repo.clone()).unwrap()["enabled"], true);
        assert_eq!(dispatch(&k.hub, method::DNA_CHECK, repo.clone()).unwrap()["notes"], 0);
        assert_eq!(dispatch(&k.hub, method::DNA_GRAPH, repo.clone()).unwrap()["nodes"], json!([]));
        let hits = dispatch(&k.hub, method::DNA_SEARCH, json!({"repo": k.repo, "query": "x"})).unwrap();
        assert_eq!(hits, json!([]));
        let e = dispatch(&k.hub, method::DNA_GET, json!({"repo": k.repo, "slug": "nope"})).unwrap_err();
        assert_eq!(e.code, code::NOT_FOUND);
        let e = dispatch(&k.hub, method::DNA_SEARCH, json!({"repo": k.repo})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
        assert_eq!(dispatch(&k.hub, method::DNA_DISABLE, repo).unwrap()["enabled"], false);
    }

    #[test]
    fn the_queue_is_served_over_rpc() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "q").task;
        k.hook(&t.id, json!({"hook_event_name": "UserPromptSubmit", "prompt": "work"}));
        for text in ["a", "b"] {
            dispatch(&k.hub, method::MESSAGE_SEND, json!({"task": "q", "text": text, "mode": "queue"})).unwrap();
        }
        let l = dispatch(&k.hub, method::MESSAGE_LIST_QUEUED, json!({"task_id": "q"})).unwrap();
        let (a, b) = (l[0]["id"].clone(), l[1]["id"].clone());
        let l = dispatch(&k.hub, method::MESSAGE_EDIT, json!({"id": a, "text": "a2"})).unwrap();
        assert_eq!(l[0]["text"], "a2");
        let l = dispatch(&k.hub, method::MESSAGE_REORDER, json!({"task_id": t.id, "ids": [b, a]})).unwrap();
        assert_eq!((l[0]["text"].as_str(), l[1]["text"].as_str()), (Some("b"), Some("a2")));
        let l = dispatch(&k.hub, method::MESSAGE_REMOVE, json!({"id": b})).unwrap();
        assert_eq!(l.as_array().map(Vec::len), Some(1));
        let e = dispatch(&k.hub, method::MESSAGE_REMOVE, json!({"id": b})).unwrap_err();
        assert_eq!(e.code, code::NOT_FOUND);
        k.hook(&t.id, json!({"hook_event_name": "Stop"}));
        let e = dispatch(&k.hub, method::MESSAGE_EDIT, json!({"id": a, "text": "late"})).unwrap_err();
        assert_eq!(e.code, code::CONFLICT);
        let e = dispatch(&k.hub, method::MESSAGE_REORDER, json!({"task_id": "q"})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
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
    fn a_hook_event_delivered_twice_with_one_event_id_opens_one_card() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "h").task;
        let (sock, _) = serve_kit(&k);
        let mut c = Client::connect(&sock).unwrap();
        let payload = json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash"}).to_string();
        let env = json!({"task": t.id, "payload": payload, "ts_ms": 1, "event_id": "1-2-3"});
        let _: Value = c.call(method::HOOK_EVENT, &env).unwrap();
        let _: Value = c.call(method::HOOK_EVENT, &env).unwrap();
        assert_eq!(k.pending(&t.id).len(), 1);
        let kinds = k.kinds(&t.id);
        assert_eq!(kinds.iter().filter(|k| *k == "permission_requested").count(), 1, "{kinds:?}");
    }

    fn serve_limits(k: &Kit, limits: Limits) -> std::path::PathBuf {
        let sock = k.dir.path().join("l.sock");
        let listener = bind(&sock).unwrap();
        let hub = k.hub.clone();
        std::thread::spawn(move || serve_with(hub, listener, Arc::new(|| {}), limits));
        sock
    }

    #[test]
    fn bind_replaces_a_stale_socket_but_never_another_kind_of_file() {
        let k = Kit::new();
        let sock = k.dir.path().join("s.sock");
        drop(UnixListener::bind(&sock).unwrap());
        let _l = bind(&sock).unwrap();
        assert_eq!(std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);
        let file = k.dir.path().join("f.sock");
        std::fs::write(&file, b"precious").unwrap();
        assert_eq!(bind(&file).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&file).unwrap(), b"precious");
        let dir = k.dir.path().join("d.sock");
        std::fs::create_dir(&dir).unwrap();
        assert!(bind(&dir).is_err());
        assert!(dir.is_dir());
        let link = k.dir.path().join("ln.sock");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(bind(&link).is_err());
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    }

    #[test]
    fn an_invalid_line_gets_an_error_reply_then_the_connection_closes() {
        let k = Kit::new();
        let (sock, _) = serve_kit(&k);
        let mut s = UnixStream::connect(&sock).unwrap();
        s.write_all(b"\xff\xfe not utf-8\n").unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let resp: Response = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(resp.error.unwrap().code, code::INVALID_REQUEST);
        assert_eq!(read_line(&mut r).unwrap(), None);
    }

    #[test]
    fn a_silent_connection_is_closed_after_the_first_line_timeout() {
        let k = Kit::new();
        let limits = Limits { first_line_timeout: Duration::from_millis(100), ..Limits::default() };
        let sock = serve_limits(&k, limits);
        let s = UnixStream::connect(&sock).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let t = std::time::Instant::now();
        assert_eq!(read_line(&mut BufReader::new(s)).unwrap(), None);
        assert!(t.elapsed() < Duration::from_secs(2));
        // Only the first request is timed: a connection that spoke may idle.
        let mut c = Client::connect(&sock).unwrap();
        let _: Value = c.call(method::DAEMON_STATUS, Value::Null).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let _: Value = c.call(method::DAEMON_STATUS, Value::Null).unwrap();
    }

    #[test]
    fn connections_past_the_cap_are_refused_busy() {
        let k = Kit::new();
        let sock = serve_limits(&k, Limits { max_connections: 1, ..Limits::default() });
        let mut first = Client::connect(&sock).unwrap();
        let _: Value = first.call(method::DAEMON_STATUS, Value::Null).unwrap();
        let mut second = Client::connect(&sock).unwrap();
        match second.call::<Value>(method::DAEMON_STATUS, Value::Null) {
            Err(ClientError::Rpc(e)) => assert_eq!(e.code, code::BUSY),
            other => panic!("{other:?}"),
        }
        drop(first);
        // The slot is released when the first connection ends.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let mut c = Client::connect(&sock).unwrap();
            match c.call::<Value>(method::DAEMON_STATUS, Value::Null) {
                Ok(_) => break,
                Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("{e}"),
            }
        }
    }

    #[test]
    fn subscribe_resumes_with_since_seq_and_replays_missed_events() {
        let k = Kit::new();
        let (sock, _) = serve_kit(&k);
        let mut first = Client::connect(&sock).unwrap();
        let snap: Snapshot = first.call(method::EVENTS_SUBSCRIBE, json!({})).unwrap();
        drop(first);
        k.create(AgentKind::Claude, "missed");
        let mut again = Client::connect(&sock).unwrap();
        let r: Snapshot = again
            .call(method::EVENTS_SUBSCRIBE, SubscribeParams { since_seq: Some(snap.seq), boot_id: Some(snap.boot_id) })
            .unwrap();
        assert!(r.resumed);
        let n = again.next_notification().unwrap().unwrap();
        assert_eq!((n.params["kind"].as_str(), n.params["seq"].as_u64()), (Some("task_created"), Some(snap.seq + 1)));
        let mut fresh = Client::connect(&sock).unwrap();
        let full: Snapshot =
            fresh.call(method::EVENTS_SUBSCRIBE, json!({"since_seq": snap.seq, "boot_id": "another"})).unwrap();
        assert!(!full.resumed);
        assert_eq!(full.tasks.len(), 1);
    }

    #[test]
    fn session_resize_reaches_the_live_driver() {
        let k = Kit::new();
        let t = k.started(AgentKind::Claude, "r").task;
        let (sock, _) = serve_kit(&k);
        let mut c = Client::connect(&sock).unwrap();
        let _: Value = c.call(method::SESSION_RESIZE, json!({"task": t.id, "rows": 30, "cols": 100})).unwrap();
        assert!(k.calls().contains(&"resize:30x100".to_string()), "{:?}", k.calls());
        let e = c.call::<Value>(method::SESSION_RESIZE, json!({"task": "nope", "rows": 1, "cols": 1}));
        assert!(matches!(e, Err(ClientError::Rpc(e)) if e.code == code::NOT_FOUND));
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
