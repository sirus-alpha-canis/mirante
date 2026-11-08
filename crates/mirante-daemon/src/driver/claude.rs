//! Claude Code driver: the unmodified interactive `claude` in a PTY (FINDINGS: the
//! human-attended mode that fits the terms, steers mid-run and resumes). Status comes from
//! hooks (via `mirante-hook` → daemon) fused with this driver's transcript tail; the screen
//! is read only to act on dialogs.
//! - Boot: the workspace-trust dialog is reported as `TrustPrompt` and answered only by the
//!   user (`trust`). The real 2.1.285 dialog has no digits and starts on "No, exit": the
//!   answer is one Down, then Enter once the screen shows the cursor on the exact "Yes, I
//!   trust this folder" option (a numbered dialog gets that option's digit instead).
//!   `Ready` needs the `SessionStart` hook, the input prompt, no dialog, and a screen that
//!   stayed unchanged for `ready_stable` (the "manual mode on" footer skips the wait).
//!   There is no time-based fallback: an unknown startup screen keeps the session booting.
//!   After `boot_limit` the watcher keeps polling, slower, and reports (a `boot_waiting`
//!   step) what readiness still waits for.
//! - Everything typed goes through one typist thread, one thing at a time: messages, Esc,
//!   permission and trust answers.
//! - A message is a bracketed paste, a pause, Enter. Right before the paste AND right
//!   before Enter the typist checks again that no dialog is on screen and that the hub holds
//!   no pending permission for the session: a dialog can appear between `send` and the
//!   keystrokes, and typed text would answer it. A message stopped that way is handed back
//!   (`DeliveryAborted`). Stopped after the paste, nothing more is typed then (a dialog may
//!   be taking keys); the pasted text is left in the input box and cleared (Ctrl-E, two
//!   Ctrl-U per pasted line) right before the next paste, once nothing blocks
//!   typing, so the requeued message is never sent twice. Never Enter, Esc or a digit.
//!   While the hub holds the session (a permission is pending) the typist hands queued
//!   messages back at once, and cuts the pauses of the one it is typing short, so an answer
//!   never waits behind steers.
//! - A blocked `send` is `NotNow`, its reason starting with `hold` (a permission request is
//!   pending) or `dialog` (a dialog is on screen). Once nothing blocks any more, the driver
//!   reports `DialogClosed` so the hub delivers what it requeued.
//! - Permission answers (only when the hub's Claude inline answers are on, and only with
//!   its evidence): the digit of the exact "Yes" / "No" option of the dialog that is
//!   exactly the request's (tool title and the whole command or path; see
//!   `screen::dialog_matches`, defence in depth only: screen text can never prove which
//!   request a dialog is). Checked when asked AND again by the typist right before the key,
//!   on the screen as it is then, and the hub's `AnswerGuard` (card generation, hook-loss
//!   marker) is the very last check; if anything changed, nothing is typed and the answer
//!   fails (`NoDialog`). No guard, no key. Trust answers are re-checked on the screen too.
//! - Interrupt (Esc) is refused (`NotNow`) while a permission is pending or a dialog is
//!   open: Esc would answer that dialog ("Esc to cancel"). Checked when asked AND by the
//!   typist right before the key (the caller waits for it), so an Esc that waited behind a
//!   message never lands in a dialog that opened meanwhile.
//! - Stop ends the whole process tree (SIGTERM, grace, SIGKILL; see `proctree`), also what
//!   an agent that already exited left behind (see `pty`).
//! - A `--resume` whose process exits within `resume_window`, before `SessionStart` or
//!   `Ready`, reports `ResumeFailed`; a fresh start is the user's call.
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
use super::{screen, AnswerGuard, Attachment, Driver, DriverError, DriverEvent, Sink};

const ESC: &[u8] = b"\x1b";
/// Ctrl-E: end of the line in the input box.
const END_OF_LINE: u8 = 0x05;
/// Ctrl-U: clears the input line. Not an answer to any dialog.
const CLEAR_LINE: u8 = 0x15;
/// How long an answer may wait for the typist (it may be finishing a message).
const ANSWER_WAIT: Duration = Duration::from_secs(3);
/// Why an interrupt is refused while a dialog is pending or open.
pub const INTERRUPT_REFUSED: &str = "dialog open — answer or deny it first (mirante perms / mirante attach)";

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
    /// After this long without `Ready` the boot watcher reports what it waits for and polls
    /// slower (the session stays `Booting`).
    pub boot_limit: Duration,
    /// How long the input prompt must stay unchanged before `Ready` (no footer fast path).
    pub ready_stable: Duration,
    /// A resumed process that exits sooner than this failed to resume.
    pub resume_window: Duration,
    /// After `Ready`: how often the screen and the hold flag are checked for a dialog that
    /// closed (`DialogClosed`).
    pub monitor: Duration,
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
            monitor: Duration::from_millis(250),
        }
    }
}

type Reply = mpsc::Sender<Result<(), DriverError>>;

