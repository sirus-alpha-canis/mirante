//! The Claude Code hook command. Claude runs it for every hook event with the payload on
//! stdin. It forwards the payload to the daemon synchronously (so the daemon sees events
//! in the order Claude fires them) and, when the daemon is unreachable, appends it to the
//! spool file that the daemon ingests on its next start. It prints nothing (hook stdout
//! can become context for Claude) and never fails or blocks the hook: delivery has an
//! overall deadline, a circuit breaker skips the socket after a timeout, stdin is read
//! with a size cap and a timeout, and the spool is size-capped.
//!
//! Every forwarded/spooled message is a [`HookEnvelope`]: `HookParams` plus `event_id` and
//! `received_ts_ms`. **The daemon's `hook.event` handling and spool ingest must dedupe by
//! `event_id`** (a timed-out forward is also spooled, so it can arrive twice), skip lines
//! that do not parse, and remember that spool replays can be older than live events.
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use mirante_ipc::rpc::{code, MAX_LINE};
use mirante_ipc::types::{method, HookEnvelope, HookParams};
use mirante_ipc::{Client, ClientError, Paths};
use serde_json::Value;

/// Overall deadline for forwarding (connect + write + read the ack), then spool.
pub const TIMEOUT: Duration = Duration::from_millis(800);
/// After a forward timed out, hooks spool directly for this long.
pub const BREAKER_WINDOW: Duration = Duration::from_secs(5);
/// How long to wait for Claude to deliver stdin.
pub const STDIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Stdin bytes kept; the rest is dropped and the payload flagged truncated.
pub const STDIN_CAP: usize = MAX_LINE / 2;
/// The spool stops growing here; further events are lost (with one stderr line).
pub const SPOOL_CAP: u64 = 64 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// No `MIRANTE_TASK_ID`: not a Mirante session, do nothing.
    Skipped,
    Forwarded,
    Spooled,
    /// Neither the daemon nor the spool file could be reached. One line goes to stderr.
    Lost,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn new_event_id(ts_ms: i64) -> String {
    format!("{ts_ms}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Reads up to `cap` bytes, lossy-decodes, and reports whether more input was dropped.
/// Invalid UTF-8 becomes U+FFFD: never dropped.
pub fn read_capped(r: impl Read, cap: usize) -> io::Result<(String, bool)> {
    let mut buf = Vec::new();
    r.take(cap as u64 + 1).read_to_end(&mut buf)?;
    let truncated = buf.len() > cap;
    buf.truncate(cap);
    Ok((String::from_utf8_lossy(&buf).into_owned(), truncated))
}

/// [`read_capped`] on stdin in a helper thread, giving up after `timeout` (whatever was
/// read so far is lost, the empty payload is flagged truncated so it is still reported).
pub fn read_stdin(timeout: Duration, cap: usize) -> (String, bool) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_capped(io::stdin().lock(), cap));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(v)) => v,
        _ => (String::new(), true),
    }
}

pub fn deliver(paths: &Paths, task: Option<&str>, payload: &str, ts_ms: i64) -> Outcome {
    deliver_with(paths, task, payload, ts_ms, false)
}

/// `truncated`: the payload was cut at [`STDIN_CAP`]; it is replaced by a small valid-JSON
/// stub `{"truncated":true,...}` and the envelope is flagged.
pub fn deliver_with(paths: &Paths, task: Option<&str>, payload: &str, ts_ms: i64, truncated: bool) -> Outcome {
    deliver_session(paths, task, None, payload, ts_ms, truncated)
}

/// [`deliver_with`] plus the Mirante session id (`MIRANTE_SESSION_ID`), carried in the
/// envelope's `session` field when present and non-empty.
pub fn deliver_session(
    paths: &Paths,
    task: Option<&str>,
    session: Option<&str>,
    payload: &str,
    ts_ms: i64,
    truncated: bool,
) -> Outcome {
    deliver_inner(paths, task, session, payload, ts_ms, truncated, TIMEOUT)
}

