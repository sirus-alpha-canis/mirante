//! A task's live terminal: `attach` takes over a connection, then raw bytes flow both
//! ways. Output is read in a thread and handed to `on_output`; `on_end` runs once when the
//! stream ends (agent exited, daemon gone, or [`Attachments::close`]). Window size changes
//! go through `session.resize` (an RPC call), not through this connection.
//!
//! Output is batched and flow-controlled:
//! - What is ready together goes out as one message (up to [`MAX_BATCH`]); a large read
//!   waits up to [`COALESCE`] for the rest of a burst, a small one (a keystroke's echo)
//!   goes out at once.
//! - The frontend acks what it has drawn ([`Attachments::ack`]). With [`MAX_UNACKED`]
//!   bytes handed over and not acked, the reader stops reading: the daemon's writes block,
//!   its per-client queue overflows, and it sends a repaint instead of the backlog. While
//!   stopped it still reads a little every `stall_every`, so the daemon (which cuts off a
//!   client whose writes block for 5 s) keeps the stream.
//!
//! Writes never hold the map's lock: each attachment has its own writer lock, and a write
//! that a stalled daemon does not take within the write timeout fails with
//! [`BridgeError::Timeout`]. An attachment whose stream ended leaves the map by itself.
use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mirante_ipc::types::{method, AttachParams, AttachResult};
use mirante_ipc::Client;
use serde::Serialize;

use crate::rpc::BridgeError;

/// How long typed bytes may wait for the daemon to take them.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Output handed to the frontend and not acked yet, past which reading stops.
pub const MAX_UNACKED: usize = 256 * 1024;
/// Most bytes in one output message.
pub const MAX_BATCH: usize = 64 * 1024;
/// How long a large read waits for the rest of a burst before it goes out.
pub const COALESCE: Duration = Duration::from_millis(3);
/// A read at least this large is taken as part of a burst (worth waiting [`COALESCE`]).
const BURST_READ: usize = 4096;
/// While reading is stopped, this much is still read every `stall_every`.
const STALL_READ: usize = 4096;
/// Default `stall_every`: well inside the daemon's 5 s stream write timeout.
pub const STALL_READ_EVERY: Duration = Duration::from_secs(1);

/// Output handed over and not acked yet, shared by the reader thread and `ack`/`close`.
#[derive(Default)]
struct Flow {
    state: Mutex<FlowState>,
    room: Condvar,
}

#[derive(Default)]
struct FlowState {
    unacked: usize,
    closed: bool,
}

impl Flow {
    /// Waits until fewer than [`MAX_UNACKED`] bytes are unacked (`Some(true)`) or `max`
    /// passes (`Some(false)`); `None` once closed.
    fn wait_room(&self, max: Duration) -> Option<bool> {
        let deadline = Instant::now() + max;
        let mut s = self.state.lock().unwrap();
        loop {
            if s.closed {
                return None;
            }
            if s.unacked < MAX_UNACKED {
                return Some(true);
            }
            let now = Instant::now();
            if now >= deadline {
                return Some(false);
            }
            s = self.room.wait_timeout(s, deadline - now).unwrap().0;
        }
    }

    fn sent(&self, n: usize) {
        self.state.lock().unwrap().unacked += n;
    }

    fn ack(&self, n: usize) {
        let mut s = self.state.lock().unwrap();
        s.unacked = s.unacked.saturating_sub(n);
        self.room.notify_all();
    }

    fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.room.notify_all();
    }
}

struct Attachment {
    writer: Arc<Mutex<UnixStream>>,
    /// A clone of the socket for shutting it down without waiting on `writer`'s lock.
    control: UnixStream,
    input: bool,
    flow: Arc<Flow>,
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
    stall_every: Duration,
}

impl Default for Attachments {
    fn default() -> Self {
        Self::with_write_timeout(WRITE_TIMEOUT)
    }
}

