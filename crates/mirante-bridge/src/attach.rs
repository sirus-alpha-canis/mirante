//! A task's live terminal: `attach` takes over a connection, then raw bytes flow both
//! ways. Output is read in a thread and handed to `on_output`; `on_end` runs once when the
//! stream ends (agent exited, daemon gone, or [`Attachments::close`]). Window size changes
//! go through `session.resize` (an RPC call), not through this connection.
//!
//! Writes never hold the map's lock: each attachment has its own writer lock, and a write
//! that a stalled daemon does not take within the write timeout fails with
//! [`BridgeError::Timeout`]. An attachment whose stream ended leaves the map by itself.
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use mirante_ipc::types::{method, AttachParams, AttachResult};
use mirante_ipc::Client;
use serde::Serialize;

use crate::rpc::BridgeError;

/// How long typed bytes may wait for the daemon to take them.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

struct Attachment {
    writer: Arc<Mutex<UnixStream>>,
    /// A clone of the socket for shutting it down without waiting on `writer`'s lock.
    control: UnixStream,
    input: bool,
    reader: Option<JoinHandle<()>>,
}

type Open = Arc<Mutex<HashMap<u32, Attachment>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Opened {
    pub id: u32,
    /// False for a read-only stream (Codex): typed bytes are dropped.
    pub input: bool,
}

/// Open attachments by id (the frontend holds ids, never sockets).
pub struct Attachments {
    next: AtomicU32,
    open: Open,
    write_timeout: Duration,
}

impl Default for Attachments {
    fn default() -> Self {
        Self::with_write_timeout(WRITE_TIMEOUT)
    }
}

impl Attachments {
    pub fn with_write_timeout(write_timeout: Duration) -> Self {
        Attachments { next: AtomicU32::new(0), open: Arc::default(), write_timeout }
    }

