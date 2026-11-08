//! `mirante`: thin client of `mirante-daemon`.
use std::collections::HashMap;
use std::error::Error;
use std::io::{self, BufRead, Read, Write};
use std::net::Shutdown;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mirante_core::model::{PermissionRequest, Status};
use mirante_ipc::rpc::code;
use mirante_ipc::types::{
    method, AnswerPermission, AttachParams, AttachResult, BootState, CheckReport, CreateTask, DaemonStatus, DnaGet,
    DnaGraph, DnaNoteAction, DnaProposals, DnaPropose, DnaRef, DnaReview, DnaSearch, DnaStatus, DoctorReport,
    EditQueued, KillOrphan, ListParams, NoteAction, NoteSummary, NoteView, OrphanView, PermissionView, ProposalView,
    ProposeAnchor, QueueRef, QueuedMessage, QueuedRef, ReorderQueue, RepoOrigin, ResizeParams, SendMessage, SendMode,
    SendResult, SkillAction, SkillActionKind, SkillAdd, SkillEdits, SkillGet, SkillLearn, SkillListParams, SkillMine,
    SkillMined, SkillProposalView, SkillProposals, SkillRepoParams, SkillReview, SkillSummary, SkillView, Snapshot,
    StartResult, StartSession, SubscribeParams, SystemView, TaskRef, TaskView, TenantView, TrustAnswer,
};
use mirante_ipc::{signals, Client, ClientError, Paths};
use serde_json::{json, Value};

use mirante_cli::args::{self, Cmd, DaemonCmd, MemoryCmd, QueueCmd, SkillOp, SkillsCmd, USAGE};
use mirante_cli::{confirm, daemon_ctl, memcheck, render, skilllint, term};
use mirante_core::skills::lint::has_errors;

type Res = Result<(), Box<dyn Error>>;

/// What `memory check` found wrong (a stale or unverifiable anchor, a note that does not
/// parse): exit 1. Any other `memory check` error means it could not check: exit 3.
#[derive(Debug)]
struct Findings(String);

impl std::fmt::Display for Findings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Findings {}

/// `memory check` could not run (not a checkout, a bad `--rev`, the daemon unreachable, a
/// git failure).
const EXIT_CHECK_FAILED_TO_RUN: u8 = 3;

/// Ctrl-] detaches from `attach`.
const DETACH: u8 = 0x1d;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A progress line on stderr (daemon start-up).
fn notice(m: &str) {
    eprintln!("mirante: {m}");
}

fn client(paths: &Paths) -> Result<Client, Box<dyn Error>> {
    let mut c = daemon_ctl::connect_with_notice(paths, &notice)?;
    // `session.start` may wait up to 45 s for the agent to boot.
    c.set_timeout(Some(Duration::from_secs(90)))?;
    warn_on_version_skew(&mut c);
    Ok(c)
}

/// One stderr line when the daemon is another version than this CLI (best effort).
fn warn_on_version_skew(c: &mut Client) {
    if let Ok(s) = c.call::<DaemonStatus>(method::DAEMON_STATUS, json!({})) {
        if let Some(w) = daemon_ctl::version_warning(&s, VERSION) {
            eprintln!("{w}");
        }
    }
}

