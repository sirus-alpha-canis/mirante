//! End to end through the real binaries (`mirante`, `mirante-daemon`, `mirante-hook`) with
//! a fake `claude` (tests/fake-claude.sh): dispatch, status from hooks, steer, stop, resume
//! on the pinned native session id, permissions answered once, a daemon killed mid-turn, a
//! slow subscriber, a stale hook, hibernate/wake, archive. Everything lives in a temp dir.
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use mirante_ipc::test_dir::TestDir;
use mirante_ipc::types::{method, DaemonStatus, HookEnvelope, HookParams, Snapshot, SubscribeParams, TaskView};
use mirante_ipc::{Client, Paths};
use serde_json::{json, Value};

fn bin(name: &str) -> PathBuf {
    // target/debug/deps/e2e-<hash> -> target/debug/<name>
    let p = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().join(name);
    assert!(p.exists(), "{} missing: run `cargo build --workspace` first", p.display());
    p
}

struct Env {
    dir: TestDir,
    home: PathBuf,
    repo: PathBuf,
    log: PathBuf,
}

impl Env {
    fn new() -> Env {
        let dir = TestDir::new();
        let home = dir.path().join("h");
        let repo = dir.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q", "-b", "trunk"][..],
            &["config", "user.email", "t@example.com"],
            &["config", "user.name", "T"],
            &["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
        }
        let log = dir.path().join("fake-claude.log");
        Env { dir, home, repo, log }
    }

    fn cmd(&self, program: &Path) -> Command {
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake-claude.sh");
        let mut c = Command::new(program);
        c.env("MIRANTE_HOME", &self.home)
            .env("MIRANTE_DAEMON_BIN", bin("mirante-daemon"))
            .env("MIRANTE_HOOK_BIN", bin("mirante-hook"))
            .env("MIRANTE_CLAUDE_BIN", fake)
            .env("FAKE_CLAUDE_LOG", &self.log)
            // Results must not depend on this machine's memory pressure.
            .env("MIRANTE_MEMGUARD", "off");
        c
    }

    fn mirante(&self, args: &[&str]) -> Output {
        self.cmd(&bin("mirante")).args(args).stdin(Stdio::null()).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.mirante(args);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "mirante {args:?}: {text}{}", String::from_utf8_lossy(&out.stderr));
        text
    }

    fn client(&self) -> Client {
        let c = Client::connect(&Paths::new(&self.home).socket()).unwrap();
        c.set_timeout(Some(Duration::from_secs(10))).unwrap();
        c
    }

    fn subscribe(&self) -> Client {
        let mut sub = self.client();
        let _: Snapshot = sub.call(method::EVENTS_SUBSCRIBE, json!({})).unwrap();
        sub
    }

    fn task(&self, branch: &str) -> TaskView {
        self.client().call(method::TASK_GET, json!({ "task": branch })).unwrap()
    }

    /// Every event of this daemon boot so far (replayed from seq 0).
    fn events(&self) -> Vec<Value> {
        let now: Snapshot = self.client().call(method::EVENTS_SUBSCRIBE, json!({})).unwrap();
        let mut sub = self.client();
        let replay = SubscribeParams { since_seq: Some(0), boot_id: Some(now.boot_id) };
        let snap: Snapshot = sub.call(method::EVENTS_SUBSCRIBE, replay).unwrap();
        assert!(snap.resumed);
        let mut out: Vec<Value> = Vec::new();
        while out.last().and_then(|e| e["seq"].as_u64()).unwrap_or(0) < now.seq {
            out.push(sub.next_notification().unwrap().expect("replay").params);
        }
        out
    }

    fn keys(&self) -> String {
        std::fs::read_to_string(self.log.with_extension("log.keys")).unwrap_or_default()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.mirante(&["daemon", "stop"]);
        let _ = &self.dir;
    }
}

/// Reads `event` notifications until `n` of kind `kind` arrived (10 s budget).
fn wait_events(sub: &mut Client, kind: &str, n: usize) -> Vec<Value> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while seen.iter().filter(|e: &&Value| e["kind"] == kind).count() < n {
        assert!(Instant::now() < deadline, "only {seen:?}");
        let note = sub.next_notification().unwrap().expect("daemon alive");
        seen.push(note.params);
    }
    seen
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The `--session-id` the fake was started with on its first launch.
fn pinned_id(log: &Path) -> String {
    let args = std::fs::read_to_string(log).unwrap();
    let first = args.lines().next().unwrap();
    let mut it = first.split_whitespace();
    while let Some(a) = it.next() {
        if a == "--session-id" {
            return it.next().unwrap().to_string();
        }
    }
    panic!("no --session-id in {first}")
}

