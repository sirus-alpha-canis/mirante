//! Claude Code driver: the unmodified interactive `claude` in a PTY (FINDINGS: the
//! human-attended mode that fits the terms, steers mid-run and resumes). Status comes from
//! hooks (via `mirante-hook` → daemon) fused with this driver's transcript tail; the screen
//! is read only to act on dialogs.
//! - Boot: the workspace-trust dialog is reported as `TrustPrompt` and answered only by the
//!   user (`trust`, which types the digit of the exact "Yes, I trust this folder" option).
//!   `Ready` needs the `SessionStart` hook, the input prompt, no dialog, and a screen that
//!   stayed unchanged for `ready_stable` (the "manual mode on" footer skips the wait).
//!   There is no time-based fallback: an unknown startup screen keeps the session booting.
//! - Messages are typed by one typist thread, one at a time: a bracketed paste, a pause,
//!   Enter. Right before the paste AND right before Enter the typist checks again that no
//!   dialog is on screen and that the hub holds no pending permission for the session:
//!   a dialog can appear between `send` and the keystrokes, and typed text would answer it.
//!   A message stopped that way is handed back (`DeliveryAborted`); stopped after the
//!   paste, the input line is cleared with Ctrl-U (never Enter, Esc or a digit, which a
//!   dialog would take as an answer).
//! - Interrupt is Esc; permission answers type the digit of the exact "Yes" / "No" option
//!   of the dialog that matches the request.
//! - Stop ends the whole process tree (SIGTERM, grace, SIGKILL; see `proctree`).
//! - A `--resume` whose process exits within `resume_window` reports `ResumeFailed`; a
//!   fresh start is the user's call.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use mirante_core::transcript_tail::TranscriptTail;
use mirante_ipc::types::BootState;
use serde_json::json;

use super::paste;
use super::proctree::Proc;
use super::pty::{PtyProcess, PtySpec};
use super::{screen, Attachment, Driver, DriverError, DriverEvent, Sink};

const ESC: &[u8] = b"\x1b";
/// Ctrl-U: clears the input line. Not an answer to any dialog.
const CLEAR_LINE: &[u8] = b"\x15";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeTiming {
    /// Screen polling interval during boot.
    pub tick: Duration,
    /// Pause between `send` and the first keystroke (lets a dialog that is about to open
    /// show up before anything is typed).
    pub pre_type: Duration,
    /// Pause between the pasted text and Enter (the TUI drops an Enter that arrives too fast).
    pub type_pause: Duration,
    pub tail_poll: Duration,
    /// SIGTERM to SIGKILL on stop.
    pub stop_grace: Duration,
    /// The boot watcher gives up after this long (the session stays `Booting`).
    pub boot_limit: Duration,
    /// How long the input prompt must stay unchanged before `Ready` (no footer fast path).
    pub ready_stable: Duration,
    /// A resumed process that exits sooner than this failed to resume.
    pub resume_window: Duration,
}

impl Default for ClaudeTiming {
    fn default() -> Self {
        ClaudeTiming {
            tick: Duration::from_millis(100),
            pre_type: Duration::from_millis(150),
            type_pause: Duration::from_millis(400),
            tail_poll: Duration::from_millis(300),
            stop_grace: Duration::from_secs(3),
            boot_limit: Duration::from_secs(600),
            ready_stable: Duration::from_millis(500),
            resume_window: Duration::from_secs(3),
        }
    }
}

enum Typing {
    /// (the message as given, the bytes to type before Enter)
    Message(String, Vec<u8>),
    /// Keys for a dialog or the TUI (answers, Esc): typed as they are.
    Keys(Vec<u8>),
}

