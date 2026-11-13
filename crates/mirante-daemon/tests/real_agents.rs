//! The real agents through the daemon (hub + socket + hook binary). Ignored by default:
//! they need your Claude / Codex logins and spend plan quota. Everything runs in a fresh
//! temp git repo; no permission is broadened and nothing is auto-approved (the test answers
//! the one permission it provokes, for a `touch` inside the throwaway worktree).
//!
//!   cargo build --workspace
//!   MIRANTE_TEST_TRUST=1 MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude \
//!     cargo test -p mirante-daemon --test real_agents -- --ignored --test-threads=1 claude
//!   MIRANTE_CODEX_BIN=$(dirname $(readlink -f $(which node)))/codex \
//!     cargo test -p mirante-daemon --test real_agents -- --ignored --test-threads=1 codex
//!
//! `MIRANTE_TEST_TRUST=1` is you answering "yes" to Claude's trust dialog for the temp
//! worktree; without it the Claude test stops at the dialog.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use mirante_core::model::{AgentKind, Event, Status};
use mirante_core::store::Store;
use mirante_daemon::driver::real::RealFactory;
use mirante_daemon::hub::{Hub, HubError};
use mirante_daemon::server;
use mirante_ipc::test_dir::TestDir;
use mirante_ipc::types::{
    AnswerPermission, AttachParams, BootState, CreateTask, SendMessage, SendMode, StartSession, TrustAnswer,
};
use mirante_ipc::Paths;

struct Rig {
    hub: Arc<Hub>,
    events: mpsc::Receiver<Event>,
    paths: Paths,
    repo: PathBuf,
    _dir: TestDir,
}

fn rig() -> Rig {
    let dir = TestDir::new();
    let target = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    let hook = target.join("mirante-hook");
    assert!(hook.exists(), "run `cargo build --workspace` first");
    // Single-threaded test run (--test-threads=1): setting the variable is safe here.
    std::env::set_var("MIRANTE_HOOK_BIN", &hook);
    let paths = Paths::new(&dir.path().join("h"));
    paths.ensure_home().unwrap();
    let repo = dir.path().join("app");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "-b", "trunk"][..], &["commit", "-q", "--allow-empty", "-m", "init"]] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=T"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    let store = Store::open(&paths.database()).unwrap();
    let (hub, rx) = Hub::new(store, paths.clone(), Box::new(RealFactory::default())).unwrap();
    let hub = Arc::new(hub);
    hub.spawn_pump(rx);
    let listener = server::bind(&paths.socket()).unwrap();
    let h = hub.clone();
    std::thread::spawn(move || server::serve(h, listener, Arc::new(|| {})));
    let (_, events) = hub.subscribe().unwrap();
    Rig { hub, events, paths, repo, _dir: dir }
}

impl Rig {
    fn create(&self, agent: AgentKind, branch: &str, goal: &str) -> String {
        let v = self
            .hub
            .create_task(CreateTask {
                group: None,
                repos: Vec::new(),
                repo: self.repo.to_string_lossy().into_owned(),
                agent,
                branch: branch.into(),
                base: None,
                goal: goal.into(),
                title: None,
            })
            .unwrap();
        v.task.id
    }

    /// Waits for an event of `kind` on `task` matching `pred`; returns it.
    fn wait_event(&self, task: &str, kind: &str, secs: u64, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let e = self.events.recv_timeout(left).unwrap_or_else(|_| panic!("no {kind} event in {secs}s"));
            if e.task_id.as_deref() == Some(task) && e.kind == kind && pred(&e) {
                return e;
            }
        }
    }

    fn start(&self, task: &str, prompt: Option<&str>, resume: bool) -> bool {
        let mut r = self
            .hub
            .start_session(StartSession { task: task.into(), prompt: prompt.map(String::from) }, resume)
            .unwrap();
        if r.boot == BootState::TrustPrompt {
            assert_eq!(
                std::env::var("MIRANTE_TEST_TRUST").as_deref(),
                Ok("1"),
                "Claude asks to trust the temp worktree:\n{}\nrerun with MIRANTE_TEST_TRUST=1 to answer yes",
                r.screen
            );
            r = self.hub.trust(TrustAnswer { task: task.into(), accept: true }).unwrap();
        }
        assert_eq!(r.boot, BootState::Ready, "{}", r.screen);
        r.resumed
    }

    fn say(&self, task: &str, text: &str) {
        self.hub.send(SendMessage { task: task.into(), text: text.into(), mode: SendMode::Steer }).unwrap();
    }

    /// Answers the dialog in the agent's terminal, as the user does in `mirante attach`:
    /// only once the screen shows this command's dialog (the card can come before the
    /// dialog is drawn).
    fn answer_in_terminal(&self, task: &str, command: &str, key: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let (driver, a) = self.hub.attach(AttachParams { task: task.into(), rows: 40, cols: 120 }).unwrap();
            let screen = String::from_utf8_lossy(&a.initial).replace("\x1b[C", " ");
            if screen.contains(command) && screen.contains("Do you want to proceed?") {
                driver.input(a.id, key).unwrap();
                return;
            }
            assert!(Instant::now() < deadline, "this command's dialog never showed:\n{screen}");
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    /// Last assistant text in the live session's transcript (read through a second store
    /// connection; WAL allows a concurrent reader).
    fn last_answer(&self, task: &str) -> String {
        let store = Store::open(&self.paths.database()).unwrap();
        let s = store.live_session(task).unwrap().expect("live session");
        let text = std::fs::read_to_string(s.transcript_path.expect("transcript path")).unwrap();
        text.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["type"] == "assistant")
            .filter_map(|v| v["message"]["content"].as_array().cloned())
            .flatten()
            .filter_map(|c| c["text"].as_str().map(String::from))
            .next_back()
            .unwrap_or_default()
    }
}

