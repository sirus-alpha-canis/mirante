//! Test double: records every call, lets the test emit events through the captured sink.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use super::proctree::Proc;
use super::{Attachment, Driver, DriverError, DriverEvent, DriverFactory, Sink, SpawnRequest};

#[derive(Default)]
pub struct FakeDriver {
    pub calls: Mutex<Vec<String>>,
    pub dialog: AtomicBool,
    pub fail_answers: AtomicBool,
    /// The attached client's output channel, kept open until `detach` (like a live agent).
    pub attached: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    /// What `root()` reports (memory sampling tests set it).
    pub root: Mutex<Option<Proc>>,
    /// When set, `send` fails with this error (after recording the call).
    pub send_error: Mutex<Option<DriverError>>,
    /// When set, the next `answer_permission` signals the first channel, then waits on the
    /// second before answering (a test acts on the hub meanwhile).
    #[allow(clippy::type_complexity)]
    pub answer_gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
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
    ) -> Result<(), DriverError> {
        if let Some((entered, go)) = self.answer_gate.lock().unwrap().take() {
            let _ = entered.send(());
            let _ = go.recv();
        }
        if self.fail_answers.load(Ordering::SeqCst) {
            return Err(DriverError::NoDialog("fake".into()));
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
    }
    fn session_started(&self) {
        self.record("session_started".into());
    }
    fn root(&self) -> Option<Proc> {
        *self.root.lock().unwrap()
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
}