fn ask(question: &str) -> io::Result<bool> {
    print!("{question}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Mirante did not type the answer (Claude: inline answers off, or no evidence the dialog
/// is this card's): says so and, on a terminal, offers to attach to the task to answer it
/// there. Not answered is an error (non-zero exit) unless the user attached.
fn answer_in_terminal(paths: &Paths, c: &mut Client, id: &str, why: &str) -> Res {
    eprintln!("mirante: {why}");
    let perms: Vec<PermissionView> = c.call(method::PERMISSION_LIST, json!({}))?;
    let card = perms.into_iter().find(|p| p.request.id == id || (id.len() >= 4 && p.request.id.ends_with(id)));
    let Some(card) = card else {
        return Err("not answered".into());
    };
    let task = card.branch;
    if term::is_tty(0) && term::is_tty(1) && ask(&format!("attach to {task} now to answer it there? [y/N] "))? {
        return attach(paths, &task);
    }
    Err(format!("not answered: answer it in `mirante attach {task}`").into())
}

/// Walks the start result: asks the user about Claude's trust dialog (unless `--trust`
/// said yes up front), reports how the boot ended.
/// `mirante queue ...`: positions are 1-based, resolved to message ids from a fresh list
/// (the daemon refuses a message that left the queue meanwhile).
fn queue(paths: &Paths, q: QueueCmd) -> Res {
    let mut c = client(paths)?;
    let task = match &q {
        QueueCmd::Ls { task } | QueueCmd::Edit { task, .. } | QueueCmd::Rm { task, .. } | QueueCmd::Mv { task, .. } => {
            task.clone()
        }
    };
    let list: Vec<QueuedMessage> = c.call(method::MESSAGE_LIST_QUEUED, QueueRef { task_id: task.clone() })?;
    let id_at = |n: usize| {
        list.get(n - 1).map(|m| m.id.clone()).ok_or_else(|| format!("no queued message #{n} ({} queued)", list.len()))
    };
    let after: Vec<QueuedMessage> = match q {
        QueueCmd::Ls { .. } => list.clone(),
        QueueCmd::Edit { n, text, .. } => c.call(method::MESSAGE_EDIT, EditQueued { id: id_at(n)?, text })?,
        QueueCmd::Rm { n, .. } => c.call(method::MESSAGE_REMOVE, QueuedRef { id: id_at(n)? })?,
        QueueCmd::Mv { n, to, .. } => {
            let id = id_at(n)?;
            let mut ids: Vec<String> = list.iter().map(|m| m.id.clone()).filter(|i| *i != id).collect();
            ids.insert((to - 1).min(ids.len()), id);
            c.call(method::MESSAGE_REORDER, ReorderQueue { task_id: task, ids })?
        }
    };
    if after.is_empty() {
        println!("nothing queued");
    }
    for (i, m) in after.iter().enumerate() {
        println!("{:>3}  {}", i + 1, m.text);
    }
    Ok(())
}

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
            BootState::Exited if r.resumed => {
                return Err(format!(
                    "the agent exited right after resuming (its conversation may be gone):\n{}\n\
                     `mirante resume {id} --fresh` starts a new conversation",
                    r.screen
                )
                .into())
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

/// While attached: SIGWINCH sends the new size (`session.resize`); SIGTERM / SIGHUP /
/// SIGINT (raw mode turns Ctrl-C into a byte, so these come from outside) restore the
/// terminal and reset its input modes before exiting.
fn attach_signals(paths: &Paths, task: &str, saved: libc::termios) -> io::Result<()> {
    let sigs = signals::subscribe(&[signals::SIGWINCH, signals::SIGTERM, signals::SIGHUP, signals::SIGINT])?;
    let (paths, task) = (paths.clone(), task.to_string());
    std::thread::spawn(move || {
        let mut resize: Option<Client> = None;
        for sig in sigs {
            if sig != signals::SIGWINCH {
                term::restore(&saved);
                print!("{}\r\nmirante: terminated by signal {sig}\r\n", term::RESET_INPUT_MODES);
                let _ = io::stdout().flush();
                std::process::exit(128 + sig);
            }
            let (rows, cols) = term::size();
            if resize.is_none() {
                resize = Client::connect(&paths.socket()).ok();
                if let Some(c) = &resize {
                    let _ = c.set_timeout(Some(Duration::from_secs(5)));
                }
            }
            if let Some(c) = resize.as_mut() {
                let r = c.call::<Value>(method::SESSION_RESIZE, ResizeParams { task: task.clone(), rows, cols });
                // A typed refusal (e.g. Codex cannot resize) keeps the connection.
                if matches!(r, Err(mirante_ipc::ClientError::Io(_) | mirante_ipc::ClientError::Protocol(_))) {
                    resize = None;
                }
            }
        }
    });
    Ok(())
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
    // Handlers first: a signal arriving right after raw mode is on must still restore it.
    attach_signals(paths, task, term::get_attrs()?)?;
    let raw = term::RawMode::enable()?;
    let detached = Arc::new(AtomicBool::new(false));
    let mut w = writer.try_clone()?;
    let user_left = detached.clone();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = [0u8; 1024];
        loop {
            let n = match stdin.read(&mut buf) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
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
        // Ctrl-] or the end of our stdin: the user left (not the session).
        user_left.store(true, Ordering::SeqCst);
        let _ = w.shutdown(Shutdown::Both);
    });
    let mut out = io::stdout().lock();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Ok(0) | Err(_) => break,
            Ok(n) => {
                out.write_all(&buf[..n])?;
                out.flush()?;
            }
        }
    }
    drop(raw);
    // The agent's TUI may have left input modes on (mouse, bracketed paste, kitty keys...).
    let how = if detached.load(Ordering::SeqCst) { "detached" } else { "session ended" };
    write!(out, "{}\r\n{how}\r\n", term::RESET_INPUT_MODES)?;
    out.flush()?;
    Ok(())
}

