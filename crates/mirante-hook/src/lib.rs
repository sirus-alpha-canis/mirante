//! The Claude Code hook command. Claude runs it for every hook event with the payload on
//! stdin. It forwards the payload to the daemon synchronously (so the daemon sees events
//! in the order Claude fires them) and, when the daemon is unreachable, appends it to the
//! spool file that the daemon ingests on its next start. It prints nothing (hook stdout
//! can become context for Claude) and never fails the hook.
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

use mirante_ipc::types::{method, HookParams};
use mirante_ipc::{Client, ClientError, Paths};
use serde_json::Value;

/// How long the hook waits for the daemon before spooling.
pub const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No `MIRANTE_TASK_ID`: not a Mirante session, do nothing.
    Skipped,
    Forwarded,
    Spooled,
    /// Neither the daemon nor the spool file could be reached.
    Lost,
}

pub fn deliver(paths: &Paths, task: Option<&str>, payload: &str, ts_ms: i64) -> Outcome {
    let Some(task) = task.filter(|t| !t.is_empty()) else {
        return Outcome::Skipped;
    };
    let params = HookParams { task: task.to_string(), payload: payload.to_string(), ts_ms };
    match forward(paths, &params) {
        // The daemon got the payload; an RPC error (e.g. unparseable) is logged there.
        Ok(()) | Err(ClientError::Rpc(_)) => Outcome::Forwarded,
        Err(_) => match spool(paths, &params) {
            Ok(()) => Outcome::Spooled,
            Err(_) => Outcome::Lost,
        },
    }
}

fn forward(paths: &Paths, params: &HookParams) -> Result<(), ClientError> {
    let mut client = Client::connect(&paths.socket())?;
    client.set_timeout(Some(TIMEOUT))?;
    client.call::<Value>(method::HOOK_EVENT, params)?;
    Ok(())
}

/// Appends one JSON line (mode 0600). One `write_all` per line: concurrent hook processes
/// appending with O_APPEND cannot interleave mid-line.
pub fn spool(paths: &Paths, params: &HookParams) -> io::Result<()> {
    paths.ensure_home()?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(paths.spool())?;
    let mut line = serde_json::to_vec(params)?;
    line.push(b'\n');
    f.write_all(&line)
}
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_ipc::rpc::{read_line, write_line, Request, Response};
    use mirante_ipc::test_dir::TestDir;
    use serde_json::json;
    use std::io::BufReader;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    #[test]
    fn without_a_task_id_nothing_happens() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        assert_eq!(deliver(&paths, None, "{}", 1), Outcome::Skipped);
        assert_eq!(deliver(&paths, Some(""), "{}", 1), Outcome::Skipped);
        assert!(!paths.spool().exists());
    }

    #[test]
    fn daemon_down_spools_one_private_line_per_event() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        assert_eq!(deliver(&paths, Some("t1"), r#"{"hook_event_name":"Stop"}"#, 5), Outcome::Spooled);
        assert_eq!(deliver(&paths, Some("t1"), r#"{"hook_event_name":"SessionEnd"}"#, 6), Outcome::Spooled);
        let text = std::fs::read_to_string(paths.spool()).unwrap();
        let lines: Vec<HookParams> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].task, "t1");
        assert_eq!(lines[1].ts_ms, 6);
        assert_eq!(std::fs::metadata(paths.spool()).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn daemon_up_receives_the_payload_verbatim() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let listener = UnixListener::bind(paths.socket()).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let req: Request = serde_json::from_str(&read_line(&mut r).unwrap().unwrap()).unwrap();
            let mut w = stream;
            write_line(&mut w, &Response::ok(req.id, json!({}))).unwrap();
            req
        });
        let payload = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#;
        assert_eq!(deliver(&paths, Some("t9"), payload, 42), Outcome::Forwarded);
        let req = server.join().unwrap();
        assert_eq!(req.method, method::HOOK_EVENT);
        let p: HookParams = serde_json::from_value(req.params).unwrap();
        assert_eq!((p.task.as_str(), p.payload.as_str(), p.ts_ms), ("t9", payload, 42));
        assert!(!paths.spool().exists());
    }
}