pub struct ClaudeDriver {
    pty: Arc<PtyProcess>,
    sink: Sink,
    typist: Mutex<mpsc::Sender<Typing>>,
    timing: ClaudeTiming,
    tail_path: Mutex<Option<PathBuf>>,
    /// Bumped to stop the current tail thread (new path, stop, exit).
    tail_gen: Arc<AtomicU64>,
    /// The hub saw this session's `SessionStart` hook.
    started: Arc<AtomicBool>,
    /// `Ready` was reported: messages may be typed; the trust prompt is no longer looked for.
    ready: Arc<AtomicBool>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Why typing must not happen now, if it must not.
fn blocked(pty: &PtyProcess, sink: &Sink) -> Option<&'static str> {
    if sink.held() {
        Some("a permission request is pending")
    } else if screen::dialog_open(&pty.grid()) {
        Some("a dialog is open on the agent's screen")
    } else {
        None
    }
}

fn type_message(pty: &PtyProcess, sink: &Sink, timing: ClaudeTiming, text: String, bytes: &[u8]) {
    std::thread::sleep(timing.pre_type);
    if let Some(why) = blocked(pty, sink) {
        sink.emit(DriverEvent::DeliveryAborted { text, reason: format!("{why}; nothing was typed") });
        return;
    }
    if pty.write(bytes).is_err() {
        sink.emit(DriverEvent::DeliveryAborted { text, reason: "the agent's terminal is closed".into() });
        return;
    }
    std::thread::sleep(timing.type_pause);
    if let Some(why) = blocked(pty, sink) {
        let _ = pty.write(CLEAR_LINE);
        let reason = format!("{why} before Enter; the typed text was cleared, not sent");
        sink.emit(DriverEvent::DeliveryAborted { text, reason });
        return;
    }
    let _ = pty.write(b"\r");
}

impl ClaudeDriver {
    /// Spawns `spec`. `resuming`: the launch passes `--resume` (an exit within
    /// `resume_window` is then reported as `ResumeFailed`).
    pub fn spawn(
        spec: PtySpec,
        sink: Sink,
        timing: ClaudeTiming,
        resuming: bool,
    ) -> Result<Arc<ClaudeDriver>, DriverError> {
        let tail_gen = Arc::new(AtomicU64::new(0));
        let (exit_sink, exit_gen) = (sink.clone(), tail_gen.clone());
        let born = Instant::now();
        let pty = PtyProcess::spawn(&spec, move |code| {
            exit_gen.fetch_add(1, Ordering::SeqCst);
            let after = born.elapsed();
            if resuming && after < timing.resume_window {
                let reason = format!(
                    "claude exited {:.1} s after --resume (exit code {code:?}); its conversation may be gone",
                    after.as_secs_f32()
                );
                exit_sink.emit(DriverEvent::ResumeFailed { reason });
            }
            exit_sink.emit(DriverEvent::Exited { code });
        })?;
        let (tx, rx) = mpsc::channel::<Typing>();
        let (typist_pty, typist_sink) = (pty.clone(), sink.clone());
        std::thread::spawn(move || {
            for t in rx {
                match t {
                    Typing::Message(text, bytes) => type_message(&typist_pty, &typist_sink, timing, text, &bytes),
                    Typing::Keys(k) => {
                        let _ = typist_pty.write(&k);
                    }
                }
            }
        });
        let started = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let driver = Arc::new(ClaudeDriver {
            pty: pty.clone(),
            sink: sink.clone(),
            typist: Mutex::new(tx),
            timing,
            tail_path: Mutex::new(None),
            tail_gen,
            started: started.clone(),
            ready: ready.clone(),
        });
        std::thread::spawn(move || watch_boot(&pty, &sink, timing, &started, &ready));
        Ok(driver)
    }

    fn type_(&self, t: Typing) -> Result<(), DriverError> {
        lock(&self.typist).send(t).map_err(|_| DriverError::Failed("typist stopped".into()))
    }
}