enum Typing {
    /// (the message as given, the bytes to type before Enter)
    Message(String, Vec<u8>),
    /// Esc, typed only if nothing blocks typing right before the key (else `NotNow`).
    Interrupt { deadline: Instant, reply: Reply },
    /// A permission answer, checked against the screen and the hub's evidence (`guard`)
    /// right before its key is typed.
    Answer {
        detail: String,
        tool: String,
        description: Option<String>,
        label: &'static str,
        guard: AnswerGuard,
        deadline: Instant,
        reply: Reply,
    },
    /// "Yes, I trust this folder", checked against the screen right before its key is typed.
    Trust { deadline: Instant, reply: Reply },
}

pub struct ClaudeDriver {
    pty: Arc<PtyProcess>,
    sink: Sink,
    typist: Mutex<mpsc::Sender<Typing>>,
    timing: ClaudeTiming,
    /// The worktree (paths in dialogs are shown relative to it).
    cwd: PathBuf,
    tail_path: Mutex<Option<PathBuf>>,
    /// Bumped to stop the current tail thread (new path, stop, exit).
    tail_gen: Arc<AtomicU64>,
    /// The hub saw this session's `SessionStart` hook.
    started: Arc<AtomicBool>,
    /// `Ready` was reported: messages may be typed; the trust prompt is no longer looked for.
    ready: Arc<AtomicBool>,
    /// Set when the driver is stopped or dropped: the screen watcher ends.
    gone: Arc<AtomicBool>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Why typing must not happen now, if it must not: `hold: …` or `dialog: …`.
fn blocked(pty: &PtyProcess, sink: &Sink) -> Option<&'static str> {
    if sink.held() {
        Some("hold: a permission request is pending")
    } else if screen::dialog_open(&pty.grid()) {
        Some("dialog: a dialog is open on the agent's screen")
    } else {
        None
    }
}

