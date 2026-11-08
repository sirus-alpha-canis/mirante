//! A scripted stand-in for `mirante-daemon` on a real Unix socket: the same framing and
//! method names, canned answers, events pushed by the test. Every request is recorded.
use std::io::{BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mirante_ipc::rpc::{code, read_line, write_line, Notification, Request, Response, RpcError};
use mirante_ipc::types::{method, SubscribeParams};
use serde_json::{json, Value};

#[derive(Default)]
struct Shared {
    calls: Vec<(String, Value)>,
    boot_id: String,
    events: Vec<Value>,
    tasks: Vec<Value>,
    subscribers: Vec<UnixStream>,
    open: Vec<UnixStream>,
    dna: crate::fake_dna::DnaDemo,
}

#[derive(Clone)]
pub struct FakeDaemon {
    pub socket: PathBuf,
    shared: Arc<Mutex<Shared>>,
}

impl FakeDaemon {
    /// Binds `socket` and serves until the process ends.
    pub fn start(socket: &Path) -> FakeDaemon {
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket).expect("bind fake daemon socket");
        let shared = Arc::new(Mutex::new(Shared { boot_id: "boot-1".into(), ..Default::default() }));
        let s = shared.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let s = s.clone();
                std::thread::spawn(move || serve(s, conn));
            }
        });
        FakeDaemon { socket: socket.to_path_buf(), shared }
    }

    pub fn calls(&self) -> Vec<(String, Value)> {
        self.shared.lock().unwrap().calls.clone()
    }

    pub fn methods(&self) -> Vec<String> {
        self.calls().into_iter().map(|(m, _)| m).collect()
    }

    /// Waits until `method` was called `n` times.
    pub fn wait_calls(&self, method: &str, n: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.methods().iter().filter(|m| *m == method).count() >= n {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// The `TaskView` JSON values served by snapshots and `task.list`.
    pub fn set_tasks(&self, tasks: Vec<Value>) {
        self.shared.lock().unwrap().tasks = tasks;
    }

    /// Appends an event (next `seq`) and sends it to every subscriber. Returns its seq.
    pub fn push_event(&self, task: Option<&str>, kind: &str, payload: Value) -> u64 {
        let mut s = self.shared.lock().unwrap();
        let seq = s.events.len() as u64 + 1;
        let ev = json!({"seq": seq, "boot_id": s.boot_id, "task_id": task, "kind": kind, "payload": payload, "ts_ms": 1_700_000_000_000u64 + seq});
        s.events.push(ev.clone());
        let line = Notification::new(method::EVENT, ev);
        s.subscribers.retain_mut(|w| write_line(w, &line).is_ok());
        seq
    }

    /// Sends `line` as is to every subscriber (for undecodable input); not recorded.
    pub fn push_raw(&self, line: &str) {
        let mut s = self.shared.lock().unwrap();
        s.subscribers.retain_mut(|w| w.write_all(format!("{line}\n").as_bytes()).is_ok());
    }

    /// Closes every open connection (as a daemon restart would), keeping the boot id.
    pub fn drop_connections(&self) {
        let mut s = self.shared.lock().unwrap();
        for c in s.open.drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
        s.subscribers.clear();
    }

    /// A new boot: later subscriptions get a full snapshot whatever they ask for.
    pub fn set_boot(&self, boot: &str) {
        self.shared.lock().unwrap().boot_id = boot.into();
    }
}

fn record(shared: &Arc<Mutex<Shared>>, req: &Request) {
    shared.lock().unwrap().calls.push((req.method.clone(), req.params.clone()));
}

fn serve(shared: Arc<Mutex<Shared>>, conn: UnixStream) {
    let Ok(mut writer) = conn.try_clone() else { return };
    if let Ok(c) = conn.try_clone() {
        shared.lock().unwrap().open.push(c);
    }
    let mut reader = BufReader::new(conn);
    while let Ok(Some(line)) = read_line(&mut reader) {
        let Ok(req) = serde_json::from_str::<Request>(&line) else {
            let _ = write_line(&mut writer, &Response::err(None, RpcError::new(code::PARSE, "bad json")));
            continue;
        };
        record(&shared, &req);
        match req.method.as_str() {
            method::EVENTS_SUBSCRIBE => return subscribe(&shared, req, writer, reader),
            method::ATTACH => return attach(req, writer, reader),
            _ => {
                let resp = match answer(&shared, &req.method, &req.params) {
                    Ok(v) => Response::ok(req.id, v),
                    Err(e) => Response::err(Some(req.id), e),
                };
                if write_line(&mut writer, &resp).is_err() {
                    return;
                }
            }
        }
    }
}

fn answer(shared: &Arc<Mutex<Shared>>, m: &str, p: &Value) -> Result<Value, RpcError> {
    let mut s = shared.lock().unwrap();
    let s = &mut *s;
    if let Some(r) = s.dna.answer(m, p, &s.tasks) {
        return r;
    }
    match m {
        method::DAEMON_STATUS => Ok(json!({"pid": 1, "boot_id": s.boot_id, "version": "fake", "live_sessions": 0})),
        method::TASK_LIST => Ok(Value::Array(s.tasks.clone())),
        method::TASK_GET => s
            .tasks
            .iter()
            .find(|t| t["id"] == p["task"])
            .cloned()
            .ok_or_else(|| RpcError::new(code::NOT_FOUND, format!("no task {}", p["task"]))),
        method::PERMISSION_LIST | method::TASK_EVENTS => Ok(json!([])),
        _ => Ok(json!({})),
    }
}

fn subscribe(shared: &Arc<Mutex<Shared>>, req: Request, mut writer: UnixStream, mut reader: BufReader<UnixStream>) {
    let p: SubscribeParams = serde_json::from_value(req.params).unwrap_or_default();
    {
        let mut s = shared.lock().unwrap();
        let last = s.events.len() as u64;
        let resume = p.since_seq.filter(|q| p.boot_id.as_deref() == Some(s.boot_id.as_str()) && *q <= last);
        let snap = match resume {
            Some(q) => json!({"boot_id": s.boot_id, "seq": q, "tasks": [], "resumed": true}),
            None => json!({"boot_id": s.boot_id, "seq": last, "tasks": s.tasks, "resumed": false}),
        };
        if write_line(&mut writer, &Response::ok(req.id, snap)).is_err() {
            return;
        }
        let from = resume.unwrap_or(last) as usize;
        for ev in &s.events[from..] {
            let _ = write_line(&mut writer, &Notification::new(method::EVENT, ev.clone()));
        }
        if let Ok(w) = writer.try_clone() {
            s.subscribers.push(w);
        }
    }
    // Hold the connection until the client goes away.
    let mut sink = [0u8; 64];
    while matches!(reader.read(&mut sink), Ok(n) if n > 0) {}
}

/// Answers `{input: true}`, writes a banner, then echoes every input byte back.
fn attach(req: Request, mut writer: UnixStream, mut reader: BufReader<UnixStream>) {
    if write_line(&mut writer, &Response::ok(req.id, json!({"input": true}))).is_err() {
        return;
    }
    let _ = writer.write_all(b"fake terminal\r\n");
    let mut buf = [0u8; 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if writer.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = writer.shutdown(Shutdown::Both);
}

/// A task view as the daemon serves it (`TaskView`: the task flattened plus live facts).
pub fn task_json(id: &str, repo: &str, branch: &str, agent: &str, status: &str) -> Value {
    json!({
        "id": id, "title": format!("goal of {branch}"), "repo_path": repo,
        "worktree_path": format!("/wt/{branch}"), "branch": branch, "agent": agent,
        "goal": format!("goal of {branch}"), "notes": "", "lifecycle": "active", "status": status,
        "created_at": 1, "updated_at": 2, "live": true, "boot": "ready", "needs_you": 0, "queued": 0,
        "detail": null
    })
}