#[test]
fn dispatch_steer_stop_resume_archive_with_a_fake_claude() {
    let env = Env::new();
    assert!(env.ok(&["daemon", "status"]).contains("not running"));
    assert!(env.ok(&["daemon", "start"]).contains("started"));
    let mut sub = env.subscribe();

    let repo = env.repo.to_string_lossy().into_owned();
    let out = env.ok(&["new", &repo, "--agent", "claude", "--branch", "e2e", "say", "hi"]);
    assert!(out.contains("ready"), "{out}");
    let events = wait_events(&mut sub, "turn_ended", 1);
    let statuses: Vec<&str> =
        events.iter().filter(|e| e["kind"] == "status").filter_map(|e| e["payload"]["to"].as_str()).collect();
    assert!(statuses.contains(&"working"), "{statuses:?}");
    let sid = pinned_id(&env.log);
    assert!(events.iter().any(|e| e["kind"] == "native_session_id" || e["payload"].to_string().contains(&sid)));

    assert!(env.ok(&["say", "e2e", "again"]).contains("sent now"));
    wait_events(&mut sub, "turn_ended", 1);
    let ls = env.ok(&["ls"]);
    assert!(ls.lines().any(|l| l.contains("e2e") && l.contains("idle") && l.contains("manual")), "{ls}");

    assert!(env.ok(&["stop", "e2e"]).contains("ended"));
    let out = env.ok(&["resume", "e2e"]);
    assert!(out.contains("ready (resumed)"), "{out}");
    let args = std::fs::read_to_string(&env.log).unwrap();
    assert!(args.lines().last().unwrap().contains(&format!("--resume {sid}")), "{args}");
    assert!(args.contains("--permission-mode manual") && !args.contains("dangerously"), "{args}");

    assert!(!env.mirante(&["archive", "e2e"]).status.success(), "archive must refuse a live session");
    env.ok(&["stop", "e2e"]);
    assert!(env.ok(&["archive", "e2e"]).contains("archived"));
    assert!(env.ok(&["ls"]).contains("no tasks"));
    assert!(env.ok(&["daemon", "stop"]).contains("stopped"));
    assert!(env.ok(&["daemon", "status"]).contains("not running"));
}

#[test]
fn bad_input_is_reported_not_swallowed() {
    let env = Env::new();
    let out = env.mirante(&["new", "/nope", "--agent", "claude", "--branch", "b", "go"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("/nope"));
    let out = env.mirante(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    let out = env.mirante(&["say", "missing-task", "hi"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("no task missing-task"));
}

#[test]
fn concurrent_answers_to_one_permission_type_one_key() {
    let env = Env::new();
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "perm", "ask", "first"]);
    wait_until("the card", || env.task("perm").needs_you == 1);
    let perms = env.ok(&["perms"]);
    let id = perms.lines().nth(1).unwrap().split_whitespace().next().unwrap().to_string();
    // Two `allow`s race: both succeed (the second is the stored answer), one key is typed.
    let a = env.cmd(&bin("mirante")).args(["allow", &id]).stdout(Stdio::piped()).spawn().unwrap();
    let b = env.cmd(&bin("mirante")).args(["allow", &id]).stdout(Stdio::piped()).spawn().unwrap();
    for out in [a.wait_with_output().unwrap(), b.wait_with_output().unwrap()] {
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        assert!(String::from_utf8_lossy(&out.stdout).contains("allowed_once"));
    }
    wait_until("the key", || !env.keys().is_empty());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(env.keys(), "1\n");
    // A different answer to the answered card is a conflict, and types nothing.
    let deny = env.mirante(&["deny", &id]);
    assert!(!deny.status.success());
    assert_eq!(env.keys(), "1\n");
}

#[test]
fn a_daemon_killed_mid_turn_ends_the_session_on_restart_and_resume_works() {
    let env = Env::new();
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "crash", "ask", "now"]);
    wait_until("the card", || env.task("crash").needs_you == 1);
    assert!(env.ok(&["say", "--queue", "crash", "later"]).contains("queued"));
    let s: DaemonStatus = env.client().call(method::DAEMON_STATUS, json!({})).unwrap();
    // SAFETY: plain syscall on the daemon this test started.
    assert_eq!(unsafe { libc::kill(s.pid as i32, libc::SIGKILL) }, 0);
    wait_until("the old daemon gone", || unsafe { libc::kill(s.pid as i32, 0) } != 0);

    // Any command starts a fresh daemon, whose recovery ends the orphaned session.
    let ls = env.ok(&["ls"]);
    assert!(ls.lines().any(|l| l.contains("crash") && l.contains("ended")), "{ls}");
    let kinds: Vec<String> = env.events().iter().map(|e| e["kind"].as_str().unwrap_or("").to_string()).collect();
    for k in ["permission_expired", "queue_dropped"] {
        assert!(kinds.iter().any(|x| x == k), "{k} missing from {kinds:?}");
    }
    assert!(env.ok(&["perms"]).contains("no pending"));
    let out = env.ok(&["resume", "crash"]);
    assert!(out.contains("ready (resumed)"), "{out}");
    let sid = pinned_id(&env.log);
    assert!(std::fs::read_to_string(&env.log).unwrap().lines().last().unwrap().contains(&format!("--resume {sid}")));
}

