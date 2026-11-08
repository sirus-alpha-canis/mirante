//! An agent process in a pseudo-terminal, mirrored into a vt100 grid.
//! - The child is a session and process-group leader (portable-pty calls `setsid`), so
//!   [`PtyProcess::kill_tree`] ends it with everything it started ([`proctree::kill_tree`]).
//! - Terminal queries in the output (cursor position, device attributes, status, colors)
//!   are answered by the daemon, attached or not, and stripped from what attached clients
//!   get ([`termq`]): one answer, never two. The answers go through their own thread: the
//!   reader never waits for the input side (a big paste blocked on an agent that is not
//!   reading would otherwise stop the reader, the agent would block on its output, and
//!   neither would move again).
//! - Exit: the process is seen exiting without being reaped (`waitid` with `WNOWAIT`), so
//!   its pid, and with it its process group id, stays reserved while what it left behind is
//!   ended: its group, and every process of the descendant snapshot (refreshed while it
//!   runs) that is still the same process. Only then is it reaped and the exit reported. A
//!   stop or drop after the exit uses the same snapshot.
//! - Output fans out to attached clients over bounded queues. A client that falls behind
//!   gets a full repaint; a new client starts from the mirror's scrollback (as lines) and a
//!   repaint that also restores the input modes (bracketed paste, cursor keys, mouse) the
//!   agent switched on.
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

/// Input is written in chunks of this size (the lock is held for the whole message, so
/// nothing interleaves with it).
const WRITE_CHUNK: usize = 4096;
/// Waits until `fd` is ready for `events` (`POLLOUT` / `POLLIN`); a hang-up counts as
/// ready (the next read or write reports it). Fails with `TimedOut` at `deadline` (`None`:
/// waits as long as it takes).
fn wait_fd(fd: std::os::fd::RawFd, events: libc::c_short, deadline: Option<std::time::Instant>) -> io::Result<()> {
    loop {
        let ms = match deadline {
            Some(d) => {
                let left = d.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "the agent is not reading its input"));
                }
                left.as_millis().clamp(1, i32::MAX as u128) as libc::c_int
            }
            None => -1,
        };
        let mut p = libc::pollfd { fd, events, revents: 0 };
        // SAFETY: one valid pollfd for the duration of the call.
        match unsafe { libc::poll(&mut p, 1, ms) } {
            n if n > 0 => return Ok(()),
            0 => {}
            _ => {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
        }
    }
}

/// Writes all of `bytes` to the master, which is non-blocking: what the PTY's input
/// queue cannot take now waits for room (polled), until `deadline` (`None`: no limit).
fn write_nb(
    w: &mut (dyn Write + Send),
    fd: Option<std::os::fd::RawFd>,
    mut bytes: &[u8],
    deadline: Option<std::time::Instant>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        match w.write(bytes) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "the agent's terminal took nothing")),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => match fd {
                Some(fd) => wait_fd(fd, libc::POLLOUT, deadline)?,
                None => std::thread::sleep(Duration::from_millis(5)),
            },
            Err(e) => return Err(e),
        }
    }
    w.flush()
}

/// Makes the master non-blocking (its reader and writer share the descriptor): a write
/// into a full input queue returns instead of blocking ([`write_nb`]), and the reader
/// polls for output.
fn set_nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor the PTY pair owns; no pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// How often the descendant snapshot is refreshed while the agent runs.
pub(super) const SNAPSHOT_EVERY: Duration = Duration::from_millis(200);
/// SIGTERM → SIGKILL grace for what an agent left behind when it exited on its own.
pub(super) const LEFTOVER_GRACE: Duration = Duration::from_millis(500);
/// Most processes the snapshot remembers.
pub(super) const SNAPSHOT_MAX: usize = 4096;

/// `pid` (our child) has exited and is not reaped yet. Does not reap it.
pub(super) fn exited_unreaped(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: `info` is a plain C struct the kernel fills; WNOWAIT leaves the child unreaped.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    let r = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) };
    r == 0 && si_pid(&info) == pid
}

#[cfg(target_os = "macos")]
fn si_pid(info: &libc::siginfo_t) -> i32 {
    info.si_pid
}

#[cfg(target_os = "linux")]
fn si_pid(info: &libc::siginfo_t) -> i32 {
    // SAFETY: filled by waitid for a child state change.
    unsafe { info.si_pid() }
}

