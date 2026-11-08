//! The event stream: `events.subscribe` in its own thread, forwarded as [`StreamMsg`]s.
//! When the connection drops (daemon restart, or the daemon cut a subscriber that fell
//! behind) it reconnects with backoff and asks to resume from the last `seq` + `boot_id`;
//! the daemon then replays what was missed, or sends a full snapshot (new boot, gap too big).
//! A line it cannot decode is skipped, logged and counted ([`Stream::skipped`]): reconnecting
//! would only replay it, forever.
use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use mirante_core::model::Event;
use mirante_ipc::rpc::read_line;
use mirante_ipc::types::{method, Snapshot, SubscribeParams};
use mirante_ipc::Client;
use serde::Serialize;

pub const BACKOFF_START: Duration = Duration::from_millis(250);
pub const BACKOFF_MAX: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamMsg {
    /// Subscribed. `snapshot.resumed` means "keep your state, missed events follow".
    Connected { snapshot: Snapshot },
    Event { event: Event },
    /// The stream is down; the next attempt is in `retry_ms`.
    Disconnected { error: String, retry_ms: u64 },
}

/// The running stream. Dropping it (or [`Stream::stop`]) ends the thread.
pub struct Stream {
    stop: Arc<AtomicBool>,
    skipped: Arc<AtomicU64>,
    conn: Arc<Mutex<Option<UnixStream>>>,
    thread: Option<JoinHandle<()>>,
}

