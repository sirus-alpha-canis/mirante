//! Test double: records every call, lets the test emit events through the captured sink.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use super::proctree::Proc;
use super::{AnswerGuard, Attachment, Driver, DriverError, DriverEvent, DriverFactory, Sink, SpawnRequest};

#[derive(Default)]
pub struct FakeDriver {
    pub calls: Mutex<Vec<String>>,
    pub dialog: AtomicBool,
    pub fail_answers: AtomicBool,
    /// The attached client's output channel, kept open until `detach` (like a live agent).
    pub attached: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    /// What `root()` reports (memory sampling tests set it).
    pub root: Mutex<Option<Proc>>,
    /// What `tty()` reports (identity tests set it).
    pub tty: Mutex<Option<u64>>,
    /// When set, `send` fails with this error (after recording the call).
    pub send_error: Mutex<Option<DriverError>>,
    /// When set, the next `answer_permission` signals the first channel, then waits on the
    /// second before answering (a test acts on the hub meanwhile).
    #[allow(clippy::type_complexity)]
    pub answer_gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    /// Like `answer_gate`, but before the guard is checked (the time between the hub's
    /// checks and the typist's last check).
    #[allow(clippy::type_complexity)]
    pub guard_gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    /// Runs inside `stop` (a test applies the process's exit before `stop` returns, the
    /// way the pump thread can when the agent dies quickly).
    #[allow(clippy::type_complexity)]
    pub on_stop: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl FakeDriver {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Driver for FakeDriver {
    fn send(&self, text: &str) -> Result<(), DriverError> {
        self.record(format!("send:{text}"));
        match self.send_error.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn interrupt(&self) -> Result<(), DriverError> {
        self.record("interrupt".into());
        Ok(())
    }
    fn answer_permission(
        &self,
        key: &str,
        tool: &str,
        description: Option<&str>,
        allow: bool,
        guard: Option<AnswerGuard>,
    ) -> Result<(), DriverError> {
        if let Some((entered, go)) = self.guard_gate.lock().unwrap().take() {
            let _ = entered.send(());
            let _ = go.recv();
        }
        if self.fail_answers.load(Ordering::SeqCst) {
            return Err(DriverError::NoDialog("fake".into()));
        }
        // Like the Claude typist: the guard is the last check before the key.
        if let Some(why) = guard.as_ref().and_then(|g| g().err()) {
            self.record(format!("guard_refused:{why}"));
            return Err(DriverError::NoDialog(why));
        }
        // The key is typed; the driver has not returned yet.
        if let Some((entered, go)) = self.answer_gate.lock().unwrap().take() {
            let _ = entered.send(());
            let _ = go.recv();
        }
        self.record(format!("answer:{key}:{tool}:{allow}"));
        if let Some(d) = description {
            self.record(format!("description:{d}"));
        }
        Ok(())
    }
    fn trust(&self, accept: bool) -> Result<(), DriverError> {
        self.record(format!("trust:{accept}"));
        Ok(())
    }
    fn dialog_open(&self) -> bool {
        self.dialog.load(Ordering::SeqCst)
    }
    fn tail_transcript(&self, path: &Path, offset: u64) {
        self.record(format!("tail:{}:{offset}", path.display()));
    }
    fn attach(&self, rows: u16, cols: u16) -> Result<Attachment, DriverError> {
        self.record(format!("attach:{rows}x{cols}"));
        let (tx, rx) = mpsc::channel();
        tx.send(b"live".to_vec()).unwrap();
        *self.attached.lock().unwrap() = Some(tx);
        Ok(Attachment { id: 1, initial: b"screen".to_vec(), output: rx, input: true })
    }
    fn input(&self, _attachment: u64, bytes: &[u8]) -> Result<(), DriverError> {
        self.record(format!("input:{}", String::from_utf8_lossy(bytes)));
        Ok(())
    }
    fn detach(&self, attachment: u64) {
        self.record(format!("detach:{attachment}"));
        self.attached.lock().unwrap().take();
    }
    fn resize(&self, rows: u16, cols: u16) -> Result<(), DriverError> {
        self.record(format!("resize:{rows}x{cols}"));
        Ok(())
    }
    fn screen_tail(&self) -> String {
        "fake screen".into()
    }
    fn stop(&self) {
        self.record("stop".into());
        if let Some(f) = self.on_stop.lock().unwrap().as_ref() {
            f();
        }
    }
    fn session_started(&self) {
        self.record("session_started".into());
    }
    fn root(&self) -> Option<Proc> {
        *self.root.lock().unwrap()
    }
    fn tty(&self) -> Option<u64> {
        *self.tty.lock().unwrap()
    }
}

/// Hands out one shared `FakeDriver` and keeps every sink and spawn request.
#[derive(Default)]
pub struct FakeFactory {
    pub driver: Arc<FakeDriver>,
    pub sinks: Mutex<Vec<Sink>>,
    pub resumes: Mutex<Vec<Option<String>>>,
    pub fail: AtomicBool,
    /// When set, `spawn` fails with this error (e.g. `NativeGone`).
    pub fail_with: Mutex<Option<DriverError>>,
    /// When set, the next `spawn` reports on the first channel that it started, then blocks
    /// until the second one yields (or closes).
    pub spawn_gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    /// When set, `spawn` emits this event through the sink, then fails.
    pub emit_then_fail: Mutex<Option<DriverEvent>>,
    /// What `memory_server` answers (`None`: not found).
    pub mcp_bin: Mutex<Option<std::path::PathBuf>>,
    /// Every spawn request's `mcp`, in order.
    pub mcps: Mutex<Vec<Option<std::path::PathBuf>>>,
    /// Every spawn request's `skills`, in order.
    pub skills: Mutex<Vec<Option<super::InjectedSkills>>>,
}

impl FakeFactory {
    pub fn last_sink(&self) -> Sink {
        self.sinks.lock().unwrap().last().cloned().expect("spawned")
    }

    pub fn emit(&self, event: DriverEvent) {
        self.last_sink().emit(event);
    }
}

impl DriverFactory for Arc<FakeFactory> {
    fn spawn(&self, req: SpawnRequest, sink: Sink) -> Result<Arc<dyn Driver>, DriverError> {
        if let Some((entered, release)) = self.spawn_gate.lock().unwrap().take() {
            let _ = entered.send(());
            let _ = release.recv();
        }
        self.resumes.lock().unwrap().push(req.resume.clone());
        self.mcps.lock().unwrap().push(req.mcp.clone());
        self.skills.lock().unwrap().push(req.skills.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(DriverError::Failed("spawn refused".into()));
        }
        if let Some(e) = self.fail_with.lock().unwrap().clone() {
            return Err(e);
        }
        if let Some(ev) = self.emit_then_fail.lock().unwrap().clone() {
            sink.emit(ev);
            return Err(DriverError::Failed("spawn failed after emitting".into()));
        }
        self.sinks.lock().unwrap().push(sink);
        Ok(self.driver.clone())
    }

    fn memory_server(&self) -> Result<std::path::PathBuf, String> {
        self.mcp_bin.lock().unwrap().clone().ok_or_else(|| "mirante-mcp not found (fake)".to_string())
    }
}