pub struct PtyProcess {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Writer,
    /// The master's descriptor (owned by `master`), polled before each input chunk so a
    /// paste into an agent that stops reading times out ([`PtyProcess::write_within`]).
    input_fd: Option<std::os::fd::RawFd>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    root: Option<Proc>,
    /// The PTY's terminal device, read from the root's controlling terminal at spawn.
    tty: Option<u64>,
    /// Every process seen below the root while it ran (still-running ones only).
    snapshot: Arc<Mutex<Vec<Proc>>>,
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

/// Rows the mirror keeps after they scroll off the screen, replayed to a client that
/// attaches (32 bytes a cell: about 4 MB at 120 columns).
pub(super) const SCROLLBACK_ROWS: usize = 1000;

/// Bytes that redraw `screen` on a fresh terminal, input modes included.
fn repaint_of(screen: &vt100::Screen) -> Vec<u8> {
    let mut out = b"\x1b[2J\x1b[H".to_vec();
    out.extend_from_slice(screen.state_formatted().as_bytes());
    out
}

/// [`repaint_of`] preceded by the mirror's scrollback: its rows as lines, then enough
/// line feeds to push the last of them off a fresh terminal's screen (into its own
/// scrollback), so the screen's repaint (which clears the screen) loses none of them.
fn repaint_with_history(screen: &mut vt100::Screen) -> Vec<u8> {
    let total = screen.scrollback_len();
    let mut out = Vec::new();
    if total > 0 {
        let (rows, cols) = screen.size();
        let rows = usize::from(rows.get());
        let mut start = 0;
        while start < total {
            // At offset k the view's first k rows are the last k rows of the scrollback.
            screen.set_scrollback(total - start);
            let take = rows.min(total - start);
            for row in screen.rows_formatted(0, cols.get()).take(take) {
                out.extend_from_slice(row.as_bytes());
                out.extend_from_slice(b"\x1b[m\r\n");
            }
            start += take;
        }
        screen.set_scrollback(0);
        out.extend(std::iter::repeat_n(b'\n', rows.saturating_sub(1)));
    }
    out.extend_from_slice(&repaint_of(screen));
    out
}

/// One chunk of agent output: mirror it, answer its queries (queued for the answer thread,
/// never written here), fan it out.
fn pump(
    chunk_pieces: Vec<Piece>,
    parser: &Mutex<vt100::Parser>,
    fanout: &Mutex<Fanout>,
    answers: &mpsc::Sender<Vec<u8>>,
) {
    let mut f = lock(fanout);
    // Clients first: the parse below is the slow part and they need none of it. Both happen
    // under the fanout lock, so an attach still sees a screen that matches its stream.
    let live: Vec<u8> = chunk_pieces
        .iter()
        .filter_map(|p| match p {
            Piece::Bytes(b) => Some(b.as_slice()),
            Piece::Query(_) => None,
        })
        .flatten()
        .copied()
        .collect();
    if !live.is_empty() {
        f.send(&live);
    }
    for piece in chunk_pieces {
        match piece {
            Piece::Bytes(b) => lock(parser).process(&b),
            Piece::Query(q) => {
                let cursor = lock(parser).screen().cursor_position();
                let _ = answers.send(termq::answer(q, cursor));
            }
        }
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
        // Captured now, while the pid surely names our child. `spawn_command` returns after
        // the exec, so the child already made this PTY its controlling terminal.
        let root = child.process_id().and_then(|pid| proctree::proc_of(pid as i32));
        let tty = root.and_then(|r| proctree::controlling_tty(r.pid));
        let mut reader = pair.master.try_clone_reader().map_err(other)?;
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(other)?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(nz(spec.rows), nz(spec.cols), SCROLLBACK_ROWS)));
        let fanout = Arc::new(Mutex::new(Fanout::default()));
        let input_fd = pair.master.as_raw_fd();
        if let Some(fd) = input_fd {
            set_nonblocking(fd)?;
        }
        let p = Arc::new(PtyProcess {
            master: Mutex::new(pair.master),
            writer: writer.clone(),
            input_fd,
            child: Mutex::new(child),
            root,
            tty,
            snapshot: Arc::default(),
            parser: parser.clone(),
            fanout: fanout.clone(),
            exited: Arc::new(AtomicBool::new(false)),
        });
        let (answers, answer_rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            // Ends when the reader drops its sender (end of output).
            for a in answer_rx {
                let mut w = lock(&writer);
                let _ = write_nb(&mut **w, input_fd, &a, None);
            }
        });
        std::thread::spawn(move || {
            let mut queries = TermQueries::default();
            let mut buf = [0u8; 8192];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    // The master is non-blocking: wait for output.
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        match input_fd.map(|fd| wait_fd(fd, libc::POLLIN, None)) {
                            Some(Err(_)) => break,
                            Some(Ok(())) => {}
                            None => std::thread::sleep(Duration::from_millis(5)),
                        }
                        continue;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                pump(queries.feed(&buf[..n]), &parser, &fanout, &answers);
            }
            let rest = queries.flush();
            if !rest.is_empty() {
                pump(vec![Piece::Bytes(rest)], &parser, &fanout, &answers);
            }
            // End of output: close every attached stream.
            *lock(&fanout) = Fanout::default();
        });
        let weak: Weak<PtyProcess> = Arc::downgrade(&p);
        let exited = p.exited.clone();
        std::thread::spawn(move || {
            let mut walked = std::time::Instant::now() - SNAPSHOT_EVERY;
            let code = loop {
                let Some(p) = weak.upgrade() else {
                    return;
                };
                if let Some(root) = p.root {
                    if walked.elapsed() >= SNAPSHOT_EVERY {
                        walked = std::time::Instant::now();
                        p.refresh_snapshot(root);
                    }
                    if exited_unreaped(root.pid) {
                        // The zombie pins the pid and the group id: end what it left, then reap.
                        let snapshot = p.snapshot();
                        proctree::kill_tree_with(root, &snapshot, true, LEFTOVER_GRACE, &proctree::Kill);
                    }
                }
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

    /// Adds the root's current descendants to the snapshot and forgets the ones that ended.
    fn refresh_snapshot(&self, root: Proc) {
        let now = proctree::descendants(root);
        let mut snap = lock(&self.snapshot);
        snap.retain(|p| proctree::alive(*p));
        for p in now {
            if !snap.contains(&p) && snap.len() < SNAPSHOT_MAX {
                snap.push(p);
            }
        }
    }

    /// The processes seen below the agent that are still running.
    pub fn snapshot(&self) -> Vec<Proc> {
        lock(&self.snapshot).iter().copied().filter(|p| proctree::alive(*p)).collect()
    }

    /// Writes `bytes` in chunks, holding the input lock for all of them (nothing else is
    /// typed in between). Query answers never wait on this lock in the reader.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut w = lock(&self.writer);
        for chunk in bytes.chunks(WRITE_CHUNK) {
            write_nb(&mut **w, self.input_fd, chunk, None)?;
        }
        Ok(())
    }

    /// [`PtyProcess::write`] for bytes a person types or pastes in an attached terminal,
    /// all within `timeout`. An agent that stops reading its input fills the PTY's queue;
    /// the write then fails with `TimedOut` (the caller drops the attachment) instead of
    /// holding the input lock, and with it every message and answer, forever.
    pub fn write_within(&self, bytes: &[u8], timeout: Duration) -> io::Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        let mut w = lock(&self.writer);
        write_nb(&mut **w, self.input_fd, bytes, Some(deadline))
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

    /// A new client: the scrollback and a repaint, and a live stream that continues exactly
    /// after them.
    pub fn attach(&self) -> (u64, Vec<u8>, mpsc::Receiver<Vec<u8>>) {
        let mut f = lock(&self.fanout);
        let initial = repaint_with_history(lock(&self.parser).screen_mut());
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

    /// The device of this PTY's terminal (as [`proctree::controlling_tty`] reads it): the
    /// controlling terminal of the agent and of every process it starts that keeps it.
    pub fn tty(&self) -> Option<u64> {
        self.tty
    }

    /// Ends the agent and every process below it: SIGTERM, `grace`, SIGKILL, verify. Also
    /// after the agent exited: whatever of the descendant snapshot is still the same process
    /// (and its group, while the unreaped root still pins the group id).
    pub fn kill_tree(&self, grace: Duration) -> KillReport {
        match self.root {
            Some(root) => {
                let snapshot = self.snapshot();
                // Held so the exit watcher cannot reap the root while its group is signaled.
                let _child = lock(&self.child);
                let pinned = exited_unreaped(root.pid);
                proctree::kill_tree_with(root, &snapshot, pinned, grace, &proctree::Kill)
            }
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
        // Also when the agent already exited: what it left behind goes too.
        self.kill_tree(Duration::from_millis(200));
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

    /// A client attaching in the middle of a flood sees every line once, in order: its
    /// initial screen ends where its live stream starts.
    #[test]
    fn attaching_during_a_flood_gives_contiguous_numbered_lines() {
        let n = 30_000;
        let p = PtyProcess::spawn(
            &sh(&format!("i=0; while [ $i -lt {n} ]; do echo $i; i=$((i+1)); done; printf 'END'; sleep 5")),
            |_| {},
        )
        .unwrap();
        wait_for("the flood", || p.grid().lines().any(|l| l.trim().parse::<u32>().is_ok_and(|v| v > 500)));
        let (_id, initial, rx) = p.attach();
        let drained = std::thread::spawn(move || {
            let mut live = Vec::new();
            while let Ok(c) = rx.recv_timeout(Duration::from_secs(10)) {
                live.extend_from_slice(&c);
                if live[live.len().saturating_sub(c.len() + 2)..].windows(3).any(|w| w == b"END") {
                    break;
                }
            }
            live
        });
        let live = drained.join().unwrap();
        let mut screen = vt100::Parser::new(nz(24), nz(80), 0);
        screen.process(&initial);
        let rows: Vec<String> = screen.screen().rows(0, 80).collect();
        // The cursor's row is cut where the live stream continues it (maybe empty).
        let (crow, ccol) = screen.screen().cursor_position();
        let complete: Vec<&str> = rows[..crow as usize].iter().map(String::as_str).filter(|l| !l.is_empty()).collect();
        let partial: String = rows[crow as usize].chars().take(ccol as usize).collect();
        let text = format!("{partial}{}", String::from_utf8_lossy(&live));
        let mut nums: Vec<u32> = complete.iter().map(|l| l.trim().parse().unwrap()).collect();
        for l in text.split("\r\n") {
            if let Ok(v) = l.trim().parse::<u32>() {
                nums.push(v);
            }
        }
        assert!(!text.contains('\x1b'), "a repaint interrupted the stream (the client fell behind)");
        for w in nums.windows(2) {
            assert_eq!(w[1], w[0] + 1, "gap or repeat around {}", w[0]);
        }
        assert_eq!(nums.last(), Some(&(n - 1)));
    }

    #[test]
    fn a_paste_into_an_agent_that_never_reads_times_out_instead_of_blocking() {
        // Raw mode, and nothing reads stdin: the PTY's input queue fills up.
        let p = PtyProcess::spawn(&sh("stty raw -echo; printf 'R'; sleep 30"), |_| {}).unwrap();
        wait_for("raw mode", || p.grid().contains('R'));
        let t0 = Instant::now();
        let r = p.write_within(&vec![b'x'; 1 << 20], Duration::from_millis(300));
        assert!(matches!(&r, Err(e) if e.kind() == io::ErrorKind::TimedOut), "{r:?}");
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        // The input lock is free again: the session is not wedged.
        assert!(p.writer.try_lock().is_ok());
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

    /// A client that attaches later still gets what scrolled off the screen (the mirror
    /// keeps scrollback), as plain lines before the screen's own repaint.
    #[test]
    fn attach_replays_the_scrollback_before_the_screen() {
        let p = PtyProcess::spawn(&sh("i=1; while [ $i -le 60 ]; do echo row$i; i=$((i+1)); done; sleep 5"), |_| {})
            .unwrap();
        wait_for("row60", || p.grid().contains("row60"));
        let (_, initial, _rx) = p.attach();
        let text = String::from_utf8_lossy(&initial);
        let first = text.find("row1\x1b[m\r\n").unwrap_or_else(|| panic!("the scrollback's first row: {text:?}"));
        let screen = text.find("\x1b[2J").expect("the screen's repaint");
        assert!(first < screen, "{text:?}");
        assert!(text[screen..].contains("row60"), "{text:?}");
        // Only the attach replays history: a slow client's repaint is the screen alone.
        assert!(!String::from_utf8_lossy(&p.repaint()).contains("row1\x1b"));
        assert_eq!(p.parser.lock().unwrap().screen().scrollback(), 0, "the mirror is back on the live screen");
    }

    /// The cockpit's bridge stops reading when its webview falls behind: the client's queue
    /// stays bounded, it resumes with a repaint (not the backlog), and typing still works.
    #[test]
    fn a_flood_toward_a_stalled_client_stays_bounded_and_input_still_reaches_the_agent() {
        let mark = std::env::temp_dir().join(format!("mirante-flood-{}", std::process::id()));
        let _ = std::fs::remove_file(&mark);
        let script = format!(
            "stty -echo; (while :; do echo 0123456789abcdef0123456789abcdef; done) & read line; \
             echo \"got:$line\" > '{}'; kill $!; sleep 5",
            mark.display()
        );
        let p = PtyProcess::spawn(&sh(&script), |_| {}).unwrap();
        let (_, _initial, rx) = p.attach();
        std::thread::sleep(Duration::from_millis(500));
        let backlog: Vec<Vec<u8>> = rx.try_iter().collect();
        assert!(backlog.len() <= super::super::ATTACH_QUEUE + 1, "{} chunks queued", backlog.len());
        // Once there is room again it gets the whole screen (the chunks in between are gone).
        let mut after = backlog;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !after.iter().any(|c| c.starts_with(b"\x1b[2J")) {
            assert!(Instant::now() < deadline, "resumed without a repaint");
            after.extend(rx.recv_timeout(Duration::from_secs(5)));
            after.extend(rx.try_iter());
        }
        p.write(b"q\r").unwrap();
        wait_for("the typed line", || std::fs::read_to_string(&mark).is_ok_and(|s| s.contains("got:q")));
        let _ = std::fs::remove_file(&mark);
    }

    #[test]
    fn the_pty_is_the_controlling_tty_of_the_agent_and_its_children() {
        let p = PtyProcess::spawn(&sh("sleep 30 & sleep 30"), |_| {}).unwrap();
        let tty = p.tty().expect("the agent's terminal device");
        let root = p.root().expect("root process");
        assert_eq!(proctree::controlling_tty(root.pid), Some(tty));
        wait_for("the background child", || !proctree::descendants(root).is_empty());
        for kid in proctree::descendants(root) {
            assert_eq!(proctree::controlling_tty(kid.pid), Some(tty), "{kid:?}");
        }
        assert_ne!(proctree::controlling_tty(std::process::id() as i32), Some(tty));
        let other = PtyProcess::spawn(&sh("sleep 30"), |_| {}).unwrap();
        assert_ne!(other.tty(), Some(tty), "each session has its own terminal");
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

    fn pids_in(file: &std::path::Path) -> Vec<i32> {
        std::fs::read_to_string(file).unwrap_or_default().lines().filter_map(|l| l.trim().parse().ok()).collect()
    }

    #[test]
    fn an_agent_that_exits_on_its_own_takes_its_background_children_along() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let pids = dir.path().join("pids");
        // One child stays in the agent's group, one leaves it; then the agent exits.
        let script = format!(
            "sleep 60 & echo $! > {p}; perl -e 'setpgrp(0,0); sleep 60' & echo $! >> {p}; sleep 1; exit 0",
            p = pids.display()
        );
        let (tx, rx) = mpsc::channel();
        let p = PtyProcess::spawn(&sh(&script), move |c| tx.send(c).unwrap()).unwrap();
        wait_for("both children", || pids_in(&pids).len() == 2);
        let kids = pids_in(&pids);
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), Some(0));
        // By the time the exit is reported its leftovers are gone.
        for k in kids {
            wait_for("the leftover gone", || proctree::proc_of(k).is_none());
        }
        assert!(p.exited());
    }

    #[test]
    fn kill_tree_after_the_root_exited_still_ends_what_it_left() {
        let dir = mirante_ipc::test_dir::TestDir::new();
        let pids = dir.path().join("pids");
        let p = PtyProcess::spawn(
            &sh(&format!("perl -e 'setpgrp(0,0); sleep 60' & echo $! > {}; sleep 30", pids.display())),
            |_| {},
        )
        .unwrap();
        wait_for("the child", || pids_in(&pids).len() == 1);
        let kid = pids_in(&pids)[0];
        wait_for("the snapshot", || p.snapshot().iter().any(|k| k.pid == kid));
        // The root dies by a signal nobody sent through Mirante; the child it left survives
        // until the exit handling (or a stop / drop) ends it.
        let root = p.root().unwrap();
        unsafe { libc::kill(root.pid, libc::SIGKILL) };
        wait_for("the exit", || p.exited());
        p.kill_tree(Duration::from_millis(200));
        wait_for("the leftover gone", || proctree::proc_of(kid).is_none());
    }

    #[test]
    fn the_reader_never_waits_for_a_blocked_writer() {
        // The agent does not read its input for a while, asks the terminal for the cursor
        // position, then writes a lot, then reads its input until a second of silence (the
        // tty may drop input it had no room for, so the byte count is not checked). Meanwhile a big paste is being written: it blocks
        // until the agent reads. The query answer must not wait behind the paste (the old
        // reader did, stopped reading output, and the agent blocked on its output forever).
        let big = vec![b'x'; 256 * 1024];
        let p = PtyProcess::spawn(
            &sh("stty raw -echo; sleep 0.5; printf '\\033[6n'; head -c 400000 /dev/zero | tr '\\0' 'y'; \
                 stty min 0 time 10; cat > /dev/null; printf '\\r\\nDONE'; sleep 5"),
            |_| {},
        )
        .unwrap();
        let p2 = p.clone();
        let writer = std::thread::spawn(move || p2.write(&big));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !p.grid().contains("DONE") {
            assert!(Instant::now() < deadline, "deadlock: the agent never finished:\n{}", p.grid());
            std::thread::sleep(Duration::from_millis(50));
        }
        writer.join().unwrap().unwrap();
    }
}