/// Reports `TrustPrompt` when the trust dialog shows (once per appearance, only before the
/// first `Ready`) and `Ready` once: `SessionStart` seen, no dialog, and either the
/// "manual mode on" footer or an input prompt on a screen unchanged for `ready_stable`.
/// Never answers anything itself.
fn watch_boot(pty: &PtyProcess, sink: &Sink, timing: ClaudeTiming, started: &AtomicBool, ready: &AtomicBool) {
    let start = Instant::now();
    let mut trust_shown = false;
    let (mut last, mut since) = (String::new(), Instant::now());
    while !pty.exited() && start.elapsed() < timing.boot_limit {
        let grid = pty.grid();
        if grid != last {
            last = grid.clone();
            since = Instant::now();
        }
        if screen::is_trust_dialog(&grid) {
            if !trust_shown {
                trust_shown = true;
                sink.emit(DriverEvent::Boot(BootState::TrustPrompt));
            }
        } else {
            trust_shown = false;
            let settled = screen::has_manual_mode_footer(&grid)
                || (screen::has_input_prompt(&grid) && since.elapsed() >= timing.ready_stable);
            if started.load(Ordering::SeqCst) && !screen::dialog_open(&grid) && settled {
                ready.store(true, Ordering::SeqCst);
                sink.emit(DriverEvent::Boot(BootState::Ready));
                return;
            }
        }
        std::thread::sleep(timing.tick);
    }
}

impl Drop for ClaudeDriver {
    fn drop(&mut self) {
        self.tail_gen.fetch_add(1, Ordering::SeqCst);
        if !self.pty.exited() {
            self.pty.kill_tree(Duration::from_millis(200));
        }
    }
}

impl Driver for ClaudeDriver {
    fn send(&self, text: &str) -> Result<(), DriverError> {
        if !self.ready.load(Ordering::SeqCst) {
            return Err(DriverError::NotNow("Claude is not ready yet (starting, or at the trust prompt)".into()));
        }
        if blocked(&self.pty, &self.sink).is_some() {
            return Err(DriverError::Failed("a dialog is open on the agent's screen".into()));
        }
        let bytes = paste::typed_message(text).map_err(DriverError::Failed)?;
        self.type_(Typing::Message(text.to_string(), bytes))
    }

    fn interrupt(&self) -> Result<(), DriverError> {
        self.type_(Typing::Keys(ESC.to_vec()))
    }

    fn answer_permission(&self, detail: &str, allow: bool) -> Result<(), DriverError> {
        let grid = self.pty.grid();
        if !screen::has_permission_prompt(&grid) {
            return Err(DriverError::NoDialog("no permission dialog on screen".into()));
        }
        if !screen::dialog_matches(&grid, detail) {
            return Err(DriverError::NoDialog(
                "the dialog on screen is for another request; answer it in `mirante attach`".into(),
            ));
        }
        let label = if allow { "Yes" } else { "No" };
        let key = screen::option_key(&grid, label)
            .ok_or_else(|| DriverError::NoDialog(format!("the dialog has no plain \"{label}\" option")))?;
        self.type_(Typing::Keys(vec![key as u8]))
    }

    fn trust(&self, accept: bool) -> Result<(), DriverError> {
        if self.ready.load(Ordering::SeqCst) {
            return Err(DriverError::NoDialog("the session is past its trust prompt".into()));
        }
        let key = screen::trust_key(&self.pty.grid())
            .ok_or_else(|| DriverError::NoDialog(format!("no trust dialog with a \"{}\" option", screen::TRUST_YES)))?;
        if !accept {
            let (pty, grace) = (self.pty.clone(), self.timing.stop_grace);
            std::thread::spawn(move || pty.kill_tree(grace));
            return Ok(());
        }
        self.type_(Typing::Keys(vec![key as u8]))
    }

    fn dialog_open(&self) -> bool {
        screen::dialog_open(&self.pty.grid())
    }

