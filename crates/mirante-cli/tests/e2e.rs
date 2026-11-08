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
    /// The daemon types Claude answers (`MIRANTE_CLAUDE_INLINE_ANSWERS=1`).
    inline: bool,
    /// Sessions the daemon ended come back when it starts again (off unless a test asks:
    /// most restart tests check what stays ended).
    auto_resume: bool,
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
        Env { dir, home, repo, log, inline: false, auto_resume: false }
    }

    /// A daemon with Claude inline answers on.
    fn inline() -> Env {
        let mut env = Env::new();
        env.inline = true;
        env
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
        if !self.auto_resume {
            c.env("MIRANTE_AUTO_RESUME", "off");
        } else {
            c.env_remove("MIRANTE_AUTO_RESUME");
        }
        if self.inline {
            c.env("MIRANTE_CLAUDE_INLINE_ANSWERS", "1");
        } else {
            c.env_remove("MIRANTE_CLAUDE_INLINE_ANSWERS");
        }
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
fn claude_answers_default_to_the_terminal_and_nothing_is_typed() {
    let env = Env::new();
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "perm", "ask", "first"]);
    wait_until("the card", || env.task("perm").needs_you == 1);
    let s: DaemonStatus = env.client().call(method::DAEMON_STATUS, json!({})).unwrap();
    assert!(!s.claude_inline_answers);
    let perms = env.ok(&["perms"]);
    let id = perms.lines().nth(1).unwrap().split_whitespace().next().unwrap().to_string();
    for verb in ["allow", "deny"] {
        let out = env.mirante(&[verb, &id]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{verb}: {err}");
        assert!(err.contains("answer this dialog in the terminal (mirante attach perm)"), "{err}");
        assert!(err.contains("not answered"), "{err}");
    }
    // The typed error, as the cockpit sees it.
    let r = env.client().call::<Value>(method::PERMISSION_ANSWER, json!({ "id": id, "allow": true }));
    let Err(mirante_ipc::ClientError::Rpc(e)) = r else { panic!("{r:?}") };
    assert_eq!(e.code, mirante_ipc::rpc::code::ANSWER_IN_TERMINAL);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(env.keys(), "", "nothing was typed into the dialog");
    assert_eq!(env.task("perm").needs_you, 1, "the card waits for the terminal answer");
}

#[test]
fn concurrent_answers_to_one_permission_type_one_key() {
    // Inline answers on, with the evidence: the fake writes the tool_use line to its
    // transcript before the dialog, and the hook carries its tool_use_id.
    let env = Env::inline();
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
fn a_daemon_restart_resumes_live_sessions_but_not_stopped_ones() {
    let mut env = Env::new();
    env.auto_resume = true;
    let repo = env.repo.to_string_lossy().into_owned();
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "keep", "say", "hi"]);
    env.ok(&["new", &repo, "--agent", "claude", "--branch", "halt", "say", "hi"]);
    wait_until("both ready", || env.task("keep").live && env.task("halt").live);
    wait_until("native ids", || std::fs::read_to_string(&env.log).unwrap_or_default().lines().count() >= 2);
    env.ok(&["stop", "halt"]);
    let launches = std::fs::read_to_string(&env.log).unwrap().lines().count();
    let out = env.ok(&["daemon", "restart"]);
    assert!(out.contains("they resume when the daemon starts again") && out.contains("started"), "{out}");
    wait_until("keep resumed", || env.task("keep").live);
    let log = std::fs::read_to_string(&env.log).unwrap();
    let after: Vec<&str> = log.lines().skip(launches).collect();
    assert_eq!(after.len(), 1, "one launch after the restart: {after:?}");
    assert!(after[0].contains("--resume"), "{}", after[0]);
    assert!(!env.task("halt").live, "a session the person stopped stays stopped");
    wait_until("session.auto_resumed", || env.events().iter().any(|e| e["kind"] == "session.auto_resumed"));
    // The setting turns it off, and says so in daemon.status.
    let s: Value = env.client().call(method::DAEMON_SETTINGS, json!({ "auto_resume": false })).unwrap();
    assert_eq!(s["auto_resume"], false);
    let st: DaemonStatus = env.client().call(method::DAEMON_STATUS, json!({})).unwrap();
    assert_eq!(st.auto_resume, Some(false));
    assert!(env.ok(&["daemon", "restart"]).contains("mirante resume <task> restores them"));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!env.task("keep").live, "auto-resume off: nothing comes back");
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
    // The mark survives a daemon restart (rebuilt from the trail).
    env.ok(&["daemon", "restart"]);
    assert!(env.ok(&["ls"]).contains("hibernated: mirante wake"));
    assert!(env.ok(&["wake", "nap"]).contains("ready (resumed)"));
    let doctor = env.ok(&["doctor"]);
    assert!(doctor.contains("orphan processes: none") && doctor.contains("orphan checkouts: none"), "{doctor}");
}

