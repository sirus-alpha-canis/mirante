//! `mirante`: thin client of `mirante-daemon`.
use std::collections::HashMap;
use std::error::Error;
use std::io::{self, BufRead, Read, Write};
use std::net::Shutdown;
use std::process::ExitCode;
use std::time::Duration;

use mirante_core::model::{PermissionRequest, Status};
use mirante_ipc::types::{
    method, AnswerPermission, AttachParams, AttachResult, BootState, CreateTask, DaemonStatus, ListParams,
    PermissionView, SendMessage, SendMode, SendResult, Snapshot, StartResult, StartSession, TaskRef, TaskView,
    TrustAnswer,
};
use mirante_ipc::{Client, Paths};
use serde_json::{json, Value};

use mirante_cli::args::{self, Cmd, DaemonCmd, USAGE};
use mirante_cli::{daemon_ctl, render, term};

type Res = Result<(), Box<dyn Error>>;

/// Ctrl-] detaches from `attach`.
const DETACH: u8 = 0x1d;

fn client(paths: &Paths) -> Result<Client, Box<dyn Error>> {
    let c = daemon_ctl::connect(paths)?;
    // `session.start` may wait up to 45 s for the agent to boot.
    c.set_timeout(Some(Duration::from_secs(90)))?;
    Ok(c)
}

fn ask(question: &str) -> io::Result<bool> {
    print!("{question}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Walks the start result: asks the user about Claude's trust dialog (unless `--trust`
/// said yes up front), reports how the boot ended.
fn boot_flow(c: &mut Client, mut r: StartResult, trust: bool) -> Res {
    let id = render::short(&r.task).to_string();
    loop {
        match r.boot {
            BootState::Ready => {
                println!("{id} ready{}. attach: mirante attach {id}", if r.resumed { " (resumed)" } else { "" });
                return Ok(());
            }
            BootState::Booting => {
                println!("{id} is still starting; check `mirante ls`");
                return Ok(());
            }
            BootState::Exited => return Err(format!("the agent exited during startup:\n{}", r.screen).into()),
            BootState::TrustPrompt => {
                println!("Claude asks whether to trust the worktree folder:\n\n{}\n", r.screen);
                let accept = trust || ask("Trust this folder and continue? [y/N] ")?;
                r = c.call(method::SESSION_TRUST, TrustAnswer { task: r.task.clone(), accept })?;
                if !accept {
                    println!("declined: the session ended (mirante resume {id} to try again)");
                    return Ok(());
                }
            }
        }
    }
}

fn attach(paths: &Paths, task: &str) -> Res {
    if !term::is_tty(0) || !term::is_tty(1) {
        return Err("attach needs a terminal".into());
    }
    let (rows, cols) = term::size();
    let mut c = client(paths)?;
    let r: AttachResult = c.call(method::ATTACH, AttachParams { task: task.into(), rows, cols })?;
    c.set_timeout(None)?;
    eprintln!("attached{}; detach with Ctrl-]", if r.input { "" } else { " (read-only stream)" });
    let (mut reader, writer) = c.into_raw();
    let raw = term::RawMode::enable()?;
    let mut w = writer.try_clone()?;
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = [0u8; 1024];
        loop {
            let n = match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let (chunk, detach) = match buf[..n].iter().position(|b| *b == DETACH) {
                Some(i) => (&buf[..i], true),
                None => (&buf[..n], false),
            };
            if r.input && !chunk.is_empty() && w.write_all(chunk).is_err() {
                break;
            }
            if detach {
                break;
            }
        }
        let _ = w.shutdown(Shutdown::Both);
    });
    let mut out = io::stdout().lock();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                out.write_all(&buf[..n])?;
                out.flush()?;
            }
        }
    }
    drop(raw);
    // Reset attributes and show the cursor in case the agent's TUI left them changed.
    print!("\x1b[0m\x1b[?25h\r\ndetached\r\n");
    Ok(())
}

fn board(tasks: &[TaskView], seen: &mut HashMap<String, Status>, color: bool) {
    // Bell only when a task newly needs the user.
    let bell = tasks
        .iter()
        .any(|v| v.task.status == Status::NeedsYou && seen.get(&v.task.id).is_some_and(|s| *s != Status::NeedsYou));
    seen.clear();
    seen.extend(tasks.iter().map(|v| (v.task.id.clone(), v.task.status)));
    print!("\x1b[2J\x1b[H");
    println!("mirante watch  (Ctrl-C to quit)\n");
    print!("{}", render::tasks_table(tasks, color));
    if bell {
        print!("\x07");
    }
    let _ = io::stdout().flush();
}