fn stub(original_bytes: usize) -> String {
    format!(r#"{{"truncated":true,"original_bytes":{original_bytes}}}"#)
}

fn envelope(task: &str, session: Option<&str>, payload: &str, ts_ms: i64, truncated: bool) -> HookEnvelope {
    let session = session.filter(|s| !s.is_empty()).map(String::from);
    let build = |payload: String, truncated: bool, event_id: String| HookEnvelope {
        params: HookParams { task: task.to_string(), payload, ts_ms },
        event_id: Some(event_id),
        received_ts_ms: Some(ts_ms),
        truncated: truncated.then_some(true),
        session: session.clone(),
    };
    let id = new_event_id(ts_ms);
    let first = if truncated {
        build(stub(payload.len()), true, id.clone())
    } else {
        build(payload.to_string(), false, id.clone())
    };
    // JSON escaping can inflate a payload past the line limit the daemon enforces.
    if serde_json::to_vec(&first).map(|v| v.len() >= MAX_LINE - 1024).unwrap_or(true) {
        return build(stub(payload.len()), true, id);
    }
    first
}

fn deliver_inner(
    paths: &Paths,
    task: Option<&str>,
    session: Option<&str>,
    payload: &str,
    ts_ms: i64,
    truncated: bool,
    deadline: Duration,
) -> Outcome {
    let Some(task) = task.filter(|t| !t.is_empty()) else {
        return Outcome::Skipped;
    };
    let env = envelope(task, session, payload, ts_ms, truncated);
    if !breaker_open(paths) {
        match forward(paths, &env, deadline) {
            // At its connection limit the daemon did not take the payload: spool it.
            Some(Err(ClientError::Rpc(e))) if e.code == code::BUSY => {}
            // The daemon got the payload; an RPC error (e.g. unparseable) is logged there.
            Some(Ok(())) | Some(Err(ClientError::Rpc(_))) => return Outcome::Forwarded,
            // A socket timeout and the overall deadline are the same failure (a daemon that
            // accepts but does not answer): either one trips the breaker.
            Some(Err(ClientError::Io(e))) if is_timeout(&e) => trip_breaker(paths),
            Some(Err(_)) => {}
            None => trip_breaker(paths),
        }
    }
    match spool(paths, &env) {
        Ok(()) => Outcome::Spooled,
        Err(e) => {
            eprintln!("mirante-hook: event lost: {e}");
            Outcome::Lost
        }
    }
}

fn breaker_open(paths: &Paths) -> bool {
    std::fs::metadata(paths.hook_breaker())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < BREAKER_WINDOW)
}