#[test]
fn a_slow_subscriber_is_dropped_while_the_daemon_keeps_answering_then_resumes() {
    let env = Env::new();
    env.ok(&["daemon", "start"]);
    let mut slow = env.client();
    let snap: Snapshot = slow.call(method::EVENTS_SUBSCRIBE, json!({})).unwrap();
    // 2000 events (hooks for an unknown task are logged as `hook_orphan`) while `slow`
    // reads nothing.
    let mut c = env.client();
    let hook = |i: usize| HookEnvelope {
        params: HookParams { task: "nobody".into(), payload: json!({"hook_event_name": "Stop"}).to_string(), ts_ms: 0 },
        event_id: Some(format!("e{i}")),
        received_ts_ms: None,
        truncated: None,
        session: None,
    };
    for i in 0..2000 {
        let _: Value = c.call(method::HOOK_EVENT, hook(i)).unwrap();
    }
    let started = Instant::now();
    let _: DaemonStatus = env.client().call(method::DAEMON_STATUS, json!({})).unwrap();
    assert!(started.elapsed() < Duration::from_secs(2), "the daemon must not wait for a slow reader");
    // The slow stream ends after what was buffered; it then resumes where it stopped.
    let mut last = snap.seq;
    while let Ok(Some(n)) = slow.next_notification() {
        last = n.params["seq"].as_u64().unwrap();
    }
    assert!(last < snap.seq + 2000, "the stream was cut");
    let mut again = env.client();
    let resumed: Snapshot = again
        .call(method::EVENTS_SUBSCRIBE, SubscribeParams { since_seq: Some(last), boot_id: Some(snap.boot_id) })
        .unwrap();
    assert!(resumed.resumed);
    let next = again.next_notification().unwrap().unwrap();
    assert_eq!(next.params["seq"].as_u64(), Some(last + 1));
}

#[test]
fn a_stale_hook_from_before_a_resume_is_dropped() {
    let env = Env::new();
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "stale", "say", "hi"]);
    wait_until("idle", || env.task("stale").task.status.as_str() == "idle");
    let task = env.task("stale").task.id;
    let old = env
        .events()
        .into_iter()
        .find(|e| e["kind"] == "session_launch")
        .and_then(|e| e["payload"]["session_id"].as_str().map(String::from))
        .unwrap();
    env.ok(&["stop", "stale"]);
    assert!(env.ok(&["resume", "stale"]).contains("ready (resumed)"));
    // The old Claude's hook arrives late, stamped with the old Mirante session.
    let mut h = env
        .cmd(&bin("mirante-hook"))
        .env("MIRANTE_TASK_ID", &task)
        .env("MIRANTE_SESSION_ID", &old)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    h.stdin.take().unwrap().write_all(br#"{"hook_event_name":"UserPromptSubmit","prompt":"late"}"#).unwrap();
    assert!(h.wait().unwrap().success());
    wait_until("hook_stale", || env.events().iter().any(|e| e["kind"] == "hook_stale"));
    assert_eq!(env.task("stale").task.status.as_str(), "idle");
}

#[test]
fn hibernate_wake_and_doctor_from_the_cli() {
    let env = Env::new();
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "nap", "say", "hi"]);
    wait_until("idle", || env.task("nap").task.status.as_str() == "idle");
    assert!(env.ok(&["hibernate", "nap"]).contains("hibernated"));
    assert!(env.ok(&["ls"]).contains("hibernated: mirante wake"));
    assert!(env.ok(&["wake", "nap"]).contains("ready (resumed)"));
    let doctor = env.ok(&["doctor"]);
    assert!(doctor.contains("orphan processes: none") && doctor.contains("orphan checkouts: none"), "{doctor}");
}