/// `mirante memory accept|reject` asks on the controlling terminal: without one (here:
/// `setsid`, like an agent's tool call or a pipe) it refuses before contacting the daemon.
#[test]
fn memory_review_without_a_terminal_is_refused_before_the_daemon() {
    use std::os::unix::process::CommandExt;
    let env = Env::new();
    for verb in ["accept", "reject"] {
        let mut cmd = env.cmd(&bin("mirante"));
        cmd.args(["memory", verb, "0193b7c2-0000-7000-8000-000000000000"]).stdin(Stdio::null());
        // SAFETY: setsid is async-signal-safe; it detaches the child from any terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let out = cmd.output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{err}");
        assert!(err.contains("/dev/tty") && err.contains("cockpit"), "{err}");
    }
    assert!(!env.home.join("daemon.sock").exists(), "the daemon was never started");
}

/// Git in `dir` with a fixed identity.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

impl Env {
    fn commit(&self, dir: &Path, path: &str, text: &str) {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
        git(dir, &["add", "-A", "--", path]);
        git(dir, &["commit", "-q", "-m", path]);
    }

    /// The repo with `src/a.rs` committed and a committed note `hook-spool` citing
    /// `path:start-end` of it at that commit.
    fn repo_with_note_citing(&self, path: &str, start: u32, end: u32) -> PathBuf {
        use mirante_core::dna::check::cite;
        use mirante_core::dna::note::{render, Kind, Note, Status};
        self.commit(&self.repo, path, "fn spool() {\n    write();\n}\n");
        let anchor = cite(&self.repo, "HEAD", path, None, start, end).unwrap();
        let note = Note {
            id: "0193b7c2-5f1e-7c3a-9a10-3f2b8d1e4c55".into(),
            title: "Hook events fall back to the spool".into(),
            kind: Kind::Gotcha,
            status: Status::Active,
            created: "2026-11-07".into(),
            source: "cli".into(),
            anchors: vec![anchor],
            body: "When the socket is down, events go to the spool.\n".into(),
        };
        self.commit(&self.repo, ".mirante/memory/hook-spool.md", &render(&note).unwrap());
        self.repo.clone()
    }

    fn mirante_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.cmd(&bin("mirante")).args(args).current_dir(dir).stdin(Stdio::null()).output().unwrap()
    }

    fn ok_in(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.mirante_in(dir, args);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "mirante {args:?}: {text}{}", String::from_utf8_lossy(&out.stderr));
        text
    }

    /// `mirante <args>` on a pseudo-terminal (`script`), typing `answer` once it asks.
    /// Returns everything it printed.
    fn on_tty(&self, dir: &Path, args: &[&str], answer: &str) -> String {
        self.on_tty_with(dir, args, Some(answer), &[])
    }

    /// [`Env::on_tty`] with `envs` added; `answer: None` types nothing and waits for exit.
    fn on_tty_with(&self, dir: &Path, args: &[&str], answer: Option<&str>, envs: &[(&str, &str)]) -> String {
        use std::io::{Read, Write};
        use std::sync::{Arc, Mutex};
        let mirante = bin("mirante");
        let mut cmd = self.cmd(Path::new("script"));
        cmd.envs(envs.iter().copied());
        if cfg!(target_os = "macos") {
            cmd.args(["-q", "/dev/null"]).arg(&mirante).args(args);
        } else {
            let quoted: Vec<String> = std::iter::once(mirante.to_string_lossy().into_owned())
                .chain(args.iter().map(|a| a.to_string()))
                .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
                .collect();
            cmd.args(["-q", "-e", "-c", &quoted.join(" "), "/dev/null"]);
        }
        let mut child =
            cmd.current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let seen = Arc::new(Mutex::new(String::new()));
        let mut stdout = child.stdout.take().unwrap();
        let sink = seen.clone();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        let mut stdin = child.stdin.take().unwrap();
        if let Some(answer) = answer {
            wait_until("the confirmation prompt", || seen.lock().unwrap().contains("to confirm"));
            stdin.write_all(format!("{answer}\n").as_bytes()).unwrap();
        }
        // Bounded: a prompt nobody answers must fail the test, not hang it.
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.wait();
        drop(stdin);
        reader.join().unwrap();
        let out = seen.lock().unwrap().clone();
        out
    }
}