fn trip_breaker(paths: &Paths) {
    if paths.ensure_home().is_ok() {
        let _ = std::fs::write(paths.hook_breaker(), b"");
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
}

/// Socket read/write timeout for a forward: strictly shorter than the overall deadline, so
/// a stalled daemon usually surfaces as a socket timeout before the deadline fires.
fn socket_timeout(deadline: Duration) -> Duration {
    (deadline * 3 / 4).max(Duration::from_millis(1))
}

/// `None` = the deadline passed (the worker thread is abandoned; the process is about to
/// exit anyway).
fn forward(paths: &Paths, env: &HookEnvelope, deadline: Duration) -> Option<Result<(), ClientError>> {
    let (tx, rx) = mpsc::channel();
    let socket = paths.socket();
    let env = env.clone();
    std::thread::spawn(move || {
        let r = (|| {
            let mut client = Client::connect(&socket)?;
            client.set_timeout(Some(socket_timeout(deadline)))?;
            client.call::<Value>(method::HOOK_EVENT, &env)?;
            Ok(())
        })();
        let _ = tx.send(r);
    });
    rx.recv_timeout(deadline).ok()
}

/// Appends one JSON line (mode 0600, tightened if the file was looser). One `write_all`
/// per line: concurrent hook processes appending with O_APPEND cannot interleave
/// mid-line. Refuses (error) once the file is at [`SPOOL_CAP`].
pub fn spool(paths: &Paths, env: &HookEnvelope) -> io::Result<()> {
    spool_capped(paths, env, SPOOL_CAP)
}

fn spool_capped(paths: &Paths, env: &HookEnvelope, cap: u64) -> io::Result<()> {
    paths.ensure_home()?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(paths.spool())?;
    let m = f.metadata()?;
    if m.mode() & 0o077 != 0 {
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    if m.len() >= cap {
        return Err(io::Error::other(format!("spool is full ({} bytes)", m.len())));
    }
    let mut line = serde_json::to_vec(env)?;
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

    fn lines(paths: &Paths) -> Vec<HookEnvelope> {
        std::fs::read_to_string(paths.spool()).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn events_carry_unique_ids_and_received_ts_and_old_readers_still_parse() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        deliver(&paths, Some("t"), "{}", 5);
        deliver(&paths, Some("t"), "{}", 5);
        let v = lines(&paths);
        assert_ne!(v[0].event_id, v[1].event_id);
        assert!(v[0].event_id.as_ref().unwrap().starts_with("5-"));
        assert_eq!(v[0].received_ts_ms, Some(5));
        assert_eq!(v[0].truncated, None);
        let text = std::fs::read_to_string(paths.spool()).unwrap();
        let old: HookParams = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(old.task, "t");
        // A pre-envelope spool line has no optional fields.
        let legacy: HookEnvelope = serde_json::from_str(r#"{"task":"t","payload":"{}","ts_ms":1}"#).unwrap();
        assert_eq!(legacy.event_id, None);
    }

    #[test]
    fn the_mirante_session_id_rides_along_when_set() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        deliver_session(&paths, Some("t"), Some("sess-1"), "{}", 1, false);
        deliver_session(&paths, Some("t"), Some(""), "{}", 2, false);
        deliver(&paths, Some("t"), "{}", 3);
        let v = lines(&paths);
        assert_eq!(v[0].session.as_deref(), Some("sess-1"));
        assert_eq!((v[1].session.as_deref(), v[2].session.as_deref()), (None, None));
    }

    #[test]
    fn a_busy_daemon_did_not_take_the_event_so_it_is_spooled() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let listener = UnixListener::bind(paths.socket()).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = read_line(&mut BufReader::new(stream.try_clone().unwrap()));
            let mut w = stream;
            let e = mirante_ipc::RpcError::new(code::BUSY, "busy");
            write_line(&mut w, &Response::err(None, e)).unwrap();
        });
        assert_eq!(deliver(&paths, Some("t"), "{}", 1), Outcome::Spooled);
        server.join().unwrap();
        assert_eq!(lines(&paths).len(), 1);
        assert!(!paths.hook_breaker().exists());
    }

    #[test]
    fn a_daemon_that_never_answers_cannot_stall_the_hook_and_trips_the_breaker() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let listener = UnixListener::bind(paths.socket()).unwrap();
        let _hold = std::thread::spawn(move || {
            let c = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(3));
            drop(c);
        });
        let t = std::time::Instant::now();
        let out = deliver_inner(&paths, Some("t"), None, "{}", 1, false, Duration::from_millis(150));
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert_eq!(out, Outcome::Spooled);
        assert!(paths.hook_breaker().exists());
        assert_eq!(lines(&paths).len(), 1);
    }

    #[test]
    fn a_fresh_breaker_spools_without_touching_the_socket_and_a_stale_one_does_not() {
        let d = TestDir::new();
        let paths = Paths::new(d.path());
        let listener = UnixListener::bind(paths.socket()).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::fs::write(paths.hook_breaker(), b"").unwrap();
        assert_eq!(deliver(&paths, Some("t"), "{}", 1), Outcome::Spooled);
        assert!(listener.accept().is_err(), "socket must not be contacted");
        // Age the marker past the window.
        let old = SystemTime::now() - Duration::from_secs(60);
        std::fs::File::options().write(true).open(paths.hook_breaker()).unwrap().set_modified(old).unwrap();
        assert!(!breaker_open(&paths));
    }

    #[test]
    fn stdin_is_capped_lossy_and_flagged() {
        let (s, t) = read_capped(&b"abc"[..], 10).unwrap();
        assert_eq!((s.as_str(), t), ("abc", false));
        let (s, t) = read_capped(&b"abcdef"[..], 4).unwrap();
        assert_eq!((s.as_str(), t), ("abcd", true));
        let (s, t) = read_capped(&b"a\xff\xfeb"[..], 10).unwrap();
        assert_eq!((s.as_str(), t), ("a\u{fffd}\u{fffd}b", false));
    }

    #[test]
    fn truncated_input_becomes_a_valid_json_stub_flagged_on_the_envelope() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        assert_eq!(deliver_with(&paths, Some("t"), "{\"half\":", 1, true), Outcome::Spooled);
        let e = &lines(&paths)[0];
        assert_eq!(e.truncated, Some(true));
        let v: Value = serde_json::from_str(&e.params.payload).unwrap();
        assert_eq!(v["truncated"], true);
    }

    #[test]
    fn escaping_inflation_past_the_line_limit_falls_back_to_the_stub() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        // 3 MiB of control bytes: each escapes to 6 bytes, > MAX_LINE.
        let payload = "\u{1}".repeat(3 << 20);
        deliver(&paths, Some("t"), &payload, 1);
        let text = std::fs::read_to_string(paths.spool()).unwrap();
        assert!(text.len() < MAX_LINE);
        let e: HookEnvelope = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(e.truncated, Some(true));
    }

    #[test]
    fn a_full_spool_loses_the_event_instead_of_growing() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        let env = envelope("t", None, "{}", 1, false);
        spool_capped(&paths, &env, 100).unwrap();
        while std::fs::metadata(paths.spool()).unwrap().len() < 100 {
            spool_capped(&paths, &env, 100).unwrap();
        }
        let before = std::fs::metadata(paths.spool()).unwrap().len();
        assert!(spool_capped(&paths, &env, 100).is_err());
        assert_eq!(std::fs::metadata(paths.spool()).unwrap().len(), before);
    }

    #[test]
    fn a_loose_spool_file_is_tightened_to_0600() {
        let d = TestDir::new();
        let paths = Paths::new(&d.path().join("home"));
        paths.ensure_home().unwrap();
        std::fs::write(paths.spool(), b"").unwrap();
        std::fs::set_permissions(paths.spool(), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(deliver(&paths, Some("t"), "{}", 1), Outcome::Spooled);
        assert_eq!(std::fs::metadata(paths.spool()).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn unwritable_home_is_lost_not_a_panic() {
        let d = TestDir::new();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(deliver(&Paths::new(&link), Some("t"), "{}", 1), Outcome::Lost);
    }
}