/// Sleeps `d`, in short slices, and returns early (true) as soon as the hub holds the
/// session: a held session types nothing more, so the typist must not sit out its pauses.
fn pause(sink: &Sink, d: Duration) -> bool {
    const SLICE: Duration = Duration::from_millis(5);
    let end = Instant::now() + d;
    loop {
        if sink.held() {
            return true;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        std::thread::sleep(left.min(SLICE));
    }
}

/// Clears what an aborted paste of `lines` lines left in the input box: Ctrl-E, then two
/// Ctrl-U per line. Confirmed on 2.1.285 (Task 21): each Ctrl-U either clears the text of
/// the cursor's line or joins that (empty) line to the one above, so `n` lines need
/// `2n - 1`; the spare one on an empty box does nothing. A large paste collapsed to
/// "[Pasted text #1 +N lines]" is one line and goes with the first.
pub fn clear_input(lines: usize) -> Vec<u8> {
    let mut out = vec![END_OF_LINE];
    out.extend(std::iter::repeat_n(CLEAR_LINE, 2 * lines.max(1)));
    out
}

/// Types one message. `dirty`: lines an earlier aborted paste left in the input box.
/// `owed` is set when a message is handed back: the screen watcher then reports
/// `DialogClosed` as soon as nothing blocks typing, even if it never saw the block.
fn type_message(
    pty: &PtyProcess,
    sink: &Sink,
    timing: ClaudeTiming,
    text: String,
    bytes: &[u8],
    dirty: &mut Option<usize>,
    owed: &AtomicBool,
) {
    pause(sink, timing.pre_type);
    if let Some(why) = blocked(pty, sink) {
        sink.emit(DriverEvent::DeliveryAborted { text, reason: format!("{why}; nothing was typed") });
        // After the event: the hub must have requeued it before it hears `DialogClosed`.
        owed.store(true, Ordering::SeqCst);
        return;
    }
    let mut keys = dirty.take().map(clear_input).unwrap_or_default();
    keys.extend_from_slice(bytes);
    if pty.write(&keys).is_err() {
        sink.emit(DriverEvent::DeliveryAborted { text, reason: "the agent's terminal is closed".into() });
        return;
    }
    pause(sink, timing.type_pause);
    if let Some(why) = blocked(pty, sink) {
        // Nothing more now: a dialog may be taking keys. The leftover is cleared before the
        // next paste, once nothing blocks typing.
        *dirty = Some(paste::line_count(&text));
        let reason =
            format!("{why} before Enter; nothing was submitted (the pasted text is cleared before the next message)");
        sink.emit(DriverEvent::DeliveryAborted { text, reason });
        owed.store(true, Ordering::SeqCst);
        return;
    }
    let _ = pty.write(b"\r");
}

/// The answer key for the request, from the screen as it is now.
fn answer_key(
    grid: &str,
    tool: &str,
    detail: &str,
    description: Option<&str>,
    label: &str,
    cwd: &Path,
) -> Result<char, DriverError> {
    if !screen::has_permission_prompt(grid) {
        return Err(DriverError::NoDialog("no permission dialog on screen".into()));
    }
    if !screen::dialog_matches(grid, tool, detail, description, Some(cwd)) {
        return Err(DriverError::NoDialog(
            "the dialog on screen is not exactly this request; answer it in `mirante attach`".into(),
        ));
    }
    screen::option_key(grid, label)
        .ok_or_else(|| DriverError::NoDialog(format!("the dialog has no plain \"{label}\" option")))
}

fn trust_key(grid: &str) -> Result<screen::TrustKey, DriverError> {
    screen::trust_key(grid)
        .ok_or_else(|| DriverError::NoDialog(format!("no trust dialog with a \"{}\" option", screen::TRUST_YES)))
}

/// How long the cursor may stay on "No, exit" after a Down before Down is typed again
/// (right after the trust dialog shows, Claude may not take keys yet: Task 21 live run).
const TRUST_DOWN_RETRY: Duration = Duration::from_millis(400);

/// Accepts the trust dialog, each key checked on the screen right before it: a numbered
/// dialog gets its digit; the real 2.1.285 dialog (no digits, cursor on "No, exit") gets
/// Down (again while the cursor still sits on "No, exit" after [`TRUST_DOWN_RETRY`]), then
/// Enter only once the screen shows the cursor on "Yes, I trust this folder" and the last
/// Down is at least [`TRUST_DOWN_RETRY`] old (a late Down never moves the cursor between
/// that check and Enter). Nothing is typed after `deadline`.
fn type_trust(pty: &PtyProcess, sink: &Sink, deadline: Instant, tick: Duration) -> Result<(), DriverError> {
    let mut last_down: Option<Instant> = None;
    let r = loop {
        if Instant::now() >= deadline {
            break Err(DriverError::Failed(
                "the trust answer waited too long to be typed; nothing more was typed".into(),
            ));
        }
        match trust_key(&pty.grid()) {
            Err(e) => break Err(e),
            Ok(screen::TrustKey::Digit(d)) => break pty.write(&[d as u8]).map_err(DriverError::from),
            // Not while a Down typed less than TRUST_DOWN_RETRY ago may still move the cursor.
            Ok(screen::TrustKey::Enter) if last_down.is_none_or(|t| t.elapsed() >= TRUST_DOWN_RETRY) => {
                break pty.write(b"\r").map_err(DriverError::from)
            }
            Ok(screen::TrustKey::Enter) => {}
            Ok(screen::TrustKey::Down) if last_down.is_none_or(|t| t.elapsed() >= TRUST_DOWN_RETRY) => {
                last_down = Some(Instant::now());
                if let Err(e) = pty.write(b"\x1b[B") {
                    break Err(e.into());
                }
            }
            Ok(screen::TrustKey::Down) => {}
        }
        std::thread::sleep(tick);
    };
    if let Err(e) = &r {
        sink.emit(DriverEvent::Step(
            json!({ "type": "answer_aborted", "what": "trust answer", "error": e.to_string() }),
        ));
    }
    r
}

/// Types `key` when `check` still finds it on the screen right now and `guard` (the hub's
/// evidence) still holds, before `deadline`. The guard is the very last check before the
/// key.
fn type_checked(
    pty: &PtyProcess,
    sink: &Sink,
    what: &str,
    deadline: Instant,
    check: impl FnOnce(&str) -> Result<char, DriverError>,
    guard: Option<&AnswerGuard>,
) -> Result<(), DriverError> {
    let r = if Instant::now() >= deadline {
        Err(DriverError::Failed(format!("the {what} waited too long to be typed; nothing was typed")))
    } else {
        check(&pty.grid()).and_then(|key| {
            if let Some(why) = guard.and_then(|g| g().err()) {
                return Err(DriverError::NoDialog(format!("{why}; nothing was typed, answer it in `mirante attach`")));
            }
            Ok(pty.write(&[key as u8])?)
        })
    };
    if let Err(e) = &r {
        sink.emit(DriverEvent::Step(json!({ "type": "answer_aborted", "what": what, "error": e.to_string() })));
    }
    r
}

impl ClaudeDriver {
    /// Spawns `spec`. `resuming`: the launch passes `--resume` (an exit within
    /// `resume_window`, before `SessionStart` or `Ready`, is then reported as `ResumeFailed`).
    pub fn spawn(
        spec: PtySpec,
        sink: Sink,
        timing: ClaudeTiming,
        resuming: bool,
    ) -> Result<Arc<ClaudeDriver>, DriverError> {
        let tail_gen = Arc::new(AtomicU64::new(0));
        let started = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicBool::new(false));
        let (exit_sink, exit_gen) = (sink.clone(), tail_gen.clone());
        let (exit_started, exit_ready) = (started.clone(), ready.clone());
        let born = Instant::now();
        let pty = PtyProcess::spawn(&spec, move |code| {
            exit_gen.fetch_add(1, Ordering::SeqCst);
            let after = born.elapsed();
            let came_up = exit_started.load(Ordering::SeqCst) || exit_ready.load(Ordering::SeqCst);
            if resuming && after < timing.resume_window && !came_up {
                let reason = format!(
                    "claude exited {:.1} s after --resume (exit code {code:?}); its conversation may be gone",
                    after.as_secs_f32()
                );
                exit_sink.emit(DriverEvent::ResumeFailed { reason });
            }
            exit_sink.emit(DriverEvent::Exited { code });
        })?;
        let (tx, rx) = mpsc::channel::<Typing>();
        let owed = Arc::new(AtomicBool::new(false));
        let (typist_pty, typist_sink, cwd, typist_owed) = (pty.clone(), sink.clone(), spec.cwd.clone(), owed.clone());
        std::thread::spawn(move || {
            let mut dirty = None;
            for t in rx {
                match t {
                    Typing::Message(text, bytes) => {
                        type_message(&typist_pty, &typist_sink, timing, text, &bytes, &mut dirty, &typist_owed)
                    }
                    Typing::Interrupt { deadline, reply } => {
                        // Checked again right before the key: a dialog may have opened (or a
                        // card become pending) while this waited behind a message.
                        let r = if Instant::now() >= deadline {
                            Err(DriverError::Failed("the interrupt waited too long; nothing was typed".into()))
                        } else if blocked(&typist_pty, &typist_sink).is_some() {
                            Err(DriverError::NotNow(INTERRUPT_REFUSED.into()))
                        } else {
                            typist_pty.write(ESC).map_err(DriverError::from)
                        };
                        let _ = reply.send(r);
                    }
                    Typing::Answer { detail, tool, description, label, guard, deadline, reply } => {
                        let check = |g: &str| answer_key(g, &tool, &detail, description.as_deref(), label, &cwd);
                        let r =
                            type_checked(&typist_pty, &typist_sink, "permission answer", deadline, check, Some(&guard));
                        let _ = reply.send(r);
                    }
                    Typing::Trust { deadline, reply } => {
                        let r = type_trust(&typist_pty, &typist_sink, deadline, timing.tick);
                        let _ = reply.send(r);
                    }
                }
            }
        });
        let gone = Arc::new(AtomicBool::new(false));
        let driver = Arc::new(ClaudeDriver {
            pty: pty.clone(),
            sink: sink.clone(),
            typist: Mutex::new(tx),
            timing,
            cwd: spec.cwd.clone(),
            tail_path: Mutex::new(None),
            tail_gen,
            started: started.clone(),
            ready: ready.clone(),
            gone: gone.clone(),
        });
        std::thread::spawn(move || {
            if watch_boot(&pty, &sink, timing, &started, &ready, &gone) {
                watch_dialogs(&pty, &sink, timing, &gone, &owed);
            }
        });
        Ok(driver)
    }

    fn type_(&self, t: Typing) -> Result<(), DriverError> {
        lock(&self.typist).send(t).map_err(|_| DriverError::Failed("typist stopped".into()))
    }

    /// Hands a checked answer to the typist and waits for it to be typed (or refused).
    fn type_answer(&self, make: impl FnOnce(Instant, Reply) -> Typing) -> Result<(), DriverError> {
        let (reply, result) = mpsc::channel();
        let deadline = Instant::now() + ANSWER_WAIT;
        self.type_(make(deadline, reply))?;
        result
            .recv_timeout(ANSWER_WAIT + Duration::from_secs(1))
            .map_err(|_| DriverError::Failed("the answer was not typed in time".into()))?
    }
}