fn watch(paths: &Paths) -> Res {
    let color = term::is_tty(1);
    let mut seen = HashMap::new();
    loop {
        // No auto-start here: after `mirante daemon stop` the board waits, it does not
        // bring the daemon back.
        let session = (|| -> Result<(), Box<dyn Error>> {
            let mut sub = Client::connect(&paths.socket())?;
            let snap: Snapshot = sub.call(method::EVENTS_SUBSCRIBE, json!({}))?;
            board(&snap.tasks, &mut seen, color);
            let mut q = Client::connect(&paths.socket())?;
            q.set_timeout(Some(Duration::from_secs(10)))?;
            // The stream is only a hint: every event triggers a fresh snapshot.
            while sub.next_notification()?.is_some() {
                let tasks: Vec<TaskView> = q.call(method::TASK_LIST, ListParams::default())?;
                board(&tasks, &mut seen, color);
            }
            Ok(())
        })();
        if let Err(e) = session {
            eprintln!("mirante watch: {e}; reconnecting");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn run(cmd: Cmd) -> Res {
    let paths = Paths::from_env()?;
    let color = term::is_tty(1);
    match cmd {
        Cmd::Help => println!("{USAGE}"),
        Cmd::Daemon(DaemonCmd::Status) => match Client::connect(&paths.socket()) {
            Ok(mut c) => {
                let s: DaemonStatus = c.call(method::DAEMON_STATUS, json!({}))?;
                println!(
                    "running: pid {} boot {} live sessions {} (v{})",
                    s.pid, s.boot_id, s.live_sessions, s.version
                );
            }
            Err(_) => println!("not running"),
        },
        Cmd::Daemon(DaemonCmd::Start) => {
            if Client::connect(&paths.socket()).is_ok() {
                println!("already running");
            } else {
                daemon_ctl::start_daemon(&paths)?;
                println!("started");
            }
        }
        Cmd::Daemon(DaemonCmd::Stop) => match Client::connect(&paths.socket()) {
            Ok(mut c) => {
                c.set_timeout(Some(Duration::from_secs(60)))?;
                let _: Value = c.call(method::DAEMON_SHUTDOWN, json!({}))?;
                println!("stopped (sessions ended; mirante resume <task> restores them)");
            }
            Err(_) => println!("not running"),
        },
        Cmd::New { repo, agent, branch, base, title, goal, trust } => {
            let repo = std::fs::canonicalize(&repo).map_err(|e| format!("repo {repo}: {e}"))?;
            let mut c = client(&paths)?;
            let v: TaskView = c.call(
                method::TASK_CREATE,
                CreateTask {
                    repo: repo.to_string_lossy().into_owned(),
                    agent,
                    branch,
                    base,
                    goal: goal.clone(),
                    title,
                },
            )?;
            println!("created {} on {} ({})", render::short(&v.task.id), v.task.branch, v.task.worktree_path);
            let r: StartResult =
                c.call(method::SESSION_START, StartSession { task: v.task.id.clone(), prompt: Some(goal) })?;
            boot_flow(&mut c, r, trust)?;
        }
        Cmd::Ls { all } => {
            let tasks: Vec<TaskView> = client(&paths)?.call(method::TASK_LIST, ListParams { archived: all })?;
            print!("{}", render::tasks_table(&tasks, color));
        }
        Cmd::Show { task } => {
            let mut c = client(&paths)?;
            let v: TaskView = c.call(method::TASK_GET, TaskRef { task })?;
            let perms: Vec<PermissionView> = c.call(method::PERMISSION_LIST, json!({}))?;
            let mine: Vec<PermissionRequest> =
                perms.into_iter().filter(|p| p.request.task_id == v.task.id).map(|p| p.request).collect();
            print!("{}", render::task_detail(&v, &mine));
        }
        Cmd::Watch => watch(&paths)?,
        Cmd::Attach { task } => attach(&paths, &task)?,
        Cmd::Say { task, text, queue } => {
            let mode = if queue { SendMode::Queue } else { SendMode::Steer };
            let r: SendResult = client(&paths)?.call(method::MESSAGE_SEND, SendMessage { task, text, mode })?;
            match r.delivered {
                SendMode::Steer => println!("sent now"),
                SendMode::Queue => println!("queued (#{} in line, delivered when the turn ends)", r.queued),
            }
        }
        Cmd::Perms => {
            let perms: Vec<PermissionView> = client(&paths)?.call(method::PERMISSION_LIST, json!({}))?;
            print!("{}", render::perms_table(&perms));
        }
        Cmd::Answer { id, allow } => {
            let p: PermissionRequest =
                client(&paths)?.call(method::PERMISSION_ANSWER, AnswerPermission { id, allow })?;
            println!("{} {}: {}", render::short(&p.id), p.tool, p.status.as_str());
        }
        Cmd::Interrupt { task } => {
            let _: Value = client(&paths)?.call(method::SESSION_INTERRUPT, TaskRef { task })?;
            println!("interrupt sent");
        }
        Cmd::Stop { task } => {
            let v: TaskView = client(&paths)?.call(method::SESSION_STOP, TaskRef { task })?;
            println!("{} {}", render::short(&v.task.id), v.task.status.as_str());
        }
        Cmd::Resume { task, trust } => {
            let mut c = client(&paths)?;
            let r: StartResult = c.call(method::SESSION_RESUME, StartSession { task, prompt: None })?;
            if !r.resumed {
                println!("note: no native session id was recorded; this is a fresh conversation");
            }
            boot_flow(&mut c, r, trust)?;
        }
        Cmd::Archive { task } => {
            let v: TaskView = client(&paths)?.call(method::TASK_ARCHIVE, TaskRef { task })?;
            println!("{} archived (branch {} kept)", render::short(&v.task.id), v.task.branch);
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args::parse(&args) {
        Err(e) => {
            eprintln!("mirante: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Ok(cmd) => match run(cmd) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mirante: {e}");
                ExitCode::FAILURE
            }
        },
    }
}
