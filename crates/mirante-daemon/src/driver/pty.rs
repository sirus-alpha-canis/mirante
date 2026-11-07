//! An agent process in a pseudo-terminal, mirrored into a vt100 grid.
//! - The child is a session and process-group leader (portable-pty calls `setsid`), so
//!   [`PtyProcess::kill_tree`] ends it with everything it started ([`proctree::kill_tree`]).
//! - Terminal queries in the output (cursor position, device attributes, status, colors)
//!   are answered by the daemon, attached or not, and stripped from what attached clients
//!   get ([`termq`]): one answer, never two.
//! - Output fans out to attached clients over bounded queues. A client that falls behind
//!   gets a full repaint; a new client starts from a repaint that also restores the input
//!   modes (bracketed paste, cursor keys, mouse) the agent switched on.
//!
//! The process tree is ended when the `PtyProcess` is dropped.
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use super::proctree::{self, KillReport, Proc};
use super::termq::{self, Piece, TermQueries};
use super::Fanout;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtySpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env_remove: Vec<String>,
    pub env_set: Vec<(String, String)>,
    pub rows: u16,
    pub cols: u16,
}

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct PtyProcess {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Writer,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    root: Option<Proc>,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Lock order: `fanout`, then `parser` (the reader and `attach` both follow it, so a
    /// new client's repaint and its first live bytes never overlap or leave a gap).
    fanout: Arc<Mutex<Fanout>>,
    exited: Arc<AtomicBool>,
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

pub(crate) fn nz(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n.max(1)).expect("max(1) is non-zero")
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Bytes that redraw `screen` on a fresh terminal, input modes included.
fn repaint_of(screen: &vt100::Screen) -> Vec<u8> {
    let mut out = b"\x1b[2J\x1b[H".to_vec();
    out.extend_from_slice(screen.state_formatted().as_bytes());
    out
}

/// One chunk of agent output: mirror it, answer its queries, fan it out.
fn pump(chunk_pieces: Vec<Piece>, parser: &Mutex<vt100::Parser>, fanout: &Mutex<Fanout>, writer: &Writer) {
    let mut f = lock(fanout);
    let mut live = Vec::new();
    for piece in chunk_pieces {
        match piece {
            Piece::Bytes(b) => {
                lock(parser).process(&b);
                live.extend_from_slice(&b);
            }
            Piece::Query(q) => {
                let cursor = lock(parser).screen().cursor_position();
                let mut w = lock(writer);
                let _ = w.write_all(&termq::answer(q, cursor)).and_then(|()| w.flush());
            }
        }
    }
    if !live.is_empty() {
        f.send(&live);
    }
    if f.needs_repaint() {
        // A client fell behind and lost chunks (bounded queue): send it the whole screen,
        // after which it gets live bytes again.
        let screen = repaint_of(lock(parser).screen());
        f.repaint(&screen);
    }
}

impl PtyProcess {
    /// Spawns `spec`; `on_exit` runs once, with the exit code, when the process ends.
    pub fn spawn(spec: &PtySpec, on_exit: impl FnOnce(Option<i32>) + Send + 'static) -> io::Result<Arc<PtyProcess>> {
        let pair = native_pty_system().openpty(size(spec.rows, spec.cols)).map_err(other)?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        for k in &spec.env_remove {
            cmd.env_remove(k);
        }
        for (k, v) in &spec.env_set {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).map_err(other)?;
        drop(pair.slave);
        // Captured now, while the pid surely names our child.
        let root = child.process_id().and_then(|pid| proctree::proc_of(pid as i32));
        let mut reader = pair.master.try_clone_reader().map_err(other)?;
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(other)?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(nz(spec.rows), nz(spec.cols), 0)));
        let fanout = Arc::new(Mutex::new(Fanout::default()));
        let p = Arc::new(PtyProcess {
            master: Mutex::new(pair.master),
            writer: writer.clone(),
            child: Mutex::new(child),
            root,
            parser: parser.clone(),
            fanout: fanout.clone(),
            exited: Arc::new(AtomicBool::new(false)),
        });
        std::thread::spawn(move || {
            let mut queries = TermQueries::default();
            let mut buf = [0u8; 8192];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                pump(queries.feed(&buf[..n]), &parser, &fanout, &writer);
            }
            let rest = queries.flush();
            if !rest.is_empty() {
                pump(vec![Piece::Bytes(rest)], &parser, &fanout, &writer);
            }
            // End of output: close every attached stream.
            *lock(&fanout) = Fanout::default();
        });
        let weak: Weak<PtyProcess> = Arc::downgrade(&p);
        let exited = p.exited.clone();
        std::thread::spawn(move || {
            let code = loop {
                let Some(p) = weak.upgrade() else {
                    return;
                };
                let status = lock(&p.child).try_wait();
                drop(p);
                match status {
                    Ok(Some(s)) => break Some(s.exit_code() as i32),
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break None,
                }
            };
            exited.store(true, Ordering::SeqCst);
            on_exit(code);
        });
        Ok(p)
    }

    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut w = lock(&self.writer);
        w.write_all(bytes)?;
        w.flush()
    }

    /// The screen as plain text, one line per row.
    pub fn grid(&self) -> String {
        lock(&self.parser).screen().contents()
    }

    /// Escape sequences that repaint the current screen, input modes included, on a
    /// fresh terminal.
    pub fn repaint(&self) -> Vec<u8> {
        repaint_of(lock(&self.parser).screen())
    }

    /// A new client: its repaint and a live stream that continues exactly after it.
    pub fn attach(&self) -> (u64, Vec<u8>, mpsc::Receiver<Vec<u8>>) {
        let mut f = lock(&self.fanout);
        let initial = repaint_of(lock(&self.parser).screen());
        let (id, rx) = f.subscribe();
        (id, initial, rx)
    }

    /// Resizes the PTY (the agent gets SIGWINCH and redraws) and the mirror grid.
    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        lock(&self.master).resize(size(rows, cols)).map_err(other)?;
        lock(&self.parser).set_size(nz(rows), nz(cols));
        Ok(())
    }

    pub fn subscribe(&self) -> (u64, mpsc::Receiver<Vec<u8>>) {
        lock(&self.fanout).subscribe()
    }

    pub fn unsubscribe(&self, id: u64) {
        lock(&self.fanout).remove(id);
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// The agent's root process (the session leader), as it was at spawn.
    pub fn root(&self) -> Option<Proc> {
        self.root
    }

    /// Ends the agent and every process below it: SIGTERM, `grace`, SIGKILL, verify.
    pub fn kill_tree(&self, grace: Duration) -> KillReport {
        match self.root {
            Some(root) => proctree::kill_tree(root, grace, &proctree::Kill),
            None => {
                let _ = lock(&self.child).kill();
                KillReport::default()
            }
        }
    }

    /// Waits up to `timeout` for the process to exit.
    pub fn wait_exit(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.exited() || matches!(lock(&self.child).try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Test only: feeds `bytes` into the mirror as if the agent had printed them.
    #[cfg(test)]
    pub fn mirror(&self, bytes: &[u8]) {
        lock(&self.parser).process(bytes);
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        let running = !matches!(lock(&self.child).try_wait(), Ok(Some(_)));
        if running {
            self.kill_tree(Duration::from_millis(200));
        }
        let _ = lock(&self.child).wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn sh(script: &str) -> PtySpec {
        PtySpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: std::env::temp_dir(),
            env_remove: vec![],
            env_set: vec![("TERM".into(), "xterm-256color".into())],
            rows: 24,
            cols: 80,
        }
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn typed_input_reaches_the_process_and_the_grid_mirrors_output() {
        let (tx, rx) = mpsc::channel();
        let p = PtyProcess::spawn(&sh("read line; echo \"got:$line\""), move |c| tx.send(c).unwrap()).unwrap();
        p.write(b"hello\r").unwrap();
        wait_for("echo", || p.grid().contains("got:hello"));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Some(0));
        wait_for("exited flag", || p.exited());
    }

    #[test]
    fn queries_are_answered_while_attached_and_never_reach_the_client() {
        // `stty raw -echo` so the reply reaches `dd` byte for byte.
        let p = PtyProcess::spawn(
            &sh("stty raw -echo; printf 'R\\r\\n'; dd bs=1 count=1 >/dev/null 2>&1; printf 'ab\\033[6n'; \
                 r=$(dd bs=1 count=6 2>/dev/null); printf '\\r\\nreply:%s' \"$r\" | tr '\\033' 'E'; sleep 5"),
            |_| {},
        )
        .unwrap();
        wait_for("raw mode", || p.grid().contains('R'));
        let (_, _initial, rx) = p.attach();
        p.write(b"g").unwrap();
        wait_for("reply", || p.grid().contains("reply:E[2;3R"));
        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains("reply:") {
            seen.extend(rx.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        assert!(!seen.windows(4).any(|w| w == b"\x1b[6n"), "{:?}", String::from_utf8_lossy(&seen));
        assert!(String::from_utf8_lossy(&seen).contains("ab"));
    }

    #[test]
    fn attached_clients_get_live_output_and_eof_when_the_process_ends() {
        let p = PtyProcess::spawn(&sh("read x; echo \"bye:$x\""), |_| {}).unwrap();
        let (_, rx) = p.subscribe();
        p.write(b"q\r").unwrap();
        let mut seen = Vec::new();
        while let Ok(chunk) = rx.recv_timeout(Duration::from_secs(5)) {
            seen.extend(chunk);
        }
        assert!(String::from_utf8_lossy(&seen).contains("bye:q"));
    }

    #[test]
    fn the_repaint_restores_input_modes() {
        let p = PtyProcess::spawn(&sh("printf '\\033[?2004hhello'; sleep 5"), |_| {}).unwrap();
        wait_for("hello", || p.grid().contains("hello"));
        let (_, initial, _rx) = p.attach();
        let text = String::from_utf8_lossy(&initial);
        assert!(text.contains("hello") && text.contains("\x1b[?2004h"), "{text:?}");
    }

    #[test]
    fn dropping_the_process_ends_its_tree_and_resize_reaches_the_grid() {
        let p = PtyProcess::spawn(&sh("sleep 30 & sleep 30"), |_| {}).unwrap();
        let root = p.root().expect("root process");
        wait_for("the background child", || !proctree::descendants(root).is_empty());
        let kids = proctree::descendants(root);
        p.resize(30, 100).unwrap();
        assert_eq!(p.parser.lock().unwrap().screen().size(), (nz(30), nz(100)));
        let started = Instant::now();
        drop(p);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!proctree::alive(root));
        assert!(kids.iter().all(|k| !proctree::alive(*k)), "{kids:?}");
    }
}