fn to(e: &Event) -> Status {
    serde_json::from_value(e.payload["to"].clone()).unwrap()
}

#[test]
#[ignore]
fn claude_turn_permission_stop_and_resume() {
    let r = rig();
    let goal = "A palavra secreta é JABUTICABA. Responda apenas OK.";
    let t = r.create(AgentKind::Claude, "real-claude", goal);
    assert!(!r.start(&t, Some(goal), false));
    r.wait_event(&t, "status", 120, |e| to(e) == Status::Working);
    r.wait_event(&t, "turn_ended", 120, |_| true);

    r.say(&t, "Use o Bash para rodar exatamente `touch allowed.txt` e diga se deu certo.");
    r.wait_event(&t, "permission_requested", 120, |_| true);
    let p = r.hub.permissions().unwrap();
    assert_eq!(p.len(), 1, "{p:?}");
    // Inline answers are off by default (founder decision 12): Mirante types nothing and
    // says to answer in the terminal. The test then answers there, as the user would, the
    // one dialog it provoked (a `touch` inside the throwaway worktree).
    let refused = r.hub.answer_permission(AnswerPermission { id: p[0].request.id.clone(), allow: true });
    assert!(matches!(refused, Err(HubError::AnswerInTerminal { .. })), "{refused:?}");
    r.answer_in_terminal(&t, "touch allowed.txt", b"1");
    r.wait_event(&t, "turn_ended", 120, |_| true);
    let worktree = r.hub.get_task(&t).unwrap().task.worktree_path;
    assert!(Path::new(&worktree).join("allowed.txt").exists());

    r.hub.stop(&t).unwrap();
    assert!(r.start(&t, None, true), "resume must reuse the native session id");
    r.say(&t, "Qual é a palavra secreta? Responda só a palavra.");
    r.wait_event(&t, "turn_ended", 120, |_| true);
    // The Stop hook can arrive before the answer's transcript line is written (Task 21
    // live run): give the line a moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut answer = r.last_answer(&t);
    while !answer.contains("JABUTICABA") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        answer = r.last_answer(&t);
    }
    assert!(answer.contains("JABUTICABA"), "{answer:?}");
    r.hub.stop(&t).unwrap();
}

#[test]
#[ignore]
fn codex_steer_and_resume() {
    let r = rig();
    let goal = "Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma chamada por vez, com N de 1 a 5. Ao terminar, diga FIM.";
    let t = r.create(AgentKind::Codex, "real-codex", goal);
    assert!(!r.start(&t, Some(goal), false));
    r.wait_event(&t, "status", 60, |e| to(e) == Status::Working);
    std::thread::sleep(Duration::from_secs(8));
    r.say(&t, "Mudança de planos: pare o que está fazendo e responda apenas a palavra BANANA.");
    r.wait_event(&t, "step", 180, |e| e.payload["type"] == "agent_message" && e.payload["text"] == "BANANA");
    // The status moves to idle before `turn_ended` is recorded (events are read in order).
    r.wait_event(&t, "status", 180, |e| to(e) == Status::Idle);
    r.wait_event(&t, "turn_ended", 60, |_| true);

    r.hub.stop(&t).unwrap();
    assert!(r.start(&t, None, true), "resume must reuse the thread id");
    r.say(&t, "Qual palavra você respondeu antes? Responda só a palavra.");
    r.wait_event(&t, "step", 180, |e| e.payload["type"] == "agent_message" && e.payload["text"] == "BANANA");
    r.hub.stop(&t).unwrap();
}
