//! Test double: records every call, lets the test emit events through the captured sink.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use super::{Attachment, Driver, DriverError, DriverEvent, DriverFactory, Sink, SpawnRequest};

#[derive(Default)]
pub struct FakeDriver {
    pub calls: Mutex<Vec<String>>,
    pub dialog: AtomicBool,
    pub fail_answers: AtomicBool,
    /// The attached client's output channel, kept open until `detach` (like a live agent).
    pub attached: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
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
        Ok(())
    }
    fn interrupt(&self) -> Result<(), DriverError> {
        self.record("interrupt".into());
        Ok(())
    }
    fn answer_permission(&self, key: &str, allow: bool) -> Result<(), DriverError> {
        if self.fail_answers.load(Ordering::SeqCst) {
            return Err(DriverError::NoDialog("fake".into()));
        }
        self.record(format!("answer:{key}:{allow}"));
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
    fn screen_tail(&self) -> String {
        "fake screen".into()
    }
    fn stop(&self) {
        self.record("stop".into());
    }
}

/// Hands out one shared `FakeDriver` and keeps every sink and spawn request.
#[derive(Default)]
pub struct FakeFactory {
    pub driver: Arc<FakeDriver>,
    pub sinks: Mutex<Vec<Sink>>,
    pub resumes: Mutex<Vec<Option<String>>>,
    pub fail: AtomicBool,
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
        if self.fail.load(Ordering::SeqCst) {
            return Err(DriverError::Failed("spawn refused".into()));
        }
        self.resumes.lock().unwrap().push(req.resume);
        self.sinks.lock().unwrap().push(sink);
        Ok(self.driver.clone())
    }
}