/// What readiness still waits for, in words.
fn waiting_for(grid: &str, started: bool, since: Duration, timing: ClaudeTiming) -> Vec<&'static str> {
    let mut w = Vec::new();
    if !started {
        w.push("the SessionStart hook");
    }
    if screen::is_trust_dialog(grid) {
        w.push("your answer to the trust prompt (mirante trust)");
    } else if screen::dialog_open(grid) {
        w.push("a dialog on the agent's screen to close (mirante attach)");
    }
    if !screen::has_manual_mode_footer(grid) {
        if !screen::has_input_prompt(grid) {
            w.push("the input prompt");
        } else if since < timing.ready_stable {
            w.push("the screen to settle");
        }
    }
    w
}

/// Reports `TrustPrompt` when the trust dialog shows (once per appearance, only before the
/// first `Ready`) and `Ready` once: `SessionStart` seen, no dialog, and either the
/// "manual mode on" footer or an input prompt on a screen unchanged for `ready_stable`.
/// After `boot_limit` it polls ten times slower and reports what it waits for (again when
/// that changes). Never answers anything itself. True once `Ready` was reported.
fn watch_boot(
    pty: &PtyProcess,
    sink: &Sink,
    timing: ClaudeTiming,
    started: &AtomicBool,
    ready: &AtomicBool,
    gone: &AtomicBool,
) -> bool {
    let start = Instant::now();
    let mut trust_shown = false;
    let mut reported: Option<Vec<&'static str>> = None;
    let (mut last, mut since) = (String::new(), Instant::now());
    while !pty.exited() && !gone.load(Ordering::SeqCst) {
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
                return true;
            }
        }
        let late = start.elapsed() >= timing.boot_limit;
        if late {
            let w = waiting_for(&grid, started.load(Ordering::SeqCst), since.elapsed(), timing);
            if reported.as_ref() != Some(&w) {
                sink.emit(DriverEvent::Step(json!({
                    "type": "boot_waiting",
                    "after_s": start.elapsed().as_secs(),
                    "waiting_for": w,
                })));
                reported = Some(w);
            }
        }
        std::thread::sleep(if late { timing.tick * 10 } else { timing.tick });
    }
    false
}