#[test]
fn memory_check_offline_exits_1_on_stale_and_names_the_note() {
    let env = Env::new();
    let repo = env.repo_with_note_citing("src/a.rs", 1, 2);
    // From a subdirectory too: the repo is the checkout around it.
    let out = env.mirante_in(&repo.join("src"), &["memory", "check", "--offline"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("fresh  hook-spool  src/a.rs:1-2"), "{text}");

    env.commit(&repo, "src/a.rs", "changed\n");
    let out = env.mirante_in(&repo, &["memory", "check", "--offline"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("stale  hook-spool  src/a.rs:1-2"), "{text}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("1 stale"), "{}", String::from_utf8_lossy(&out.stderr));

    // Against the earlier commit (a PR's base, say) the note still holds.
    let out = env.mirante_in(&repo, &["memory", "check", "--offline", "--rev", "HEAD~1"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(!env.home.join("daemon.sock").exists(), "offline never starts the daemon");
}

/// `memory check` exits 1 only for what it found (stale, unverifiable, invalid) and 3 when
/// it could not check at all, so a CI job tells a broken note from a broken setup.
#[test]
fn memory_check_exits_3_when_it_could_not_run() {
    let env = Env::new();
    let repo = env.repo_with_note_citing("src/a.rs", 1, 2);
    let code = |dir: &Path, args: &[&str]| {
        let out = env.mirante_in(dir, args);
        (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let (c, err) = code(env.dir.path(), &["memory", "check", "--offline"]);
    assert_eq!(c, Some(3), "not a checkout: {err}");
    let (c, err) = code(&repo, &["memory", "check", "--offline", "--rev", "no-such-rev"]);
    assert_eq!(c, Some(3), "a bad --rev: {err}");
    // The daemon cannot start.
    let out = env
        .cmd(&bin("mirante"))
        .args(["memory", "check"])
        .env("MIRANTE_DAEMON_BIN", env.dir.path().join("missing"))
        .current_dir(&repo)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stderr));
    // A shallow clone: no cited commit is in it. Exit 3 with how to fix it, not a list of
    // "no longer exists"; online too, before any daemon is started.
    env.commit(&repo, "src/b.rs", "b\n");
    let clone = env.dir.path().join("shallow");
    let url = format!("file://{}", repo.display());
    let ok = std::process::Command::new("git")
        .args(["clone", "-q", "--depth", "1", &url])
        .arg(&clone)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git clone --depth 1");
    for args in [&["memory", "check", "--offline"][..], &["memory", "check"][..]] {
        let (c, err) = code(&clone, args);
        assert_eq!(c, Some(3), "{args:?}: {err}");
        assert!(err.contains("shallow clone") && err.contains("fetch-depth: 0"), "{err}");
    }
    assert!(!env.home.join("daemon.sock").exists(), "a shallow clone never starts the daemon");
    // A finding is still 1, online and offline.
    env.commit(&repo, "src/a.rs", "changed\n");
    assert_eq!(code(&repo, &["memory", "check", "--offline"]).0, Some(1));
    assert_eq!(code(&repo, &["memory", "check"]).0, Some(1));
}

#[test]
fn memory_enable_ls_accept_round_trip_through_the_daemon() {
    use mirante_core::model::AgentKind;
    use mirante_ipc::types::{CreateTask, DnaProposals, ProposalView};
    let env = Env::new();
    let repo = env.repo.clone();
    assert!(env.ok_in(&repo, &["memory", "enable"]).contains("enabled"));
    assert!(repo.join(".mirante/memory/README.md").exists());
    assert!(env.ok_in(&repo, &["memory", "ls"]).contains("no notes"));

    // A task to cite from (no session needed): committed code in its worktree.
    let create = CreateTask {
        repo: repo.to_string_lossy().into_owned(),
        agent: AgentKind::Claude,
        branch: "mem".into(),
        base: None,
        goal: "g".into(),
        title: None,
    };
    let t: TaskView = env.client().call(method::TASK_CREATE, create).unwrap();
    env.commit(Path::new(&t.task.worktree_path), "src/a.rs", "fn spool() {\n    write();\n}\n");
    let out = env.ok_in(
        &repo,
        &[
            "memory",
            "propose",
            "--task",
            "mem",
            "--title",
            "Spool falls back",
            "--kind",
            "gotcha",
            "--cite",
            "src/a.rs:1-3#spool",
            "uses TOKEN=ghp_0123456789abcdefghijABCDEFGHIJ012345 when down",
        ],
    );
    assert!(out.contains("waiting for review") && out.contains("1 secret"), "{out}");
    let queue = env.ok_in(&repo, &["memory", "proposals"]);
    assert!(queue.contains("Spool falls back") && queue.contains("cli"), "{queue}");
    let pending: Vec<ProposalView> = env.client().call(method::DNA_PROPOSALS, DnaProposals::default()).unwrap();
    let id = pending[0].id.clone();

    let out = env.on_tty(&repo, &["memory", "accept", &id], "accept");
    assert!(out.contains("accepted: .mirante/memory/spool-falls-back.md"), "{out}");
    assert!(env.ok_in(&repo, &["memory", "proposals"]).contains("no pending proposals"));
    assert!(env.ok_in(&repo, &["memory", "proposals", "--all"]).contains("accepted"));

    let ls = env.ok_in(&repo, &["memory", "ls"]);
    assert!(ls.lines().any(|l| l.contains("spool-falls-back") && l.contains("reviewed")), "{ls}");
    let show = env.ok_in(&repo, &["memory", "show", "spool-falls-back"]);
    assert!(
        show.contains("Spool falls back") && show.contains("src/a.rs:1-3") && show.contains("«redacted»"),
        "{show}"
    );
    assert!(!show.contains("ghp_0123"), "{show}");
    assert!(env.ok_in(&repo, &["memory", "search", "spool"]).contains("spool-falls-back"));
    // The accepted note cites code only on the task branch: the checkout's HEAD lacks it.
    let check = env.mirante_in(&repo, &["memory", "check"]);
    assert_eq!(check.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&check.stdout).contains("spool-falls-back  src/a.rs:1-3"));
    // Once the code lands on the checkout the note holds there, and a person confirms it.
    env.commit(&repo, "src/a.rs", "fn spool() {\n    write();\n}\n");
    assert!(env.ok_in(&repo, &["memory", "check"]).contains("fresh  spool-falls-back  src/a.rs:1-3"));
    let out = env.on_tty(&repo, &["memory", "confirm", "spool-falls-back"], "confirm");
    assert!(out.contains("confirmed spool-falls-back"), "{out}");
    let text = std::fs::read_to_string(repo.join(".mirante/memory/spool-falls-back.md")).unwrap();
    let head = String::from_utf8(
        Command::new("git").arg("-C").arg(&repo).args(["rev-parse", "HEAD"]).output().unwrap().stdout,
    )
    .unwrap();
    assert!(text.contains(&format!("@{}~", head.trim())), "re-anchored at the checkout's HEAD: {text}");

    // A wrong word decides nothing; retire keeps the file.
    let out = env.on_tty(&repo, &["memory", "retire", "spool-falls-back"], "yes");
    assert!(out.contains("not confirmed"), "{out}");
    let out = env.on_tty(&repo, &["memory", "retire", "spool-falls-back"], "retire");
    assert!(out.contains("retired spool-falls-back"), "{out}");
    let text = std::fs::read_to_string(repo.join(".mirante/memory/spool-falls-back.md")).unwrap();
    assert!(text.contains("status: retired"), "{text}");

    assert!(env.ok_in(&repo, &["memory", "disable"]).contains("disabled"));
    assert!(repo.join(".mirante/memory/spool-falls-back.md").exists());
}

/// On a terminal, a Mirante-launched agent's process (here: its env marker) is refused
/// before it is shown or asked anything; the daemon is the boundary, the prompt only UX.
#[test]
fn memory_review_from_an_agent_is_refused_before_the_prompt() {
    use mirante_core::model::AgentKind;
    use mirante_ipc::types::{CreateTask, DnaProposals, ProposalView};
    let env = Env::new();
    let repo = env.repo.clone();
    env.ok_in(&repo, &["memory", "enable"]);
    let create = CreateTask {
        repo: repo.to_string_lossy().into_owned(),
        agent: AgentKind::Claude,
        branch: "mem".into(),
        base: None,
        goal: "g".into(),
        title: None,
    };
    let t: TaskView = env.client().call(method::TASK_CREATE, create).unwrap();
    env.commit(Path::new(&t.task.worktree_path), "src/a.rs", "fn spool() {}\n");
    let propose =
        ["memory", "propose", "--task", "mem", "--title", "T", "--kind", "fact", "--cite", "src/a.rs:1-1", "b"];
    env.ok_in(&repo, &propose);
    let pending: Vec<ProposalView> = env.client().call(method::DNA_PROPOSALS, DnaProposals::default()).unwrap();
    let id = pending[0].id.clone();
    let out = env.on_tty_with(&repo, &["memory", "accept", &id], None, &[("MIRANTE_TASK", &t.task.id)]);
    assert!(out.contains("Mirante-launched agents are refused") && !out.contains("to confirm"), "{out}");
    let still: Vec<ProposalView> = env.client().call(method::DNA_PROPOSALS, DnaProposals::default()).unwrap();
    assert_eq!(still.len(), 1, "nothing was decided");
}

/// `confirm` and `retire` are person decisions like `accept`: no terminal, no decision.
#[test]
fn memory_note_actions_without_a_terminal_are_refused_before_the_daemon() {
    use std::os::unix::process::CommandExt;
    let env = Env::new();
    for verb in ["confirm", "retire"] {
        let mut cmd = env.cmd(&bin("mirante"));
        cmd.args(["memory", verb, "a-note"]).current_dir(&env.repo).stdin(Stdio::null());
        // SAFETY: setsid is async-signal-safe; it detaches the child from any terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let out = cmd.output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{err}");
        assert!(err.contains("/dev/tty") && err.contains("cockpit"), "{err}");
    }
    assert!(!env.home.join("daemon.sock").exists(), "the daemon was never started");
}

/// What a Mirante-launched agent's process tree may do over the real socket: its memory MCP
/// server (`mirante-mcp`, own task) searches and proposes; the `mirante` CLI run from inside
/// the agent (the agent env marker) is refused lifecycle methods like `new` and `ls`.
#[test]
fn an_agent_tree_reaches_its_memory_server_but_no_lifecycle_method() {
    use mirante_core::model::AgentKind;
    use mirante_ipc::types::{CreateTask, DnaProposals, ProposalView};
    use std::io::Write;
    let env = Env::new();
    let repo = env.repo.clone();
    env.ok_in(&repo, &["memory", "enable"]);
    let create = CreateTask {
        repo: repo.to_string_lossy().into_owned(),
        agent: AgentKind::Claude,
        branch: "agent".into(),
        base: None,
        goal: "g".into(),
        title: None,
    };
    let t: TaskView = env.client().call(method::TASK_CREATE, create).unwrap();
    env.commit(Path::new(&t.task.worktree_path), "src/a.rs", "fn spool() {\n    write();\n}\n");

    let mut mcp = env
        .cmd(&bin("mirante-mcp"))
        .env("MIRANTE_TASK_ID", &t.task.id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let call = |id: u32, name: &str, args: Value| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}});
    let anchor = json!([{"path": "src/a.rs", "start_line": 1, "end_line": 3}]);
    let propose = json!({"title": "Spool", "kind": "fact", "body": "Writes go to the spool.", "anchors": anchor});
    let input = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        call(2, "memory_search", json!({"query": "spool"})),
        call(3, "memory_propose", propose),
    ];
    {
        let mut stdin = mcp.stdin.take().unwrap();
        for line in &input {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    let out = mcp.wait_with_output().unwrap();
    let replies: Vec<Value> =
        String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 3, "{replies:?}");
    for r in &replies[1..] {
        assert_ne!(r["result"]["isError"], true, "{r}");
    }
    assert!(replies[2]["result"]["content"][0]["text"].as_str().unwrap().contains("waiting for a person's review"));
    let pending: Vec<ProposalView> = env.client().call(method::DNA_PROPOSALS, DnaProposals::default()).unwrap();
    assert_eq!(pending.len(), 1);

    // The CLI from inside the agent: refused, and nothing was created.
    for args in [&["ls"][..], &["new", &repo.to_string_lossy(), "--agent", "claude", "--branch", "x", "hi"]] {
        let out = env.cmd(&bin("mirante")).args(args).env("MIRANTE_TASK", &t.task.id).stdin(Stdio::null()).output();
        let out = out.unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!out.status.success(), "mirante {args:?}: {text}");
        assert!(text.contains("Mirante-launched agents are refused"), "mirante {args:?}: {text}");
    }
    let tasks: Vec<TaskView> = env.client().call(method::TASK_LIST, Value::Null).unwrap();
    assert_eq!(tasks.len(), 1);
}

/// The `mirante-skills` server from an agent's tree: `skill_propose` reaches the daemon for
/// its own task only (an agent proposal of the task's repo), and the CLI from that tree
/// still cannot review it.
#[test]
fn an_agent_proposes_a_skill_through_the_skills_server() {
    use mirante_core::model::AgentKind;
    use mirante_ipc::types::{CreateTask, SkillLearn, SkillProposalView, SkillProposals};
    use std::io::Write;
    let env = Env::new();
    let repo = env.repo.clone();
    let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
    git(&["remote", "add", "origin", "https://x-access-token:ghp_secret@github.com/Acme/App.git"]);
    env.ok(&["daemon", "start"]);
    let create = |branch: &str| CreateTask {
        repo: repo.to_string_lossy().into_owned(),
        agent: AgentKind::Codex,
        branch: branch.into(),
        base: None,
        goal: "g".into(),
        title: None,
    };
    let t: TaskView = env.client().call(method::TASK_CREATE, create("agent")).unwrap();
    let other: TaskView = env.client().call(method::TASK_CREATE, create("other")).unwrap();
    let learn = SkillLearn { tenant: "github.com/acme".into(), enabled: true };
    let _: Value = env.client().call(method::SKILL_LEARN, learn).unwrap();

    let mut mcp = env
        .cmd(&bin("mirante-mcp"))
        .args(["--surface", "skills"])
        .env("MIRANTE_TASK_ID", &t.task.id)
        .env("MIRANTE_TASK", &t.task.id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let args = json!({"name": "tests-first", "description": "Run the tests first", "body": "Run `cargo test` first.\n",
                      "task": other.task.id});
    let input = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "skill_propose", "arguments": args}}),
    ];
    {
        let mut stdin = mcp.stdin.take().unwrap();
        for line in &input {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    let out = mcp.wait_with_output().unwrap();
    let replies: Vec<Value> =
        String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_eq!(replies[1]["result"]["tools"][0]["name"], "skill_propose");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(replies[2]["result"]["isError"], false, "{}", replies[2]);
    let text = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("(new skill tests-first) is waiting for a person's review"), "{text}");
    let pending: Vec<SkillProposalView> =
        env.client().call(method::SKILL_PROPOSALS, SkillProposals::default()).unwrap();
    assert_eq!(pending.len(), 1);
    let p = &pending[0].proposal;
    assert_eq!((p.origin.as_str(), p.task_id.as_deref()), ("agent", Some(t.task.id.as_str())), "its own task");
    assert_eq!(p.scope, "repo:github.com/acme/app");
    assert_eq!(p.tenant, "github.com/acme");
}