    fn tail_transcript(&self, path: &Path, offset: u64) {
        let mut cur = lock(&self.tail_path);
        if cur.as_deref() == Some(path) {
            return;
        }
        *cur = Some(path.to_path_buf());
        let my_gen = self.tail_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let (gen, sink, poll) = (self.tail_gen.clone(), self.sink.clone(), self.timing.tail_poll);
        let mut tail = TranscriptTail::new(path, offset);
        let path = path.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            let mut last = offset;
            while gen.load(Ordering::SeqCst) == my_gen {
                // Catch up while the batch says more is waiting (never loop on file length:
                // a partial last line keeps the offset behind it).
                loop {
                    match tail.poll() {
                        Ok(batch) => {
                            for line in batch.lines {
                                sink.emit(DriverEvent::Transcript(line));
                            }
                            if tail.offset() != last {
                                last = tail.offset();
                                sink.emit(DriverEvent::TranscriptOffset { path: path.clone(), offset: last });
                            }
                            if !batch.more {
                                break;
                            }
                        }
                        Err(e) => {
                            sink.emit(DriverEvent::Step(json!({ "transcript_error": e.to_string() })));
                            break;
                        }
                    }
                }
                std::thread::sleep(poll);
            }
        });
    }

    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError> {
        self.pty.resize(rows, cols)?;
        let (id, initial, output) = self.pty.attach();
        Ok(Attachment { id, initial, output, input: true })
    }

    fn input(&self, _attachment: u64, bytes: &[u8]) -> Result<(), DriverError> {
        Ok(self.pty.write(bytes)?)
    }

    fn detach(&self, attachment: u64) {
        self.pty.unsubscribe(attachment);
    }

    fn resize(&self, rows: u16, cols: u16) -> Result<(), DriverError> {
        Ok(self.pty.resize(rows, cols)?)
    }

    fn screen_tail(&self) -> String {
        screen::tail(&self.pty.grid(), 20)
    }

    fn stop(&self) {
        self.tail_gen.fetch_add(1, Ordering::SeqCst);
        if !self.pty.exited() {
            self.pty.kill_tree(self.timing.stop_grace);
            self.pty.wait_exit(Duration::from_secs(2));
        }
    }

    fn session_started(&self) {
        self.started.store(true, Ordering::SeqCst);
    }

    fn root(&self) -> Option<Proc> {
        self.pty.root()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::proctree;
    use mirante_ipc::test_dir::TestDir;
    use std::sync::atomic::AtomicBool;

    fn fast() -> ClaudeTiming {
        ClaudeTiming {
            tick: Duration::from_millis(20),
            pre_type: Duration::from_millis(150),
            type_pause: Duration::from_millis(50),
            tail_poll: Duration::from_millis(20),
            stop_grace: Duration::from_secs(1),
            boot_limit: Duration::from_secs(10),
            ready_stable: Duration::from_millis(150),
            resume_window: Duration::from_secs(3),
        }
    }

    struct Fake {
        d: Arc<ClaudeDriver>,
        rx: mpsc::Receiver<(String, DriverEvent)>,
        /// The hub's side of the hold flag.
        hold: Arc<AtomicBool>,
    }

    /// A fake `claude`: a shell script in the PTY (raw mode, no echo).
    fn fake_with(script: &str, timing: ClaudeTiming, resuming: bool) -> Fake {
        let (tx, rx) = mpsc::channel();
        let spec = PtySpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), format!("stty raw -echo; {script}")],
            cwd: std::env::temp_dir(),
            env_remove: vec![],
            env_set: vec![("TERM".into(), "xterm-256color".into())],
            rows: 30,
            cols: 100,
        };
        let sink = Sink::new("s1", tx);
        let hold = sink.hold_flag();
        Fake { d: ClaudeDriver::spawn(spec, sink, timing, resuming).unwrap(), rx, hold }
    }

    fn fake(script: &str) -> Fake {
        fake_with(script, fast(), false)
    }

    fn next(rx: &mpsc::Receiver<(String, DriverEvent)>, want: impl Fn(&DriverEvent) -> bool) -> DriverEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let (_, ev) = rx.recv_timeout(left).expect("event in time");
            if want(&ev) {
                return ev;
            }
        }
    }

    fn wait_grid(d: &ClaudeDriver, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !d.pty.grid().contains(text) {
            assert!(Instant::now() < deadline, "screen never showed {text:?}:\n{}", d.pty.grid());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Ready on screen (footer fast path); the test still has to report `SessionStart`.
    const READY: &str = "printf '\\033[2J\\033[H> \\r\\n  manual mode on\\r\\n'";

    fn ready(f: &Fake) {
        f.d.session_started();
        assert_eq!(next(&f.rx, |e| matches!(e, DriverEvent::Boot(_))), DriverEvent::Boot(BootState::Ready));
    }

    const DIALOG: &[u8] = b"\x1b[2J\x1b[H Bash command\r\n   touch x\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n   2. No\r\n Esc to cancel\r\n";

    #[test]
    fn the_trust_prompt_waits_for_the_user_then_its_exact_option_is_typed() {
        let f = fake(
            "printf 'Do you trust the files in this folder?\\r\\n \\342\\235\\257 1. No, exit\\r\\n   2. Yes, I trust this folder\\r\\n Enter to confirm \\302\\267 Esc to exit\\r\\n'; \
             k=$(dd bs=1 count=1 2>/dev/null); \
             printf '\\033[2J\\033[Hkeys:%s\\r\\n> \\r\\n  manual mode on\\r\\n' \"$k\"; sleep 5",
        );
        assert_eq!(next(&f.rx, |e| matches!(e, DriverEvent::Boot(_))), DriverEvent::Boot(BootState::TrustPrompt));
        assert!(f.d.dialog_open());
        assert!(matches!(f.d.send("hi"), Err(DriverError::NotNow(_))));
        f.d.trust(true).unwrap();
        wait_grid(&f.d, "keys:2");
        ready(&f);
        assert!(matches!(f.d.trust(true), Err(DriverError::NoDialog(_))));
    }

    #[test]
    fn declining_trust_ends_the_process() {
        let f = fake(
            "printf ' \\342\\235\\257 1. No, exit\\r\\n   2. Yes, I trust this folder\\r\\n Enter to confirm\\r\\n'; sleep 30",
        );
        next(&f.rx, |e| *e == DriverEvent::Boot(BootState::TrustPrompt));
        f.d.trust(false).unwrap();
        assert!(matches!(next(&f.rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { .. }));
    }

    #[test]
    fn ready_needs_session_start_and_a_settled_input_prompt() {
        let f = fake("printf '> '; sleep 5");
        wait_grid(&f.d, ">");
        // The prompt alone is not enough: no SessionStart yet.
        std::thread::sleep(Duration::from_millis(400));
        assert!(f.rx.try_iter().all(|(_, e)| !matches!(e, DriverEvent::Boot(_))));
        ready(&f);
    }

    #[test]
    fn nothing_is_typed_before_ready() {
        let f = fake(&format!(
            "{READY}; stty min 0 time 10; k=$(dd bs=1 count=10 2>/dev/null); printf 'got:[%s]' \"$k\"; sleep 5"
        ));
        wait_grid(&f.d, "manual mode on");
        assert!(matches!(f.d.send("hi"), Err(DriverError::NotNow(_))));
        wait_grid(&f.d, "got:[]");
    }

    #[test]
    fn messages_are_one_bracketed_paste_then_enter() {
        let dir = TestDir::new();
        let out = dir.path().join("typed");
        let want = [paste::bracketed("use\r\ntabs\x1b[201~ now"), b"\r".to_vec(), b"/compact\r".to_vec()].concat();
        let f = fake(&format!(
            "{READY}; dd bs=1 count={} of={} 2>/dev/null; printf 'done'; sleep 5",
            want.len(),
            out.display()
        ));
        ready(&f);
        assert!(f.d.send("!rm -rf build").unwrap_err().to_string().contains("attach and type it yourself"));
        f.d.send("use\r\ntabs\x1b[201~ now").unwrap();
        f.d.send("/compact").unwrap();
        wait_grid(&f.d, "done");
        assert_eq!(std::fs::read(&out).unwrap(), want);
        assert_eq!(want[..6], *b"\x1b[200~");
    }

    #[test]
    fn two_concurrent_sends_never_interleave() {
        let dir = TestDir::new();
        let out = dir.path().join("typed");
        let (a, b) = ("first message ".repeat(20), "second message ".repeat(20));
        let pa = [paste::bracketed(&a), b"\r".to_vec()].concat();
        let pb = [paste::bracketed(&b), b"\r".to_vec()].concat();
        let f = fake(&format!(
            "{READY}; dd bs=1 count={} of={} 2>/dev/null; printf 'done'; sleep 5",
            pa.len() + pb.len(),
            out.display()
        ));
        ready(&f);
        let (d1, d2) = (f.d.clone(), f.d.clone());
        let t1 = std::thread::spawn(move || d1.send(&a).unwrap());
        let t2 = std::thread::spawn(move || d2.send(&b).unwrap());
        t1.join().unwrap();
        t2.join().unwrap();
        wait_grid(&f.d, "done");
        let got = std::fs::read(&out).unwrap();
        assert!(
            got == [pa.clone(), pb.clone()].concat() || got == [pb, pa].concat(),
            "{:?}",
            String::from_utf8_lossy(&got)
        );
    }

    #[test]
    fn a_dialog_that_appears_before_the_keystrokes_gets_nothing() {
        let f = fake(&format!(
            "{READY}; stty min 0 time 15; k=$(dd bs=1 count=10 2>/dev/null); printf 'got:[%s]' \"$k\"; sleep 5"
        ));
        ready(&f);
        f.d.send("1").unwrap();
        // The dialog opens between `send` and the first keystroke.
        f.d.pty.mirror(DIALOG);
        let ev = next(&f.rx, |e| matches!(e, DriverEvent::DeliveryAborted { .. }));
        assert!(
            matches!(ev, DriverEvent::DeliveryAborted { ref text, ref reason } if text == "1" && reason.contains("nothing was typed"))
        );
        wait_grid(&f.d, "got:[]");
    }

    #[test]
    fn a_permission_pending_before_enter_clears_the_line_instead_of_submitting() {
        let pasted = paste::bracketed("go on");
        let f = fake_with(
            &format!(
                "{READY}; dd bs=1 count={} >/dev/null 2>&1; printf 'pasted'; stty min 0 time 20; \
                 k=$(dd bs=1 count=1 2>/dev/null | od -An -b | tr -d ' '); printf ' next:[%s]' \"$k\"; sleep 5",
                pasted.len()
            ),
            ClaudeTiming { type_pause: Duration::from_millis(800), ..fast() },
            false,
        );
        ready(&f);
        f.d.send("go on").unwrap();
        wait_grid(&f.d, "pasted");
        f.hold.store(true, Ordering::SeqCst);
        let ev = next(&f.rx, |e| matches!(e, DriverEvent::DeliveryAborted { .. }));
        assert!(matches!(ev, DriverEvent::DeliveryAborted { ref reason, .. } if reason.contains("before Enter")));
        // Ctrl-U (octal 025), never Enter.
        wait_grid(&f.d, "next:[025]");
        assert!(matches!(f.d.send("more"), Err(DriverError::Failed(_))));
    }

    #[test]
    fn permission_answer_types_the_exact_option_for_the_matching_dialog() {
        let dialog = "printf '\\033[2J\\033[H Bash command\\r\\n   touch x\\r\\n Do you want to proceed?\\r\\n \\342\\235\\257 1. Yes\\r\\n   2. Yes, and always allow access to /w\\r\\n   3. No\\r\\n Esc to cancel\\r\\n'";
        let f = fake(&format!(
            "{READY}; sleep 1; {dialog}; k=$(dd bs=1 count=1 2>/dev/null); printf '\\r\\npicked:%s' \"$k\"; sleep 5"
        ));
        ready(&f);
        wait_grid(&f.d, "Do you want to proceed?");
        assert!(f.d.dialog_open());
        assert!(matches!(f.d.answer_permission("rm -rf /", true), Err(DriverError::NoDialog(_))));
        f.d.answer_permission("touch x", false).unwrap();
        wait_grid(&f.d, "picked:3");
    }

    #[test]
    fn transcript_lines_and_offsets_are_emitted_from_the_given_offset() {
        let dir = TestDir::new();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "{\"old\":true}\n{\"type\":\"user\"}\n").unwrap();
        let f = fake("sleep 5");
        f.d.tail_transcript(&path, 13);
        assert_eq!(
            next(&f.rx, |e| matches!(e, DriverEvent::Transcript(_))),
            DriverEvent::Transcript(json!({"type": "user"}))
        );
        let off = next(&f.rx, |e| matches!(e, DriverEvent::TranscriptOffset { .. }));
        assert_eq!(off, DriverEvent::TranscriptOffset { path: path.to_string_lossy().into_owned(), offset: 29 });
        f.d.tail_transcript(&path, 0);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"n\":2}\n"))
            .unwrap();
        assert_eq!(next(&f.rx, |e| matches!(e, DriverEvent::Transcript(_))), DriverEvent::Transcript(json!({"n": 2})));
    }

    #[test]
    fn stop_ends_the_whole_tree_even_a_child_forked_during_the_kill() {
        let dir = TestDir::new();
        let (pidfile, readyfile, child) =
            (dir.path().join("forked.pid"), dir.path().join("ready"), dir.path().join("child.sh"));
        std::fs::write(
            &child,
            format!(
                "trap 'perl -e \"setpgrp(0,0); sleep 60\" & echo $! > {}' TERM HUP\n: > {}\nwhile :; do sleep 0.1; done\n",
                pidfile.display(),
                readyfile.display()
            ),
        )
        .unwrap();
        let f = fake(&format!("sh {} & wait; sleep 60", child.display()));
        let root = f.d.root().expect("root");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !readyfile.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        f.d.stop();
        assert!(matches!(next(&f.rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { .. }));
        assert!(!proctree::alive(root));
        let forked: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(proctree::proc_of(forked).is_none(), "the child forked during the kill survived");
    }

    #[test]
    fn a_resume_that_exits_at_once_is_reported_never_restarted() {
        let f = fake_with("exit 3", fast(), true);
        let ev = next(&f.rx, |e| matches!(e, DriverEvent::ResumeFailed { .. } | DriverEvent::Exited { .. }));
        assert!(matches!(ev, DriverEvent::ResumeFailed { ref reason } if reason.contains("--resume")), "{ev:?}");
        assert_eq!(next(&f.rx, |e| matches!(e, DriverEvent::Exited { .. })), DriverEvent::Exited { code: Some(3) });
        // A fresh start that exits is just an exit.
        let g = fake("exit 3");
        let ev = next(&g.rx, |e| matches!(e, DriverEvent::ResumeFailed { .. } | DriverEvent::Exited { .. }));
        assert_eq!(ev, DriverEvent::Exited { code: Some(3) });
    }

    #[test]
    fn attach_repaints_with_input_modes_then_streams_and_input_reaches_the_agent() {
        let f =
            fake("printf '\\033[?2004hhello'; k=$(dd bs=1 count=2 2>/dev/null); printf ' typed:%s' \"$k\"; sleep 5");
        wait_grid(&f.d, "hello");
        let a = f.d.attach(24, 80).unwrap();
        let initial = String::from_utf8_lossy(&a.initial);
        assert!(initial.contains("hello") && initial.contains("\x1b[?2004h"), "{initial:?}");
        f.d.input(a.id, b"ok").unwrap();
        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains("typed:ok") {
            seen.extend(a.output.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        f.d.detach(a.id);
    }

    #[test]
    fn arbitrary_bytes_pass_through_attach_unchanged() {
        let dir = TestDir::new();
        let file = dir.path().join("bytes");
        // Not UTF-8, every value but ESC (the one byte terminal queries start with).
        let mut x: u32 = 0x1234_5678;
        let data: Vec<u8> = std::iter::from_fn(|| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            Some((x >> 24) as u8)
        })
        .filter(|b| *b != 0x1b)
        .take(4096)
        .collect();
        assert!(std::str::from_utf8(&data).is_err());
        std::fs::write(&file, &data).unwrap();
        let f = fake(&format!("printf R; dd bs=1 count=1 >/dev/null 2>&1; cat {}; sleep 5", file.display()));
        wait_grid(&f.d, "R");
        let a = f.d.attach(30, 100).unwrap();
        f.d.input(a.id, b"g").unwrap();
        let mut seen = Vec::new();
        while seen.len() < data.len() {
            seen.extend(a.output.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        assert_eq!(seen, data);
    }
}
