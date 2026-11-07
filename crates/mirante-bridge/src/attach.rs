//! A task's live terminal: `attach` takes over a connection, then raw bytes flow both
//! ways. Output is read in a thread and handed to `on_output`; `on_end` runs once when the
//! stream ends (agent exited, daemon gone, or [`Attachments::close`]). Window size changes
//! go through `session.resize` (an RPC call), not through this connection.
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread::JoinHandle;

use mirante_ipc::types::{method, AttachParams, AttachResult};
use mirante_ipc::Client;
use serde::Serialize;

use crate::rpc::BridgeError;

struct Attachment {
    writer: UnixStream,
    input: bool,
    reader: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Opened {
    pub id: u32,
    /// False for a read-only stream (Codex): typed bytes are dropped.
    pub input: bool,
}

/// Open attachments by id (the frontend holds ids, never sockets).
#[derive(Default)]
pub struct Attachments {
    next: AtomicU32,
    open: Mutex<HashMap<u32, Attachment>>,
}

impl Attachments {
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
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Ok(0) | Err(_) => break,
                    Ok(n) => on_output(buf[..n].to_vec()),
                }
            }
            on_end();
        });
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let a = Attachment { writer, input: r.input, reader: Some(thread) };
        self.open.lock().unwrap().insert(id, a);
        Ok(Opened { id, input: r.input })
    }

    /// Typed bytes. Ignored on a read-only stream; an unknown id is an error.
    pub fn write(&self, id: u32, bytes: &[u8]) -> Result<(), BridgeError> {
        let mut open = self.open.lock().unwrap();
        let a = open.get_mut(&id).ok_or_else(|| BridgeError::Protocol { message: format!("no attachment {id}") })?;
        if !a.input {
            return Ok(());
        }
        a.writer.write_all(bytes).map_err(|e| BridgeError::Disconnected { message: e.to_string() })
    }

    /// Detaches (the session keeps running). Unknown ids are ignored.
    pub fn close(&self, id: u32) {
        let a = self.open.lock().unwrap().remove(&id);
        if let Some(mut a) = a {
            let _ = a.writer.shutdown(Shutdown::Both);
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
}