/// `mirante skills tenant` prints the checkout's normalized origin and tenant: no token, no
/// ssh alias, no userinfo.
#[test]
fn tenant_prints_normalized_origin_without_token() {
    let env = Env::new();
    let repo = env.repo.clone();
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://x-access-token:ghp_0123456789abcdefghijABCDEFGHIJ012345@github.com/Acme/App.git",
        ],
    );
    let out = env.ok_in(&repo, &["skills", "tenant"]);
    assert!(out.contains("origin  github.com/acme/app"), "{out}");
    assert!(out.contains("tenant  github.com/acme"), "{out}");
    assert!(out.contains("learning off"), "{out}");
    assert!(!out.contains("ghp_") && !out.contains("x-access-token") && !out.contains("Acme"), "{out}");
}

/// Offline lint of a SKILL.md: exit 0 when clean, 1 with `rule (severity) file:line` per
/// finding, 3 when the file cannot be read.
#[test]
fn skills_lint_exits_1_on_errors_and_prints_rule_and_line() {
    let env = Env::new();
    let dir = env.dir.path().join("skill");
    std::fs::create_dir_all(dir.join("reference")).unwrap();
    std::fs::write(dir.join("reference/a.md"), "ref\n").unwrap();
    let md = dir.join("SKILL.md");
    std::fs::write(&md, "---\nname: tests-first\ndescription: Run the tests first\n---\nRun `cargo test`.\nSee [a](./reference/a.md).\n").unwrap();
    let run = |args: &[&str]| {
        let out = env.mirante_in(&env.repo, args);
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (code, out, err) = run(&["skills", "lint", md.to_str().unwrap()]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("no findings"), "{out}");
    std::fs::write(
        &md,
        "---\nname: tests-first\ndescription: d\nallowed-tools: Bash\nmodel: x\n---\nRun tests.\nUse --dangerously-skip-permissions to go fast.\n",
    )
    .unwrap();
    let (code, out, err) = run(&["skills", "lint", md.to_str().unwrap()]);
    assert_eq!(code, Some(1), "{out}{err}");
    assert!(out.contains("forbidden (error) SKILL.md:8"), "the file's line, frontmatter counted: {out}");
    assert!(out.contains("forbidden (error) frontmatter:0: allowed-tools"), "{out}");
    assert!(out.lines().any(|l| l.starts_with("frontmatter (warn)")), "{out}");
    assert!(!env.home.join("daemon.sock").exists(), "lint runs offline");
    let (code, _, err) = run(&["skills", "lint", dir.join("missing.md").to_str().unwrap()]);
    assert_eq!(code, Some(3), "{err}");
}

/// A person adds a skill folder (a proposal), accepts it on a terminal (signed v1), lists
/// and shows it; a wrong word decides nothing; retire and restore are typed too. From an
/// agent's environment the review is refused before any prompt.
#[test]
fn skills_add_accept_ls_round_trip() {
    use mirante_ipc::types::{SkillProposalView, SkillProposals};
    let env = Env::new();
    let repo = env.repo.clone();
    git(&repo, &["remote", "add", "origin", "git@github.com:acme/app.git"]);
    let dir = env.dir.path().join("tests-first");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: tests-first\ndescription: Run the tests first\n---\nRun `cargo test` before committing.\n",
    )
    .unwrap();
    let out = env.ok_in(&repo, &["skills", "add", dir.to_str().unwrap(), "--scope", "org:github.com/acme"]);
    assert!(out.contains("waiting for review") && out.contains("mirante skills accept"), "{out}");
    let queue = env.ok_in(&repo, &["skills", "proposals"]);
    assert!(queue.contains("tests-first") && queue.contains("import"), "{queue}");
    let pending: Vec<SkillProposalView> =
        env.client().call(method::SKILL_PROPOSALS, SkillProposals::default()).unwrap();
    let id = pending[0].proposal.id.clone();

    // From an agent's environment: refused before it is shown or asked anything.
    let out = env.on_tty_with(&repo, &["skills", "accept", &id], None, &[("MIRANTE_TASK", "some-task")]);
    assert!(out.contains("Mirante-launched agents are refused") && !out.contains("to confirm"), "{out}");
    let out = env.on_tty(&repo, &["skills", "accept", &id], "yes");
    assert!(out.contains("not confirmed"), "{out}");
    let out = env.on_tty(&repo, &["skills", "accept", &id, "--activation", "on_demand"], "accept");
    assert!(out.contains("Run `cargo test` before committing.") && out.contains("org:github.com/acme"), "{out}");
    assert!(out.contains("accepted: tests-first v1"), "{out}");
    assert!(env.home.join("skills/tests-first/v1/mirante-skill.sig").exists());
    assert!(env.ok_in(&repo, &["skills", "proposals"]).contains("no pending proposals"));
    assert!(env.ok_in(&repo, &["skills", "proposals", "--all"]).contains("accepted"));

    let ls = env.ok_in(&repo, &["skills", "ls"]);
    assert!(ls.lines().any(|l| l.contains("tests-first") && l.contains("on_demand") && l.contains("verified")), "{ls}");
    let show = env.ok_in(&repo, &["skills", "show", "tests-first"]);
    assert!(show.contains("Run `cargo test` before committing.") && show.contains("versions: 1"), "{show}");

    let out = env.on_tty(&repo, &["skills", "retire", "tests-first"], "retire");
    assert!(out.contains("retired tests-first"), "{out}");
    assert!(env.ok_in(&repo, &["skills", "ls"]).lines().any(|l| l.contains("tests-first") && l.contains("retired")));
    let out = env.on_tty(&repo, &["skills", "restore", "tests-first"], "restore");
    assert!(out.contains("restored tests-first"), "{out}");
}