    pub fn open(
        &self,
        socket: &Path,
        task: &str,
        rows: u16,
        cols: u16,
        mut on_output: impl FnMut(Vec<u8>) + Send + 'static,
        on_end: impl FnOnce() + Send + 'static,
    ) -> Result<Opened, BridgeError> {
        let mut c = Client::connect(socket).map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
        c.set_timeout(Some(std::time::Duration::from_secs(10)))
            .map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
        let r: AttachResult = c.call(method::ATTACH, AttachParams { task: task.into(), rows, cols })?;
        let (mut reader, writer) = c.into_raw();
        writer
            .set_write_timeout(Some(self.write_timeout))
            .map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        // In the map before the reader runs, so its exit always finds the entry to remove.
        let control = writer.try_clone().map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
        let a = Attachment { writer: Arc::new(Mutex::new(writer)), control, input: r.input, reader: None };
        self.open.lock().unwrap().insert(id, a);
        let open = self.open.clone();
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Ok(0) | Err(_) => break,
                    Ok(n) => on_output(buf[..n].to_vec()),
                }
            }
            // Ended (or closed): drop the entry (detaching this thread's own handle).
            let gone = open.lock().unwrap().remove(&id);
            drop(gone);
            on_end();
        });
        if let Some(a) = self.open.lock().unwrap().get_mut(&id) {
            a.reader = Some(thread);
        }
        Ok(Opened { id, input: r.input })
    }

    /// Typed bytes. Ignored on a read-only stream; an unknown id is an error. Only this
    /// attachment's writer is locked while writing.
    pub fn write(&self, id: u32, bytes: &[u8]) -> Result<(), BridgeError> {
        let (writer, input) = {
            let open = self.open.lock().unwrap();
            let a = open.get(&id).ok_or_else(|| BridgeError::Protocol { message: format!("no attachment {id}") })?;
            (a.writer.clone(), a.input)
        };
        if !input {
            return Ok(());
        }
        let mut w = writer.lock().unwrap();
        w.write_all(bytes).map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => BridgeError::Timeout {
                message: format!("the daemon did not take the input within {:?}", self.write_timeout),
            },
            _ => BridgeError::Disconnected { message: e.to_string() },
        })
    }

    /// Detaches (the session keeps running). Unknown ids are ignored.
    pub fn close(&self, id: u32) {
        let a = self.open.lock().unwrap().remove(&id);
        if let Some(mut a) = a {
            // Not through the writer's lock: a write stalled on it must not delay the close.
            let _ = a.control.shutdown(Shutdown::Both);
            if let Some(t) = a.reader.take() {
                let _ = t.join();
            }
        }
    }

    pub fn close_all(&self) {
        let ids: Vec<u32> = self.open.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.close(id);
        }
    }

    pub fn len(&self) -> usize {
        self.open.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeDaemon;
    use mirante_ipc::test_dir::TestDir;
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::Duration;

    fn read_until(rx: &mpsc::Receiver<Vec<u8>>, want: &[u8]) -> Vec<u8> {
        let mut got = Vec::new();
        while !got.windows(want.len()).any(|w| w == want) {
            got.extend(rx.recv_timeout(Duration::from_secs(5)).expect("output"));
        }
        got
    }

    #[test]
    fn output_and_input_flow_until_close() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let all = Attachments::default();
        let (tx, rx) = mpsc::channel();
        let (end_tx, end_rx) = mpsc::channel();
        let o = all
            .open(&fake.socket, "t1", 30, 100, move |b| tx.send(b).unwrap(), move || end_tx.send(()).unwrap())
            .unwrap();
        assert_eq!(o, Opened { id: 1, input: true });
        assert_eq!(fake.calls()[0], ("attach".to_string(), json!({"task": "t1", "rows": 30, "cols": 100})));
        read_until(&rx, b"fake terminal\r\n");
        all.write(o.id, b"ls\r").unwrap();
        read_until(&rx, b"ls\r");
        all.close(o.id);
        end_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(all.is_empty());
        assert!(all.write(o.id, b"x").is_err());
    }

    #[test]
    fn the_stream_ending_on_the_daemon_side_calls_on_end() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let all = Attachments::default();
        let (end_tx, end_rx) = mpsc::channel();
        all.open(&fake.socket, "t1", 24, 80, |_| {}, move || end_tx.send(()).unwrap()).unwrap();
        fake.drop_connections();
        end_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        all.close_all();
    }

    /// A daemon that answers `attach` and then never reads what is typed.
    fn stalled_daemon(sock: &std::path::Path) {
        use mirante_ipc::rpc::{read_line, write_line, Request, Response};
        let l = std::os::unix::net::UnixListener::bind(sock).unwrap();
        std::thread::spawn(move || {
            for conn in l.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut w = conn.try_clone().unwrap();
                    let mut r = std::io::BufReader::new(conn);
                    let req: Request = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
                    write_line(&mut w, &Response::ok(req.id, json!({"input": true}))).unwrap();
                    std::thread::sleep(Duration::from_secs(30));
                    drop(r);
                });
            }
        });
    }

    #[test]
    fn a_stalled_daemon_times_out_the_write_with_a_typed_error() {
        let d = TestDir::new();
        let sock = d.path().join("stall.sock");
        stalled_daemon(&sock);
        let all = Attachments::with_write_timeout(Duration::from_millis(200));
        let o = all.open(&sock, "t1", 24, 80, |_| {}, || {}).unwrap();
        let big = vec![b'x'; 1 << 20];
        let started = std::time::Instant::now();
        let err = loop {
            if let Err(e) = all.write(o.id, &big) {
                break e;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "the write never stalled");
        };
        assert!(matches!(err, BridgeError::Timeout { .. }), "{err:?}");
        assert_eq!(serde_json::to_value(&err).unwrap()["kind"], "timeout");
        all.close_all();
    }

    #[test]
    fn a_stalled_write_does_not_block_other_attachments() {
        let d = TestDir::new();
        let sock = d.path().join("stall.sock");
        stalled_daemon(&sock);
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let all = std::sync::Arc::new(Attachments::with_write_timeout(Duration::from_secs(3)));
        let stuck = all.open(&sock, "t1", 24, 80, |_| {}, || {}).unwrap();
        let (tx, rx) = mpsc::channel();
        let ok = all.open(&fake.socket, "t2", 24, 80, move |b| tx.send(b).unwrap(), || {}).unwrap();
        read_until(&rx, b"fake terminal\r\n");
        let a = all.clone();
        let writer = std::thread::spawn(move || {
            let big = vec![b'x'; 1 << 20];
            while a.write(stuck.id, &big).is_ok() {}
        });
        std::thread::sleep(Duration::from_millis(300));
        let t0 = std::time::Instant::now();
        all.write(ok.id, b"pwd\r").unwrap();
        assert!(t0.elapsed() < Duration::from_secs(1), "blocked behind the stalled attach: {:?}", t0.elapsed());
        read_until(&rx, b"pwd\r");
        writer.join().unwrap();
        all.close_all();
    }

    #[test]
    fn an_attachment_that_ends_is_removed() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let all = Attachments::default();
        let (end_tx, end_rx) = mpsc::channel();
        let o = all.open(&fake.socket, "t1", 24, 80, |_| {}, move || end_tx.send(()).unwrap()).unwrap();
        assert_eq!(all.len(), 1);
        fake.drop_connections();
        end_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(all.is_empty(), "the ended attachment stays in the map");
        assert!(matches!(all.write(o.id, b"x"), Err(BridgeError::Protocol { .. })));
        all.close(o.id);
    }
}