/// One read, retried when interrupted. `Ok(0)`: end of stream.
fn read_some(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match r.read(buf) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

/// Whether `fd` has something to read (or hit its end) before `deadline`.
fn readable_before(fd: i32, deadline: Instant) -> bool {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let ms = left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd for the duration of the call.
        let r = unsafe { libc::poll(&mut p, 1, ms) };
        if r > 0 {
            return true;
        }
        if r == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
}

/// Reads output until the stream ends or the attachment is closed, in batches, within the
/// flow window (see the module docs).
fn pump_output(
    mut reader: BufReader<UnixStream>,
    flow: &Flow,
    stall_every: Duration,
    on_output: &mut impl FnMut(Vec<u8>),
) {
    let fd = reader.get_ref().as_raw_fd();
    let mut buf = vec![0u8; MAX_BATCH];
    loop {
        let Some(room) = flow.wait_room(stall_every) else {
            return;
        };
        let limit = if room { MAX_BATCH } else { STALL_READ };
        let mut n = match read_some(&mut reader, &mut buf[..limit]) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let mut ended = false;
        if room && n >= BURST_READ {
            let deadline = Instant::now() + COALESCE;
            while n < limit && (!reader.buffer().is_empty() || readable_before(fd, deadline)) {
                match read_some(&mut reader, &mut buf[n..limit]) {
                    Ok(0) | Err(_) => {
                        ended = true;
                        break;
                    }
                    Ok(m) => n += m,
                }
            }
        }
        flow.sent(n);
        on_output(buf[..n].to_vec());
        if ended {
            return;
        }
    }
}

impl Attachments {
    pub fn with_write_timeout(write_timeout: Duration) -> Self {
        Self::with_flow(write_timeout, STALL_READ_EVERY)
    }

    /// `stall_every`: how often a little is read while the frontend acks nothing.
    pub fn with_flow(write_timeout: Duration, stall_every: Duration) -> Self {
        Attachments { next: AtomicU32::new(0), open: Arc::default(), write_timeout, stall_every }
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
        let (reader, writer) = c.into_raw();
        writer
            .set_write_timeout(Some(self.write_timeout))
            .map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        // In the map before the reader runs, so its exit always finds the entry to remove.
        let control = writer.try_clone().map_err(|e| BridgeError::Disconnected { message: e.to_string() })?;
        let flow = Arc::new(Flow::default());
        let a = Attachment {
            writer: Arc::new(Mutex::new(writer)),
            control,
            input: r.input,
            flow: flow.clone(),
            reader: None,
        };
        self.open.lock().unwrap().insert(id, a);
        let open = self.open.clone();
        let stall_every = self.stall_every;
        let thread = std::thread::spawn(move || {
            pump_output(reader, &flow, stall_every, &mut on_output);
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

    /// The frontend drew `bytes` more of this attachment's output. Unknown ids are ignored
    /// (the stream may have just ended).
    pub fn ack(&self, id: u32, bytes: usize) {
        let flow = self.open.lock().unwrap().get(&id).map(|a| a.flow.clone());
        if let Some(f) = flow {
            f.ack(bytes);
        }
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
            // A reader stopped for want of acks wakes up and ends.
            a.flow.close();
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

    /// A daemon that answers `attach`, then writes `burst` bytes in 8 KiB writes (as the
    /// daemon's PTY pump does), then keeps writing while `flood` is set, counting what the
    /// socket took, and records every typed byte.
    struct Burst {
        written: Arc<std::sync::atomic::AtomicUsize>,
        typed: Arc<Mutex<Vec<u8>>>,
    }

    fn burst_daemon(sock: &std::path::Path, burst: usize, flood: bool) -> Burst {
        use mirante_ipc::rpc::{read_line, write_line, Request, Response};
        use std::sync::atomic::AtomicUsize;
        let l = std::os::unix::net::UnixListener::bind(sock).unwrap();
        let written = Arc::new(AtomicUsize::new(0));
        let typed = Arc::new(Mutex::new(Vec::new()));
        let (w2, t2) = (written.clone(), typed.clone());
        std::thread::spawn(move || {
            let conn = l.incoming().next().unwrap().unwrap();
            let mut w = conn.try_clone().unwrap();
            let mut r = std::io::BufReader::new(conn);
            let req: Request = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
            write_line(&mut w, &Response::ok(req.id, json!({"input": true}))).unwrap();
            std::thread::spawn(move || {
                let mut buf = [0u8; 256];
                while let Ok(n @ 1..) = r.read(&mut buf) {
                    t2.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
            let chunk = vec![b'x'; 8192];
            let mut sent = 0;
            while sent < burst || flood {
                if w.write_all(&chunk).is_err() {
                    return;
                }
                sent += chunk.len();
                w2.store(sent, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_secs(30));
        });
        Burst { written, typed }
    }

    #[test]
    fn a_burst_reaches_the_frontend_in_few_large_messages() {
        let d = TestDir::new();
        let sock = d.path().join("burst.sock");
        burst_daemon(&sock, 1 << 20, false);
        let all = Arc::new(Attachments::default());
        let (tx, rx) = mpsc::channel();
        let acks = all.clone();
        let id = Arc::new(AtomicU32::new(0));
        let id2 = id.clone();
        let o = all
            .open(
                &sock,
                "t1",
                24,
                80,
                move |b| {
                    // The frontend acks what it has written (here: at once).
                    acks.ack(id2.load(Ordering::SeqCst), b.len());
                    tx.send(b.len()).unwrap();
                },
                || {},
            )
            .unwrap();
        id.store(o.id, Ordering::SeqCst);
        let (mut bytes, mut messages) = (0, 0);
        while bytes < 1 << 20 {
            bytes += rx.recv_timeout(Duration::from_secs(5)).expect("output");
            messages += 1;
        }
        assert_eq!(bytes, 1 << 20);
        // 8 KiB reads were one message each (128); batches are up to MAX_BATCH.
        assert!(messages <= 40, "{messages} messages for 1 MiB");
        all.close_all();
    }

    #[test]
    fn small_output_is_not_held_back_for_coalescing() {
        let d = TestDir::new();
        let fake = FakeDaemon::start(&d.path().join("d.sock"));
        let all = Attachments::default();
        let (tx, rx) = mpsc::channel();
        let o = all.open(&fake.socket, "t1", 24, 80, move |b| tx.send(b).unwrap(), || {}).unwrap();
        read_until(&rx, b"fake terminal\r\n");
        let mut lat = Vec::new();
        for _ in 0..200 {
            let t = std::time::Instant::now();
            all.write(o.id, b"x").unwrap();
            read_until(&rx, b"x");
            lat.push(t.elapsed());
        }
        lat.sort();
        // A keystroke's echo goes out as soon as it is read (no coalescing wait).
        assert!(lat[100] < COALESCE / 2, "p50 echo {:?}", lat[100]);
        all.close_all();
    }

    #[test]
    fn without_acks_reading_stops_and_typed_keys_still_get_through() {
        let d = TestDir::new();
        let sock = d.path().join("flood.sock");
        let daemon = burst_daemon(&sock, 0, true);
        let all = Attachments::default();
        let got = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let g = got.clone();
        let o = all
            .open(
                &sock,
                "t1",
                24,
                80,
                move |b| {
                    g.fetch_add(b.len(), Ordering::SeqCst);
                },
                || {},
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let delivered = got.load(Ordering::SeqCst);
        // Nothing acked: no more than the window (plus one batch) was handed over...
        assert!(delivered <= MAX_UNACKED + MAX_BATCH, "{delivered} bytes delivered unacked");
        // ...and the daemon is held back by the socket (its own queue then overflows into a
        // repaint) instead of the bytes piling up here.
        let held = daemon.written.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        assert!(daemon.written.load(Ordering::SeqCst) - held <= 512 * 1024, "the daemon kept writing");
        // Typing still works while output is paused.
        all.write(o.id, b"ls\r").unwrap();
        let t0 = std::time::Instant::now();
        while daemon.typed.lock().unwrap().as_slice() != b"ls\r" {
            assert!(t0.elapsed() < Duration::from_secs(5), "typed bytes never arrived");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Acking reopens the window.
        all.ack(o.id, delivered);
        let t0 = std::time::Instant::now();
        while got.load(Ordering::SeqCst) <= delivered {
            assert!(t0.elapsed() < Duration::from_secs(5), "reading never resumed");
            std::thread::sleep(Duration::from_millis(10));
        }
        let t0 = std::time::Instant::now();
        all.close(o.id);
        assert!(t0.elapsed() < Duration::from_secs(2), "close waited on the paused reader");
    }

    #[test]
    fn a_frontend_that_never_acks_still_drains_a_trickle_so_the_daemon_never_times_out() {
        let d = TestDir::new();
        let sock = d.path().join("flood.sock");
        let daemon = burst_daemon(&sock, 0, true);
        let all = Attachments::with_flow(WRITE_TIMEOUT, Duration::from_millis(100));
        let o = all.open(&sock, "t1", 24, 80, |_| {}, || {}).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let a = daemon.written.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(500));
        let b = daemon.written.load(Ordering::SeqCst);
        assert!(b > a, "the daemon's writes stalled for good");
        all.close(o.id);
    }
}