/// After `Ready`: reports `DialogClosed` each time typing stops being blocked (a dialog
/// left the screen, or the hold was released), and once nothing blocks after a message was
/// handed back (`owed`), so the hub can deliver what it requeued.
fn watch_dialogs(pty: &PtyProcess, sink: &Sink, timing: ClaudeTiming, gone: &AtomicBool, owed: &AtomicBool) {
    let mut was_blocked = false;
    while !pty.exited() && !gone.load(Ordering::SeqCst) {
        let now = blocked(pty, sink).is_some();
        if !now && (was_blocked || owed.swap(false, Ordering::SeqCst)) {
            sink.emit(DriverEvent::DialogClosed);
        }
        was_blocked = now;
        std::thread::sleep(timing.monitor);
    }
}

impl Drop for ClaudeDriver {
    fn drop(&mut self) {
        self.tail_gen.fetch_add(1, Ordering::SeqCst);
        self.gone.store(true, Ordering::SeqCst);
        // Also after an exit: what the agent left behind goes too.
        self.pty.kill_tree(Duration::from_millis(200));
    }
}

impl Driver for ClaudeDriver {
    fn send(&self, text: &str) -> Result<(), DriverError> {
        if !self.ready.load(Ordering::SeqCst) {
            return Err(DriverError::NotNow("Claude is not ready yet (starting, or at the trust prompt)".into()));
        }
        if let Some(why) = blocked(&self.pty, &self.sink) {
            return Err(DriverError::NotNow(why.into()));
        }
        let bytes = paste::typed_message(text).map_err(DriverError::Failed)?;
        self.type_(Typing::Message(text.to_string(), bytes))
    }

    fn interrupt(&self) -> Result<(), DriverError> {
        // Esc answers a dialog ("Esc to cancel"): never typed while one is pending or open,
        // checked now and again by the typist right before the key.
        if blocked(&self.pty, &self.sink).is_some() {
            return Err(DriverError::NotNow(INTERRUPT_REFUSED.into()));
        }
        self.type_answer(|deadline, reply| Typing::Interrupt { deadline, reply })
    }

    fn answer_permission(
        &self,
        detail: &str,
        tool: &str,
        description: Option<&str>,
        allow: bool,
        guard: Option<AnswerGuard>,
    ) -> Result<(), DriverError> {
        // Screen text can never prove which request a dialog is: without the hub's evidence
        // nothing is typed.
        let guard = guard.ok_or_else(|| {
            DriverError::NoDialog("no evidence that the dialog is this request's; answer it in `mirante attach`".into())
        })?;
        let label = if allow { "Yes" } else { "No" };
        // Checked now for a quick answer, and again by the typist right before the key.
        answer_key(&self.pty.grid(), tool, detail, description, label, &self.cwd)?;
        let (detail, tool, description) = (detail.to_string(), tool.to_string(), description.map(str::to_string));
        self.type_answer(|deadline, reply| Typing::Answer { detail, tool, description, label, guard, deadline, reply })
    }

    fn trust(&self, accept: bool) -> Result<(), DriverError> {
        if self.ready.load(Ordering::SeqCst) {
            return Err(DriverError::NoDialog("the session is past its trust prompt".into()));
        }
        trust_key(&self.pty.grid())?;
        if !accept {
            let (pty, grace) = (self.pty.clone(), self.timing.stop_grace);
            std::thread::spawn(move || pty.kill_tree(grace));
            return Ok(());
        }
        self.type_answer(|deadline, reply| Typing::Trust { deadline, reply })
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
        self.gone.store(true, Ordering::SeqCst);
        // Also after an exit: what the agent left behind (see `PtyProcess::kill_tree`).
        self.pty.kill_tree(self.timing.stop_grace);
        self.pty.wait_exit(Duration::from_secs(2));
    }

    fn session_started(&self) {
        self.started.store(true, Ordering::SeqCst);
    }

    fn root(&self) -> Option<Proc> {
        self.pty.root()
    }