impl Stream {
    /// Lines from the daemon that could not be decoded and were skipped.
    pub fn skipped(&self) -> u64 {
        self.skipped.load(Ordering::SeqCst)
    }

    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(c) = self.conn.lock().unwrap().take() {
            let _ = c.shutdown(Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Starts the stream thread; `emit` is called from it for every message, in order.
pub fn spawn(socket: PathBuf, emit: impl Fn(StreamMsg) + Send + 'static) -> Stream {
    let stop = Arc::new(AtomicBool::new(false));
    let skipped = Arc::new(AtomicU64::new(0));
    let conn = Arc::new(Mutex::new(None));
    let (s, k, c) = (stop.clone(), skipped.clone(), conn.clone());
    let thread = std::thread::spawn(move || run(socket, emit, s, k, c));
    Stream { stop, skipped, conn, thread: Some(thread) }
}

fn run(
    socket: PathBuf,
    emit: impl Fn(StreamMsg),
    stop: Arc<AtomicBool>,
    skipped: Arc<AtomicU64>,
    conn: Arc<Mutex<Option<UnixStream>>>,
) {
    let mut resume = SubscribeParams::default();
    let mut backoff = BACKOFF_START;
    while !stop.load(Ordering::SeqCst) {
        let error = match once(&socket, &mut resume, &emit, &stop, &skipped, &conn, &mut backoff) {
            Ok(()) => "the daemon closed the event stream".to_string(),
            Err(e) => e,
        };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        emit(StreamMsg::Disconnected { error, retry_ms: backoff.as_millis() as u64 });
        let mut waited = Duration::ZERO;
        while waited < backoff && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(25));
            waited += Duration::from_millis(25);
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

fn once(
    socket: &std::path::Path,
    resume: &mut SubscribeParams,
    emit: &impl Fn(StreamMsg),
    stop: &AtomicBool,
    skipped: &AtomicU64,
    conn: &Mutex<Option<UnixStream>>,
    backoff: &mut Duration,
) -> Result<(), String> {
    let mut client = Client::connect(socket).map_err(|e| e.to_string())?;
    client.set_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let snapshot: Snapshot = client.call(method::EVENTS_SUBSCRIBE, &*resume).map_err(|e| e.to_string())?;
    let (mut reader, writer): (BufReader<UnixStream>, UnixStream) = client.into_raw();
    *conn.lock().unwrap() = Some(writer);
    if stop.load(Ordering::SeqCst) {
        return Ok(());
    }
    *resume = SubscribeParams { since_seq: Some(snapshot.seq), boot_id: Some(snapshot.boot_id.clone()) };
    *backoff = BACKOFF_START;
    emit(StreamMsg::Connected { snapshot });
    loop {
        let Some(line) = read_line(&mut reader).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        let decoded = serde_json::from_str::<mirante_ipc::rpc::Notification>(&line)
            .and_then(|n| if n.method == method::EVENT { serde_json::from_value(n.params).map(Some) } else { Ok(None) });
        let event: Event = match decoded {
            Ok(Some(event)) => event,
            Ok(None) => continue,
            Err(e) => {
                let n = skipped.fetch_add(1, Ordering::SeqCst) + 1;
                eprintln!("mirante-bridge: skipped an undecodable event from the daemon ({n} so far): {e}");
                continue;
            }
        };
        *resume = SubscribeParams { since_seq: Some(event.seq), boot_id: Some(event.boot_id.clone()) };
        emit(StreamMsg::Event { event });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{task_json, FakeDaemon};
    use mirante_ipc::test_dir::TestDir;
    use serde_json::json;
    use std::sync::mpsc;

    fn next(rx: &mpsc::Receiver<StreamMsg>) -> StreamMsg {
        rx.recv_timeout(Duration::from_secs(5)).expect("a stream message")
    }

    fn connected(rx: &mpsc::Receiver<StreamMsg>) -> Snapshot {
        loop {
            if let StreamMsg::Connected { snapshot } = next(rx) {
                return snapshot;
            }
        }
    }

    #[test]
    fn snapshot_then_events_in_order() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        fake.set_tasks(vec![task_json("t1", "/r/app", "a", "claude", "idle")]);
        let (tx, rx) = mpsc::channel();
        let _s = spawn(fake.socket.clone(), move |m| tx.send(m).unwrap());
        let snap = connected(&rx);
        assert_eq!((snap.seq, snap.tasks.len(), snap.resumed), (0, 1, false));
        fake.push_event(Some("t1"), "status", json!({"from": "idle", "to": "working"}));
        fake.push_event(Some("t1"), "step", json!({"type": "tool"}));
        let kinds: Vec<(u64, String)> = (0..2)
            .map(|_| match next(&rx) {
                StreamMsg::Event { event } => (event.seq, event.kind),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(kinds, vec![(1, "status".into()), (2, "step".into())]);
    }

    #[test]
    fn a_dropped_stream_reconnects_and_resumes_from_the_last_seq() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let (tx, rx) = mpsc::channel();
        let _s = spawn(fake.socket.clone(), move |m| tx.send(m).unwrap());
        connected(&rx);
        fake.push_event(Some("t1"), "step", json!({}));
        assert!(matches!(next(&rx), StreamMsg::Event { .. }));
        fake.drop_connections();
        assert!(matches!(next(&rx), StreamMsg::Disconnected { retry_ms: 250, .. }));
        // Missed while down: replayed after the resumed snapshot.
        fake.push_event(Some("t1"), "status", json!({"to": "needs_you"}));
        let snap = connected(&rx);
        assert!(snap.resumed);
        assert_eq!(snap.seq, 1);
        match next(&rx) {
            StreamMsg::Event { event } => assert_eq!((event.seq, event.kind.as_str()), (2, "status")),
            other => panic!("{other:?}"),
        }
        let subs: Vec<serde_json::Value> =
            fake.calls().into_iter().filter(|(m, _)| m == method::EVENTS_SUBSCRIBE).map(|(_, p)| p).collect();
        assert_eq!(subs, vec![json!({}), json!({"since_seq": 1, "boot_id": "boot-1"})]);
    }

    #[test]
    fn an_undecodable_event_is_skipped_and_counted_not_a_reconnect() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let (tx, rx) = mpsc::channel();
        let s = spawn(fake.socket.clone(), move |m| tx.send(m).unwrap());
        connected(&rx);
        fake.push_raw(r#"{"jsonrpc":"2.0","method":"event","params":{"seq":"not a number"}}"#);
        fake.push_raw("not json at all");
        fake.push_event(Some("t1"), "step", json!({}));
        match next(&rx) {
            StreamMsg::Event { event } => assert_eq!(event.seq, 1),
            other => panic!("{other:?}"),
        }
        assert_eq!(s.skipped(), 2);
        let subs = fake.methods().iter().filter(|m| *m == method::EVENTS_SUBSCRIBE).count();
        assert_eq!(subs, 1, "the stream reconnected");
    }

    #[test]
    fn a_new_boot_gets_a_full_snapshot() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let (tx, rx) = mpsc::channel();
        let _s = spawn(fake.socket.clone(), move |m| tx.send(m).unwrap());
        connected(&rx);
        fake.set_boot("boot-2");
        fake.set_tasks(vec![task_json("t9", "/r/app", "z", "codex", "idle")]);
        fake.drop_connections();
        let snap = connected(&rx);
        assert!(!snap.resumed);
        assert_eq!((snap.boot_id.as_str(), snap.tasks.len()), ("boot-2", 1));
    }

    #[test]
    fn no_daemon_retries_with_growing_backoff_and_stop_ends_the_thread() {
        let d = TestDir::new();
        let (tx, rx) = mpsc::channel();
        let s = spawn(d.path().join("none.sock"), move |m| {
            let _ = tx.send(m);
        });
        let waits: Vec<u64> = (0..2)
            .map(|_| match next(&rx) {
                StreamMsg::Disconnected { retry_ms, .. } => retry_ms,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(waits, vec![250, 500]);
        s.stop();
    }

    #[test]
    fn messages_serialize_tagged_for_the_frontend() {
        let m = StreamMsg::Disconnected { error: "x".into(), retry_ms: 250 };
        assert_eq!(serde_json::to_value(m).unwrap(), json!({"type": "disconnected", "error": "x", "retry_ms": 250}));
    }
}