/// `tenant`, `learn` and `mine` follow the origin the daemon pinned for the checkout, not
/// the live `remote.origin.url` an agent could have changed: `tenant` shows both and the
/// counts, `mine` refuses while they differ or when the checkout has no task yet.
#[test]
fn skills_tenant_and_mine_follow_the_pinned_origin() {
    use mirante_core::model::AgentKind;
    use mirante_ipc::types::{CreateTask, SkillLearn};
    let env = Env::new();
    let repo = env.repo.clone();
    git(&repo, &["remote", "add", "origin", "git@github.com:acme/app.git"]);
    env.ok(&["daemon", "start"]);
    let run = |args: &[&str]| {
        let out = env.mirante_in(&repo, args);
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        )
    };
    // No task in this checkout yet: nothing is pinned, mining is refused.
    let (ok, out) = run(&["skills", "mine"]);
    assert!(!ok && out.contains("no task"), "{out}");
    let create = CreateTask {
        repo: repo.to_string_lossy().into_owned(),
        agent: AgentKind::Codex,
        branch: "b1".into(),
        base: None,
        goal: "g".into(),
        title: None,
    };
    let _: TaskView = env.client().call(method::TASK_CREATE, create).unwrap();
    let learn = SkillLearn { tenant: "github.com/acme".into(), enabled: true };
    let _: Value = env.client().call(method::SKILL_LEARN, learn).unwrap();
    let out = env.ok_in(&repo, &["skills", "tenant"]);
    assert!(out.contains("origin  github.com/acme/app (pinned)"), "{out}");
    assert!(out.contains("tenant  github.com/acme  (learning on; 1 tasks, 0 skills)"), "{out}");
    let (ok, out) = run(&["skills", "mine"]);
    assert!(ok && out.contains("github.com/acme:"), "{out}");
    // The remote moved (an agent can run git remote set-url): shown, and mining refused.
    git(&repo, &["remote", "set-url", "origin", "git@github.com:evil/x.git"]);
    let out = env.ok_in(&repo, &["skills", "tenant"]);
    assert!(out.contains("origin  github.com/acme/app (pinned)"), "{out}");
    assert!(out.contains("github.com/evil/x") && out.contains("WARNING"), "{out}");
    let (ok, out) = run(&["skills", "mine"]);
    assert!(!ok && out.contains("github.com/evil/x") && out.contains("github.com/acme/app"), "{out}");
    assert!(!out.contains("new proposal"), "{out}");
}