    fn tty(&self) -> Option<u64> {
        self.pty.tty()
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
            monitor: Duration::from_millis(20),
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

    /// A live permission dialog: the box border (the widest row, at column 0), title, command.
    const DIALOG: &[u8] = b"\x1b[2J\x1b[H\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\r\n Bash command\r\n   touch x\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n   2. No\r\n Esc to cancel\r\n";

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

    /// The real 2.1.285 dialog has no digits and starts on "No, exit": accepting it is Down,
    /// then (once the screen shows the cursor on "Yes, I trust this folder") Enter.
    #[test]
    fn the_real_unnumbered_trust_prompt_is_accepted_with_down_then_enter() {
        let f = fake(
            "printf ' \\342\\235\\257 No, exit\\r\\n   Yes, I trust this folder\\r\\n\\r\\n Enter to confirm \\302\\267 Esc to cancel\\r\\n'; \
             a=$(dd bs=1 count=3 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
             printf '\\033[2J\\033[H   No, exit\\r\\n \\342\\235\\257 Yes, I trust this folder\\r\\n\\r\\n Enter to confirm \\302\\267 Esc to cancel\\r\\n'; \
             b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
             printf '\\033[2J\\033[Hkeys:%s,%s\\r\\n> \\r\\n  manual mode on\\r\\n' \"$a\" \"$b\"; sleep 5",
        );
        assert_eq!(next(&f.rx, |e| matches!(e, DriverEvent::Boot(_))), DriverEvent::Boot(BootState::TrustPrompt));
        assert!(f.d.dialog_open());
        assert!(matches!(f.d.send("hi"), Err(DriverError::NotNow(_))));
        f.d.trust(true).unwrap();
        wait_grid(&f.d, "keys:1b5b42,0d");
        ready(&f);
    }

    /// Task 21 live run: right after the dialog shows, Claude may not take keys yet and the
    /// first Down is lost. Down is typed again while the cursor still sits on "No, exit";
    /// Enter still only once the screen shows it on "Yes".
    #[test]
    fn a_lost_down_is_typed_again_and_enter_waits_for_yes() {
        let f = fake(
            "printf ' \\342\\235\\257 No, exit\\r\\n   Yes, I trust this folder\\r\\n\\r\\n Enter to confirm \\302\\267 Esc to cancel\\r\\n'; \
             dd bs=1 count=3 >/dev/null 2>&1; \
             a=$(dd bs=1 count=3 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
             printf '\\033[2J\\033[H   No, exit\\r\\n \\342\\235\\257 Yes, I trust this folder\\r\\n\\r\\n Enter to confirm \\302\\267 Esc to cancel\\r\\n'; \
             b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
             printf '\\033[2J\\033[Hkeys:%s,%s\\r\\n> \\r\\n  manual mode on\\r\\n' \"$a\" \"$b\"; sleep 5",
        );
        next(&f.rx, |e| *e == DriverEvent::Boot(BootState::TrustPrompt));
        f.d.trust(true).unwrap();
        wait_grid(&f.d, "keys:1b5b42,0d");
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
    fn a_labelled_message_is_typed_whole_as_one_paste() {
        // The operator label is part of the paste: one line, the person's text after it.
        let dir = TestDir::new();
        let out = dir.path().join("typed");
        let msg = mirante_core::delivery::label_operator("use tabs");
        let want = [paste::bracketed(&msg), b"\r".to_vec()].concat();
        assert_eq!(paste::line_count(&msg), 1);
        let f = fake(&format!(
            "{READY}; dd bs=1 count={} of={} 2>/dev/null; printf 'done'; sleep 5",
            want.len(),
            out.display()
        ));
        ready(&f);
        f.d.send(&msg).unwrap();
        wait_grid(&f.d, "done");
        let got = std::fs::read(&out).unwrap();
        assert_eq!(got, want);
        assert!(String::from_utf8_lossy(&got).contains("[Mirante · message from the human operator] use tabs"));
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

    /// Task 21 live run (2.1.285): with a 3-line paste in the input box, Ctrl-E then each
    /// Ctrl-U either clears a line's text or joins it to the line above, so 3 lines need 5
    /// Ctrl-U (2n - 1); one more on the empty box is harmless. A large paste shown as
    /// "[Pasted text #1 +39 lines]" goes with one Ctrl-U.
    #[test]
    fn clearing_an_aborted_paste_sends_two_ctrl_u_per_line() {
        assert_eq!(clear_input(1), b"\x05\x15\x15".to_vec());
        assert_eq!(clear_input(3), b"\x05\x15\x15\x15\x15\x15\x15".to_vec());
        assert!(clear_input(3).iter().all(|b| *b == 0x05 || *b == 0x15), "never Enter, Esc or a digit");
    }

    #[test]
    fn a_requeued_message_is_not_duplicated_the_line_is_cleared_before_the_next_paste() {
        let dir = TestDir::new();
        let out = dir.path().join("typed");
        let pasted = paste::bracketed("go on");
        // Ctrl-E, then two Ctrl-U per line of the aborted paste; then the paste again.
        let want = [b"\x05\x15\x15".to_vec(), pasted.clone(), b"\r".to_vec()].concat();
        let f = fake_with(
            &format!(
                "{READY}; dd bs=1 count={} >/dev/null 2>&1; printf 'pasted'; stty min 0 time 30; \
                 dd bs=1 count={} of={} 2>/dev/null; printf ' done'; sleep 5",
                pasted.len(),
                want.len(),
                out.display()
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
        // Nothing is typed while held (not even Ctrl-U: a dialog may be about to take keys).
        let blocked = f.d.send("more").unwrap_err();
        assert!(matches!(blocked, DriverError::NotNow(ref m) if m.starts_with("hold")), "{blocked:?}");
        std::thread::sleep(Duration::from_millis(300));
        f.hold.store(false, Ordering::SeqCst);
        // The hub hands the requeued message back: the leftover is cleared first.
        f.d.send("go on").unwrap();
        wait_grid(&f.d, " done");
        assert_eq!(std::fs::read(&out).unwrap(), want, "{:?}", String::from_utf8_lossy(&std::fs::read(&out).unwrap()));
    }

    #[test]
    fn a_blocked_send_is_not_now_and_says_why() {
        let f = fake(&format!("{READY}; sleep 5"));
        ready(&f);
        f.d.pty.mirror(DIALOG);
        let e = f.d.send("x").unwrap_err();
        assert!(matches!(e, DriverError::NotNow(ref m) if m.starts_with("dialog")), "{e:?}");
    }

    #[test]
    fn an_answer_whose_dialog_changed_before_it_was_typed_types_nothing() {
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
        // The typist is busy (between the paste and Enter) when the answer is asked for.
        f.d.send("go on").unwrap();
        wait_grid(&f.d, "pasted");
        f.d.pty.mirror(DIALOG);
        let d = f.d.clone();
        let answer = std::thread::spawn(move || d.answer_permission("touch x", "Bash", None, true, Some(pass())));
        std::thread::sleep(Duration::from_millis(150));
        // Before the typist gets to it, another dialog replaced the one that was asked about.
        f.d.pty.mirror(b"\x1b[2J\x1b[H\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\r\n Bash command\r\n   rm -rf ~\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n   2. No\r\n Esc to cancel\r\n");
        let r = answer.join().unwrap();
        assert!(matches!(r, Err(DriverError::NoDialog(_))), "{r:?}");
        next(&f.rx, |e| matches!(e, DriverEvent::Step(v) if v["type"] == "answer_aborted"));
        wait_grid(&f.d, "next:[]");
    }

    #[test]
    fn interrupt_is_refused_while_a_permission_is_pending_or_a_dialog_is_open() {
        let dir = TestDir::new();
        let out = dir.path().join("typed");
        let f = fake(&format!(
            "{READY}; stty min 0 time 15; dd bs=1 count=1 of={} 2>/dev/null; printf 'read'; sleep 5",
            out.display()
        ));
        ready(&f);
        f.hold.store(true, Ordering::SeqCst);
        let e = f.d.interrupt().unwrap_err();
        assert!(matches!(e, DriverError::NotNow(ref m) if m.starts_with("dialog open")), "{e:?}");
        f.hold.store(false, Ordering::SeqCst);
        f.d.pty.mirror(DIALOG);
        assert!(matches!(f.d.interrupt(), Err(DriverError::NotNow(_))));
        wait_grid(&f.d, "read");
        assert_eq!(std::fs::read(&out).unwrap_or_default(), b"", "no Esc reached the agent");
        f.d.pty.mirror(b"\x1b[2J\x1b[H> \r\n  manual mode on\r\n");
        f.d.interrupt().unwrap();
    }

    /// Ported from the driver review's probe (`probe_answer_latency_behind_steers`): with
    /// five messages handed to the typist, an answer waited out every message's pauses.
    #[test]
    fn probe_an_answer_does_not_wait_behind_queued_steers() {
        let f = fake_with(
            &format!("{READY}; sleep 8"),
            ClaudeTiming {
                tick: Duration::from_millis(20),
                monitor: Duration::from_millis(20),
                ..ClaudeTiming::default()
            },
            false,
        );
        ready(&f);
        for m in ["one", "two", "three", "four", "five"] {
            f.d.send(m).unwrap();
        }
        std::thread::sleep(Duration::from_millis(200));
        f.d.pty.mirror(DIALOG);
        f.hold.store(true, Ordering::SeqCst);
        let t0 = Instant::now();
        let r = f.d.answer_permission("touch x", "Bash", None, true, Some(pass()));
        let took = t0.elapsed();
        assert!(r.is_ok(), "{r:?}");
        assert!(took < Duration::from_millis(300), "the answer waited {took:?} behind the steers");
        // Every message not typed was handed back.
        let mut aborted = 0;
        while let Ok((_, e)) = f.rx.recv_timeout(Duration::from_millis(200)) {
            if matches!(e, DriverEvent::DeliveryAborted { .. }) {
                aborted += 1;
            }
        }
        assert_eq!(aborted, 5);
    }

    fn pass() -> AnswerGuard {
        Arc::new(|| Ok(()))
    }

    /// Ported from the second driver review's probe
    /// (`review_probe_esc_queued_behind_a_message_lands_in_a_dialog`): an Esc accepted by
    /// `interrupt` while the typist was busy with a message was typed later with no re-check,
    /// after a dialog (hold + screen) appeared. Now the interrupt waits for the typist, which
    /// checks again right before the key and refuses (`NotNow`): nothing reaches the agent.
    #[test]
    fn probe_an_esc_queued_behind_a_message_is_never_typed_into_a_dialog_that_opened_meanwhile() {
        let dir = TestDir::new();
        let out = dir.path().join("typed");
        let pasted = paste::bracketed("go on");
        let f = fake_with(
            &format!(
                "{READY}; dd bs=1 count={} >/dev/null 2>&1; printf 'pasted'; stty min 0 time 20; \
                 dd bs=1 count=1 of={} 2>/dev/null; printf ' read'; sleep 5",
                pasted.len(),
                out.display()
            ),
            ClaudeTiming { type_pause: Duration::from_millis(800), ..fast() },
            false,
        );
        ready(&f);
        f.d.send("go on").unwrap();
        wait_grid(&f.d, "pasted");
        // Nothing blocks yet: the interrupt is accepted and waits behind the message.
        let d = f.d.clone();
        let esc = std::thread::spawn(move || d.interrupt());
        std::thread::sleep(Duration::from_millis(100));
        // The PermissionRequest hook arrives and the dialog is drawn.
        f.hold.store(true, Ordering::SeqCst);
        f.d.pty.mirror(DIALOG);
        let r = esc.join().unwrap();
        assert!(matches!(r, Err(DriverError::NotNow(ref m)) if m.starts_with("dialog open")), "{r:?}");
        wait_grid(&f.d, " read");
        assert_eq!(std::fs::read(&out).unwrap_or_default(), b"", "no Esc reached the dialog");
    }

    #[test]
    fn an_interrupt_with_nothing_in_the_way_types_esc_and_returns_once_typed() {
        let dir = TestDir::new();
        let out = dir.path().join("typed");
        let f = fake(&format!(
            "{READY}; stty min 0 time 20; dd bs=1 count=1 of={} 2>/dev/null; printf 'read'; sleep 5",
            out.display()
        ));
        ready(&f);
        f.d.interrupt().unwrap();
        wait_grid(&f.d, "read");
        assert_eq!(std::fs::read(&out).unwrap(), b"\x1b");
    }

    #[test]
    fn an_answer_whose_evidence_changed_before_the_key_types_nothing() {
        let f = fake(&format!(
            "{READY}; stty min 0 time 15; k=$(dd bs=1 count=1 2>/dev/null); printf 'got:[%s]' \"$k\"; sleep 5"
        ));
        ready(&f);
        f.d.pty.mirror(DIALOG);
        f.hold.store(true, Ordering::SeqCst);
        // The dialog on screen matches, but the hub's evidence no longer holds.
        let stale: AnswerGuard = Arc::new(|| Err("a newer tool call has no card".into()));
        let r = f.d.answer_permission("touch x", "Bash", None, true, Some(stale));
        assert!(matches!(r, Err(DriverError::NoDialog(ref m)) if m.contains("newer tool call")), "{r:?}");
        // No guard at all: Claude answers are never typed without the hub's evidence.
        let r = f.d.answer_permission("touch x", "Bash", None, true, None);
        assert!(matches!(r, Err(DriverError::NoDialog(_))), "{r:?}");
        wait_grid(&f.d, "got:[]");
    }

    #[test]
    fn the_guard_is_called_after_the_screen_check_right_before_the_key() {
        let f = fake(&format!("{READY}; k=$(dd bs=1 count=1 2>/dev/null); printf 'got:[%s]' \"$k\"; sleep 5"));
        ready(&f);
        f.d.pty.mirror(DIALOG);
        f.hold.store(true, Ordering::SeqCst);
        let calls = Arc::new(AtomicU64::new(0));
        let c = calls.clone();
        let g: AnswerGuard = Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        f.d.answer_permission("touch x", "Bash", None, true, Some(g)).unwrap();
        wait_grid(&f.d, "got:[1]");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "checked once, by the typist");
    }

    #[test]
    fn a_closed_dialog_or_a_released_hold_is_reported() {
        let f = fake(&format!("{READY}; sleep 5"));
        ready(&f);
        f.d.pty.mirror(DIALOG);
        std::thread::sleep(Duration::from_millis(100));
        f.d.pty.mirror(b"\x1b[2J\x1b[H> \r\n  manual mode on\r\n");
        next(&f.rx, |e| *e == DriverEvent::DialogClosed);
        f.hold.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        f.hold.store(false, Ordering::SeqCst);
        next(&f.rx, |e| *e == DriverEvent::DialogClosed);
    }

    #[test]
    fn boot_keeps_watching_after_the_limit_and_says_what_it_waits_for() {
        let f = fake_with(
            &format!("{READY}; sleep 10"),
            ClaudeTiming { boot_limit: Duration::from_millis(200), ..fast() },
            false,
        );
        let ev = next(&f.rx, |e| matches!(e, DriverEvent::Step(v) if v["type"] == "boot_waiting"));
        let DriverEvent::Step(v) = ev else { unreachable!() };
        assert!(v["waiting_for"].to_string().contains("SessionStart"), "{v}");
        // Readiness still arrives after the limit.
        ready(&f);
    }

    #[test]
    fn a_resumed_session_that_started_and_then_exited_did_not_fail_to_resume() {
        let f = fake_with("sleep 0.6; exit 3", fast(), true);
        f.d.session_started();
        let ev = next(&f.rx, |e| matches!(e, DriverEvent::ResumeFailed { .. } | DriverEvent::Exited { .. }));
        assert_eq!(ev, DriverEvent::Exited { code: Some(3) });
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