fn board(tasks: &[TaskView], memory: Option<&SystemView>, seen: &mut HashMap<String, Status>, color: bool) {
    // Bell only when a task newly needs the user.
    let bell = tasks
        .iter()
        .any(|v| v.task.status == Status::NeedsYou && seen.get(&v.task.id).is_some_and(|s| *s != Status::NeedsYou));
    seen.clear();
    seen.extend(tasks.iter().map(|v| (v.task.id.clone(), v.task.status)));
    print!("\x1b[2J\x1b[H");
    println!("mirante watch  (Ctrl-C to quit)");
    match memory {
        Some(m) => println!("{}\n", render::memory_line(m)),
        None => println!(),
    }
    print!("{}", render::tasks_table(tasks, color));
    if bell {
        print!("\x07");
    }
    let _ = io::stdout().flush();
}

fn watch(paths: &Paths) -> Res {
    let color = term::is_tty(1);
    let mut seen = HashMap::new();
    // Where the stream left off: a reconnect resumes there (the daemon replays what was
    // missed, e.g. after it dropped this board for falling behind) instead of a snapshot.
    let mut resume = SubscribeParams::default();
    loop {
        // No auto-start here: after `mirante daemon stop` the board waits, it does not
        // bring the daemon back.
        let session = (|| -> Result<(), Box<dyn Error>> {
            let mut sub = Client::connect(&paths.socket())?;
            let snap: Snapshot = sub.call(method::EVENTS_SUBSCRIBE, &resume)?;
            let mut q = Client::connect(&paths.socket())?;
            q.set_timeout(Some(Duration::from_secs(10)))?;
            let tasks = if snap.resumed { q.call(method::TASK_LIST, ListParams::default())? } else { snap.tasks };
            let memory = |q: &mut Client| q.call::<DaemonStatus>(method::DAEMON_STATUS, json!({})).ok()?.memory;
            board(&tasks, memory(&mut q).as_ref(), &mut seen, color);
            resume = SubscribeParams { since_seq: Some(snap.seq), boot_id: Some(snap.boot_id) };
            // The stream is only a hint: every event triggers a fresh snapshot.
            while let Some(n) = sub.next_notification()? {
                if let (Some(seq), Some(boot)) = (n.params["seq"].as_u64(), n.params["boot_id"].as_str()) {
                    resume = SubscribeParams { since_seq: Some(seq), boot_id: Some(boot.to_string()) };
                }
                let tasks: Vec<TaskView> = q.call(method::TASK_LIST, ListParams::default())?;
                board(&tasks, memory(&mut q).as_ref(), &mut seen, color);
            }
            Ok(())
        })();
        if let Err(e) = session {
            eprintln!("mirante watch: {e}; reconnecting");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The repo a memory command names: `repo`, else the current directory (the daemon
/// resolves the checkout around it).
fn repo_or_cwd(repo: Option<String>) -> Result<String, Box<dyn Error>> {
    let p = match repo {
        Some(r) => std::path::PathBuf::from(r),
        None => std::env::current_dir()?,
    };
    let canonical = std::fs::canonicalize(&p).map_err(|e| format!("repo {}: {e}", p.display()))?;
    Ok(canonical.to_string_lossy().into_owned())
}

/// Asks the daemon whether this process may make a person's memory decision, before
/// anything is shown or asked: the daemon is the boundary (it refuses Mirante-launched
/// agents, including a process on an agent's terminal), the typed word only an intent check.
fn may_review(c: &mut Client) -> Res {
    c.call::<Value>(method::DNA_MAY_REVIEW, json!({}))?;
    Ok(())
}

/// `mirante memory accept|reject`: the typed confirmation happens on the controlling
/// terminal, which is checked before the daemon is contacted; the daemon refuses
/// Mirante-launched agents on its side.
fn memory_review(paths: &Paths, id: String, accept: bool, title: Option<String>, note: Option<String>) -> Res {
    let mut tty = confirm::DevTty::open()?;
    let mut c = client(paths)?;
    may_review(&mut c)?;
    let query = DnaProposals { id: Some(id.clone()), status: Some("all".into()), ..Default::default() };
    let found: Vec<ProposalView> = c.call(method::DNA_PROPOSALS, query)?;
    let p = found.into_iter().next().ok_or_else(|| format!("no proposal {id}"))?;
    let word = if accept { "accept" } else { "reject" };
    confirm::typed(&mut tty, &render::proposal(&p, title.as_deref()), word)?;
    let review = DnaReview { id, accept, title, body: None, note };
    let d: ProposalView = c.call(method::DNA_REVIEW, review)?;
    match d.slug {
        Some(slug) => println!("accepted: .mirante/memory/{slug}.md written in {} (not committed)", d.repo),
        None => println!("rejected {}", render::short(&d.id)),
    }
    Ok(())
}

/// `mirante memory confirm|retire`: like accept, a person's decision typed on the
/// controlling terminal (checked before the daemon is contacted). The action carries the
/// hash of the note the person was shown, so a file changed meanwhile is refused.
fn memory_note_action(paths: &Paths, slug: String, repo: Option<String>, confirm: bool) -> Res {
    let mut tty = confirm::DevTty::open()?;
    let repo = repo_or_cwd(repo)?;
    let mut c = client(paths)?;
    may_review(&mut c)?;
    let n: NoteView = c.call(method::DNA_GET, DnaGet { at: DnaRef { repo: Some(repo.clone()), task: None }, slug })?;
    let (word, action) = if confirm { ("confirm", NoteAction::Confirm) } else { ("retire", NoteAction::Retire) };
    confirm::typed(&mut tty, &render::note_view(&n), word)?;
    let slug = n.summary.slug.clone();
    let a = DnaNoteAction { repo, slug: slug.clone(), action, expect_hash: n.content_hash };
    let v: NoteView = c.call(method::DNA_NOTE_ACTION, a)?;
    if confirm {
        println!("confirmed {slug}: re-anchored at HEAD and marked reviewed ({}; not committed)", v.path);
    } else {
        println!("retired {slug}: the file stays, with status: retired ({}; not committed)", v.path);
    }
    Ok(())
}

/// `mirante memory check`: one line per anchor; a stale or unverifiable anchor (or a note
/// that does not parse) is a [`Findings`] error (exit 1), so the exit status fails a CI
/// job; failing to check at all exits 3 (so does a shallow clone: fetch full history).
fn memory_check(paths: &Paths, repo: Option<String>, rev: Option<String>, offline: bool) -> Res {
    if offline {
        let dir = match repo {
            Some(r) => std::path::PathBuf::from(r),
            None => std::env::current_dir()?,
        };
        let top = memcheck::toplevel(&dir)?;
        let (r, sha) = memcheck::check(&top, rev.as_deref())?;
        for l in &r.lines {
            println!("{l}");
        }
        let summary = r.summary(&sha[..sha.len().min(12)]);
        if r.failed() {
            return Err(Box::new(Findings(format!("memory check failed: {summary}"))));
        }
        println!("{summary}");
        return Ok(());
    }
    let repo = repo_or_cwd(repo)?;
    // A shallow clone cannot be checked (every cited commit reads as gone): say so, exit 3.
    memcheck::refuse_shallow(std::path::Path::new(&repo))?;
    let mut c = client(paths)?;
    let at = DnaRef { repo: Some(repo), task: None };
    let report: CheckReport = c.call(method::DNA_CHECK, &at)?;
    let graph: DnaGraph = c.call(method::DNA_GRAPH, &at)?;
    let (mut stale, mut unverifiable, mut anchors) = (0, 0, 0);
    for bad in &report.invalid {
        println!("{}", render::invalid_line(&bad.file, &bad.error));
    }
    for n in graph.nodes.iter().filter(|n| n.status == "active") {
        for a in &n.anchors {
            anchors += 1;
            match a.state.as_str() {
                "stale" => stale += 1,
                "unverifiable" => unverifiable += 1,
                _ => {}
            }
            println!(
                "{}",
                render::check_line(&a.state, &n.slug, &a.path, a.start, a.end, render::anchor_reason(a).as_deref())
            );
        }
    }
    let head = report.head.as_deref().map(|h| &h[..h.len().min(12)]).unwrap_or("HEAD");
    let summary = format!(
        "{} notes, {anchors} anchors checked at {head}: {stale} stale, {unverifiable} unverifiable, {} invalid",
        report.notes,
        report.invalid.len()
    );
    if stale + unverifiable + report.invalid.len() > 0 {
        return Err(Box::new(Findings(format!("memory check failed: {summary}"))));
    }
    println!("{summary}");
    Ok(())
}

fn memory(paths: &Paths, m: MemoryCmd) -> Res {
    match m {
        MemoryCmd::Review { id, accept, title, note } => memory_review(paths, id, accept, title, note),
        MemoryCmd::NoteAction { slug, repo, confirm } => memory_note_action(paths, slug, repo, confirm),
        MemoryCmd::Check { repo, rev, offline } => memory_check(paths, repo, rev, offline),
        MemoryCmd::Enable { repo, on } => {
            let repo = repo_or_cwd(repo)?;
            let at = DnaRef { repo: Some(repo), task: None };
            let st: DnaStatus = client(paths)?.call(if on { method::DNA_ENABLE } else { method::DNA_DISABLE }, at)?;
            if st.enabled {
                println!(
                    "memory enabled for {}: new sessions of its tasks get the memory tools ({} notes in .mirante/memory/)",
                    st.repo, st.notes
                );
            } else {
                println!("memory disabled for {}: new sessions start without it; the notes stay", st.repo);
            }
            Ok(())
        }
        MemoryCmd::Ls { repo } => {
            let repo = repo_or_cwd(repo)?;
            let g: DnaGraph =
                client(paths)?.call(method::DNA_GRAPH, DnaRef { repo: Some(repo.clone()), task: None })?;
            if g.nodes.is_empty() {
                println!("no notes in {repo}/.mirante/memory");
            } else {
                print!("{}", render::notes_table(&g.nodes));
            }
            Ok(())
        }
        MemoryCmd::Show { slug, repo } => {
            let at = DnaRef { repo: Some(repo_or_cwd(repo)?), task: None };
            let n: NoteView = client(paths)?.call(method::DNA_GET, DnaGet { at, slug })?;
            print!("{}", render::note_view(&n));
            Ok(())
        }
        MemoryCmd::Search { query, repo } => {
            let at = DnaRef { repo: Some(repo_or_cwd(repo)?), task: None };
            let hits: Vec<NoteSummary> =
                client(paths)?.call(method::DNA_SEARCH, DnaSearch { at, query, limit: Some(25) })?;
            if hits.is_empty() {
                println!("no notes matched");
            } else {
                print!("{}", render::notes_table(&hits));
            }
            Ok(())
        }
        MemoryCmd::Proposals { repo, all } => {
            let repo = match repo {
                Some(r) => Some(repo_or_cwd(Some(r))?),
                None => None,
            };
            let mut c = client(paths)?;
            let mut list: Vec<ProposalView> = Vec::new();
            loop {
                let q = DnaProposals {
                    repo: repo.clone(),
                    status: all.then(|| "all".to_string()),
                    after: list.last().map(|p| p.id.clone()),
                    limit: Some(200),
                    ..Default::default()
                };
                let page: Vec<ProposalView> = c.call(method::DNA_PROPOSALS, q)?;
                let more = page.len() == 200;
                list.extend(page);
                if !more {
                    break;
                }
            }
            if list.is_empty() {
                println!("{}", if all { "no proposals" } else { "no pending proposals" });
            } else {
                print!("{}", render::proposals_table(&list));
            }
            Ok(())
        }
        MemoryCmd::Propose { task, title, kind, cites, body } => {
            let anchors = cites
                .into_iter()
                .map(|c| ProposeAnchor { path: c.path, symbol: c.symbol, start: c.start, end: c.end })
                .collect();
            let p: ProposalView =
                client(paths)?.call(method::DNA_PROPOSE, DnaPropose { task, title, kind, body, anchors })?;
            let redacted = match p.redactions {
                0 => String::new(),
                1 => " (1 secret redacted)".to_string(),
                n => format!(" ({n} secrets redacted)"),
            };
            println!("proposal {} waiting for review{redacted}: mirante memory accept|reject {}", p.id, p.id);
            Ok(())
        }
    }
}

/// `skills.may_review`: whether this process may make a person's skill decision, asked
/// before anything is shown (the daemon is the boundary; the typed word only an intent check).
fn may_review_skills(c: &mut Client) -> Res {
    c.call::<Value>(method::SKILL_MAY_REVIEW, json!({}))?;
    Ok(())
}

/// The checkout's origins as the daemon reads and pinned them (normalized: no token or
/// alias), and the counts of the tenant `learn` and `mine` act on (the pinned one).
fn repo_origin(c: &mut Client, repo: Option<String>) -> Result<(RepoOrigin, Option<TenantView>), Box<dyn Error>> {
    let repo = repo_or_cwd(repo)?;
    let o: RepoOrigin = c.call(method::SKILL_REPO_ORIGIN, SkillRepoParams { repo })?;
    let tenants: Vec<TenantView> = c.call(method::SKILL_TENANTS, json!({}))?;
    let t = tenants.into_iter().find(|t| t.tenant == render::origin_tenant(&o));
    Ok((o, t))
}

/// `mirante skills lint` found an error: exit 1 (any other failure of lint exits 3).
#[derive(Debug)]
struct LintErrors(String);

impl std::fmt::Display for LintErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for LintErrors {}

/// `mirante skills accept|reject`: like memory's, typed on the controlling terminal after
/// the daemon said this process may review.
fn skill_review(
    paths: &Paths,
    id: String,
    accept: bool,
    scope: Option<String>,
    activation: Option<String>,
    note: Option<String>,
) -> Res {
    let mut tty = confirm::DevTty::open()?;
    let mut c = client(paths)?;
    may_review_skills(&mut c)?;
    let q = SkillProposals { id: Some(id.clone()), status: Some("all".into()), ..Default::default() };
    let found: Vec<SkillProposalView> = c.call(method::SKILL_PROPOSALS, q)?;
    let p = found.into_iter().next().ok_or_else(|| format!("no skill proposal {id}"))?;
    let word = if accept { "accept" } else { "reject" };
    confirm::typed(&mut tty, &render::skill_proposal(&p, scope.as_deref(), activation.as_deref()), word)?;
    let edits =
        (scope.is_some() || activation.is_some()).then(|| SkillEdits { scope, activation, ..Default::default() });
    let d: SkillProposalView = c.call(method::SKILL_REVIEW, SkillReview { id: p.proposal.id, accept, edits, note })?;
    match (d.proposal.status.as_str(), d.proposal.version) {
        ("accepted", Some(v)) => println!(
            "accepted: {} v{v} written and signed in the skill library ({})",
            mirante_core::dna::untrusted::line(&d.proposal.name, 64),
            d.proposal.scope
        ),
        _ => println!("rejected {}", render::short(&d.proposal.id)),
    }
    Ok(())
}

/// `mirante skills retire|restore|rescope|rollback`: typed like accept; the action carries
/// the hash of the version the person was shown.
fn skill_action(paths: &Paths, name: String, op: SkillOp) -> Res {
    let mut tty = confirm::DevTty::open()?;
    let mut c = client(paths)?;
    may_review_skills(&mut c)?;
    let v: SkillView = c.call(method::SKILL_GET, SkillGet { name: name.clone(), version: None })?;
    let (word, action, scope, version) = match op {
        SkillOp::Retire => ("retire", SkillActionKind::Retire, None, None),
        SkillOp::Restore => ("restore", SkillActionKind::Restore, None, None),
        SkillOp::Rescope(s) => ("rescope", SkillActionKind::Rescope, Some(s), None),
        SkillOp::Rollback(n) => ("rollback", SkillActionKind::Rollback, None, Some(n)),
    };
    let mut summary = render::skill_view(&v);
    if let Some(s) = &scope {
        summary.push_str(&format!("\nnew scope: {}\n", mirante_core::dna::untrusted::line(s, 200)));
    }
    if let Some(n) = version {
        summary.push_str(&format!("\nroll back to the content of v{n} (as a new version)\n"));
    }
    confirm::typed(&mut tty, &summary, word)?;
    let a = SkillAction { name, action, scope, version, expect_hash: v.summary.manifest_hash };
    let after: SkillView = c.call(method::SKILL_ACTION, a)?;
    let k = &after.summary;
    let done = match word {
        "retire" => "retired",
        "restore" => "restored",
        "rescope" => "rescoped",
        _ => "rolled back",
    };
    println!("{done} {}: v{} {} {}", k.name, k.version, k.scope, k.status);
    Ok(())
}

fn skills(paths: &Paths, k: SkillsCmd) -> Res {
    match k {
        SkillsCmd::Lint { file } => {
            let findings = skilllint::lint_file(std::path::Path::new(&file))?;
            for f in &findings {
                println!("{}", skilllint::finding_line(f));
            }
            if has_errors(&findings) {
                let n = findings.iter().filter(|f| f.severity == mirante_core::skills::lint::Severity::Error).count();
                return Err(Box::new(LintErrors(format!("{n} lint error(s): a person could not accept this skill"))));
            }
            if findings.is_empty() {
                println!("no findings");
            }
            Ok(())
        }
        SkillsCmd::Tenant { repo } => {
            let (o, t) = repo_origin(&mut client(paths)?, repo)?;
            print!("{}", render::origin_summary(&o, t.as_ref()));
            Ok(())
        }
        SkillsCmd::Learn { on, repo } => {
            let mut tty = confirm::DevTty::open()?;
            let mut c = client(paths)?;
            may_review_skills(&mut c)?;
            let (o, t) = repo_origin(&mut c, repo)?;
            if let Some(why) = render::origin_mismatch(&o) {
                return Err(why.into());
            }
            let tenant = render::origin_tenant(&o).to_string();
            let head = render::origin_summary(&o, t.as_ref());
            let summary = if on {
                format!(
                    "{head}\nTurn skill learning on for {tenant}.\nMirante reads your goals, messages and denied \
permissions in this client's tasks, on this machine only, to propose skills for your review. Agents may propose \
skills for their own task (skill_propose)."
                )
            } else {
                format!("{head}\nTurn skill learning off for {tenant}. Accepted skills stay and still apply.")
            };
            let word = if on { "enable" } else { "disable" };
            confirm::typed(&mut tty, &summary, word)?;
            let t: TenantView = c.call(method::SKILL_LEARN, SkillLearn { tenant, enabled: on })?;
            println!("skill learning {} for {}", if t.learning { "on" } else { "off" }, t.tenant);
            Ok(())
        }
        SkillsCmd::Ls => {
            let list: Vec<SkillSummary> = client(paths)?.call(method::SKILL_LIST, SkillListParams::default())?;
            if list.is_empty() {
                println!("no skills yet");
            } else {
                print!("{}", render::skills_table(&list));
            }
            Ok(())
        }
        SkillsCmd::Show { name, version } => {
            let v: SkillView = client(paths)?.call(method::SKILL_GET, SkillGet { name, version })?;
            print!("{}", render::skill_view(&v));
            Ok(())
        }
        SkillsCmd::Proposals { all } => {
            let mut c = client(paths)?;
            let mut list: Vec<SkillProposalView> = Vec::new();
            loop {
                let q = SkillProposals {
                    status: all.then(|| "all".to_string()),
                    after: list.last().map(|p| p.proposal.id.clone()),
                    limit: Some(200),
                    ..Default::default()
                };
                let page: Vec<SkillProposalView> = c.call(method::SKILL_PROPOSALS, q)?;
                let more = page.len() == 200;
                list.extend(page);
                if !more {
                    break;
                }
            }
            if list.is_empty() {
                println!("{}", if all { "no skill proposals" } else { "no pending proposals" });
            } else {
                print!("{}", render::skill_proposals_table(&list));
            }
            Ok(())
        }
        SkillsCmd::Review { id, accept, scope, activation, note } => {
            skill_review(paths, id, accept, scope, activation, note)
        }
        SkillsCmd::Action { name, op } => skill_action(paths, name, op),
        SkillsCmd::Mine { repo } => {
            let mut c = client(paths)?;
            let (o, _) = repo_origin(&mut c, repo)?;
            if o.pinned.is_none() {
                return Err(format!(
                    "no task was created in this checkout yet: nothing to mine, and its origin ({}) is not pinned",
                    o.live
                )
                .into());
            }
            if let Some(why) = render::origin_mismatch(&o) {
                return Err(why.into());
            }
            let tenant = render::origin_tenant(&o).to_string();
            let m: SkillMined = c.call(method::SKILL_MINE, SkillMine { tenant })?;
            println!(
                "{}: {} new proposal(s), {} updated, {} skipped (mirante skills proposals)",
                m.tenant, m.proposed, m.updated, m.skipped
            );
            Ok(())
        }
        SkillsCmd::Add { dir, scope, activation } => {
            let dir = std::fs::canonicalize(&dir).map_err(|e| format!("{dir}: {e}"))?;
            let add = SkillAdd { dir: Some(dir.to_string_lossy().into_owned()), inline: None, scope, activation };
            let p: SkillProposalView = client(paths)?.call(method::SKILL_ADD, add)?;
            let errors =
                p.proposal.lint.iter().filter(|f| f.severity == mirante_core::skills::lint::Severity::Error).count();
            let lint = if errors > 0 {
                format!(" ({errors} lint error(s): fix them before accepting)")
            } else {
                String::new()
            };
            println!(
                "proposal {} waiting for review{lint}: mirante skills accept|reject {}",
                p.proposal.id, p.proposal.id
            );
            Ok(())
        }
    }
}

/// What `daemon stop` says about the sessions it ends: auto-resume brings them back when the
/// daemon starts again (the running daemon's setting), else `mirante resume`.
fn stop_note(c: &mut Client) -> &'static str {
    let auto = c.call::<DaemonStatus>(method::DAEMON_STATUS, json!({})).ok().and_then(|s| s.auto_resume);
    if auto == Some(true) {
        "sessions ended; they resume when the daemon starts again (auto-resume)"
    } else {
        "sessions ended; mirante resume <task> restores them"
    }
}

fn run(cmd: Cmd) -> Res {
    let paths = Paths::from_env()?;
    let color = term::is_tty(1);
    match cmd {
        Cmd::Help => println!("{USAGE}"),
        Cmd::Daemon(DaemonCmd::Status) => match Client::connect(&paths.socket()) {
            // A socket path that cannot exist is a setup error, not "not running".
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => return Err(e.into()),
            Ok(mut c) => {
                let s: DaemonStatus = c.call(method::DAEMON_STATUS, json!({}))?;
                println!(
                    "running: pid {} boot {} live sessions {} (v{})",
                    s.pid, s.boot_id, s.live_sessions, s.version
                );
                if let Some(w) = daemon_ctl::version_warning(&s, VERSION) {
                    eprintln!("{w}");
                }
            }
            Err(_) => println!("not running"),
        },
        Cmd::Daemon(DaemonCmd::Start) => {
            if Client::connect(&paths.socket()).is_ok() {
                println!("already running");
            } else {
                daemon_ctl::start_daemon_with_notice(&paths, &notice)?;
                println!("started");
            }
        }
        Cmd::Daemon(DaemonCmd::Stop) => match Client::connect(&paths.socket()) {
            Ok(mut c) => {
                c.set_timeout(Some(Duration::from_secs(60)))?;
                let back = stop_note(&mut c);
                let _: Value = c.call(method::DAEMON_SHUTDOWN, json!({}))?;
                println!("stopped ({back})");
            }
            Err(_) => println!("not running"),
        },
        Cmd::Daemon(DaemonCmd::Restart) => {
            if let Ok(mut c) = Client::connect(&paths.socket()) {
                c.set_timeout(Some(Duration::from_secs(60)))?;
                let back = stop_note(&mut c);
                let _: Value = c.call(method::DAEMON_SHUTDOWN, json!({}))?;
                daemon_ctl::wait_stopped(&paths, Duration::from_secs(10))?;
                println!("stopped ({back})");
            }
            let mut c = daemon_ctl::start_daemon_with_notice(&paths, &notice)?;
            c.set_timeout(Some(Duration::from_secs(10)))?;
            let s: DaemonStatus = c.call(method::DAEMON_STATUS, json!({}))?;
            println!("started: pid {} (v{})", s.pid, s.version);
        }
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
        Cmd::Queue(q) => queue(&paths, q)?,
        Cmd::Perms => {
            let perms: Vec<PermissionView> = client(&paths)?.call(method::PERMISSION_LIST, json!({}))?;
            print!("{}", render::perms_table(&perms));
        }
        Cmd::Answer { id, allow } => {
            let mut c = client(&paths)?;
            match c.call::<PermissionRequest>(method::PERMISSION_ANSWER, AnswerPermission { id: id.clone(), allow }) {
                Ok(p) => println!("{} {}: {}", render::short(&p.id), p.tool, p.status.as_str()),
                Err(ClientError::Rpc(e)) if e.code == code::ANSWER_IN_TERMINAL => {
                    return answer_in_terminal(&paths, &mut c, &id, &e.message);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Cmd::Interrupt { task } => {
            let _: Value = client(&paths)?.call(method::SESSION_INTERRUPT, TaskRef { task })?;
            println!("interrupt sent");
        }
        Cmd::Stop { task } => {
            let v: TaskView = client(&paths)?.call(method::SESSION_STOP, TaskRef { task })?;
            println!("{} {}", render::short(&v.task.id), v.task.status.as_str());
        }
        Cmd::Resume { task, trust, fresh: true } => {
            let mut c = client(&paths)?;
            let r: StartResult = c.call(method::SESSION_START, StartSession { task, prompt: None })?;
            println!("starting a fresh conversation (the earlier one is not resumed)");
            boot_flow(&mut c, r, trust)?;
        }
        Cmd::Resume { task, trust, fresh: false } => {
            let mut c = client(&paths)?;
            let r: StartResult = c.call(method::SESSION_RESUME, StartSession { task, prompt: None })?;
            if !r.resumed {
                println!("note: no native session id was recorded; this is a fresh conversation");
            }
            boot_flow(&mut c, r, trust)?;
        }
        Cmd::Hibernate { task } => {
            let v: TaskView = client(&paths)?.call(method::SESSION_HIBERNATE, TaskRef { task })?;
            println!("{} hibernated (mirante wake {} resumes it)", render::short(&v.task.id), v.task.branch);
        }
        Cmd::Wake { task, trust } => {
            let mut c = client(&paths)?;
            let r: StartResult = c.call(method::SESSION_WAKE, TaskRef { task })?;
            boot_flow(&mut c, r, trust)?;
        }
        Cmd::Doctor { kill: None } => {
            let d: DoctorReport = client(&paths)?.call(method::DAEMON_DOCTOR, json!({}))?;
            print!("{}", render::doctor_report(&d));
        }
        Cmd::Doctor { kill: Some(pid) } => {
            let mut c = client(&paths)?;
            let d: DoctorReport = c.call(method::DAEMON_DOCTOR, json!({}))?;
            let o = d.orphan_processes.into_iter().find(|o| o.pid == pid).ok_or_else(|| {
                format!("pid {pid} is not an orphan in Mirante's worktrees (mirante doctor lists them)")
            })?;
            if !ask(&format!("kill pid {} ({})? [y/N] ", o.pid, o.argv))? {
                println!("left running");
                return Ok(());
            }
            let k: OrphanView = c.call(method::DAEMON_KILL_ORPHAN, KillOrphan { pid: o.pid, start: o.start })?;
            println!("killed pid {}", k.pid);
        }
        Cmd::Memory(m) => memory(&paths, m)?,
        Cmd::Skills(k) => skills(&paths, k)?,
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
        Ok(cmd) => {
            let check = matches!(cmd, Cmd::Memory(MemoryCmd::Check { .. }) | Cmd::Skills(SkillsCmd::Lint { .. }));
            match run(cmd) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("mirante: {e}");
                    if check && !e.is::<Findings>() && !e.is::<LintErrors>() {
                        ExitCode::from(EXIT_CHECK_FAILED_TO_RUN)
                    } else {
                        ExitCode::FAILURE
                    }
                }
            }
        }
    }
}
