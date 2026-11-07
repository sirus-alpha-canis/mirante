//! Drives interactive `claude` in a PTY with scoped hooks.
//! steer:  sends a 2nd message while the 1st turn runs; classifies via hook timing (S1 + S3).
//! resume: sets a secret, exits, resumes by session id and asks for the secret (S4).
//! status: scripted session(s) in a fresh sandbox; compares hook-derived status with the
//!         status read off the emulated screen at checkpoints (S3 accuracy).
//! prompts: types prompts one by one (default: the three S2 prompts), waiting for each Stop
//!         (a prompt starting with `/` gets a fixed 8s instead: local commands fire no Stop),
//!         then /exit; prints session id + transcript for `spike usage` (S2 interactive).
//!
//! `MIRANTE_CLAUDE_BIN` overrides the binary (default `claude` on PATH), e.g. to skip a
//! terminal-app wrapper shim that would inject its own hooks.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use spike_kit::{delivery, hook, screen, status, transcript, util};

const FIRST: &str = "Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma chamada por vez, com N de 1 a 5. Ao terminar, diga FIM.";
const SECOND: &str = "Mudança de planos: pare o que está fazendo e responda apenas a palavra BANANA.";
const SECRET_SET: &str = "A palavra secreta é JABUTICABA. Responda apenas OK.";
const SECRET_ASK: &str = "Qual é a palavra secreta? Responda só a palavra.";

const ROWS: u16 = 40;
const COLS: u16 = 120;

struct Session {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    screen: Arc<Mutex<String>>,
    /// Emulated terminal grid: what a human would see right now.
    term: Arc<Mutex<vt100::Parser>>,
}

impl Session {
    fn start(cwd: &str, settings: &str, hooks_log: &Path, task: &str, extra: &[&str]) -> Result<Session> {
        let pair = native_pty_system().openpty(PtySize { rows: ROWS, cols: COLS, pixel_width: 0, pixel_height: 0 })?;
        let mut cmd = CommandBuilder::new(util::claude_bin());
        for a in extra {
            cmd.arg(a);
        }
        cmd.arg("--settings");
        cmd.arg(settings);
        // Pin the permission mode: a user-level defaultMode (e.g. "auto") must not
        // widen what the spike may run beyond the settings allowlist.
        cmd.arg("--permission-mode");
        cmd.arg("manual");
        cmd.cwd(cwd);
        // Nested-session and terminal-wrapper (cmux) variables would pollute the measurement.
        for k in util::env_to_scrub() {
            cmd.env_remove(k);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("MIRANTE_TASK_ID", task);
        cmd.env("MIRANTE_HOOK_LOG", hooks_log);
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pair.master.take_writer()?));
        let screen = Arc::new(Mutex::new(String::new()));
        let term = Arc::new(Mutex::new(vt100::Parser::new(ROWS.try_into()?, COLS.try_into()?, 0)));
        let (w2, s2, t2) = (writer.clone(), screen.clone(), term.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                // Answer cursor-position queries so the TUI does not stall.
                if chunk.contains("\x1b[6n") {
                    if let Ok(mut w) = w2.lock() {
                        let _ = w.write_all(b"\x1b[1;1R");
                    }
                }
                if let Ok(mut s) = s2.lock() {
                    s.push_str(&chunk);
                }
                if let Ok(mut t) = t2.lock() {
                    t.process(&buf[..n]);
                }
            }
        });
        Ok(Session { _master: pair.master, child, writer, screen, term })
    }

    fn type_line(&self, text: &str) -> Result<()> {
        {
            let mut w = self.writer.lock().unwrap();
            w.write_all(text.as_bytes())?;
            w.flush()?;
        }
        std::thread::sleep(Duration::from_millis(400));
        let mut w = self.writer.lock().unwrap();
        w.write_all(b"\r")?;
        w.flush()?;
        Ok(())
    }

    fn key(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    /// What is on the emulated terminal right now.
    fn grid(&self) -> String {
        self.term.lock().unwrap().screen().contents()
    }

    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Polls the grid until `pred` holds; false (and a TIMEOUT line) on timeout.
    fn wait_screen(&mut self, what: &str, timeout: Duration, pred: impl Fn(&str) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if pred(&self.grid()) {
                return true;
            }
            if self.exited() {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        eprintln!("TIMEOUT waiting for {what}");
        false
    }

    fn boot(&mut self) -> Result<()> {
        std::thread::sleep(Duration::from_secs(6));
        if self.screen.lock().unwrap().contains("trust") {
            // The trust dialog defaults to "No, exit": move down to "Yes, I trust this folder".
            {
                let mut w = self.writer.lock().unwrap();
                w.write_all(b"\x1b[B")?;
                w.flush()?;
            }
            std::thread::sleep(Duration::from_millis(500));
            self.type_line("")?;
            std::thread::sleep(Duration::from_secs(3));
        }
        if let Some(status) = self.child.try_wait()? {
            let screen = self.screen.lock().unwrap();
            let tail: String = screen.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
            bail!("claude exited during boot ({status:?}); screen tail:\n{tail}");
        }
        Ok(())
    }

    /// Sends `/exit`, gives claude up to 3s to quit on its own, kills it only on timeout.
    fn finish(mut self) -> String {
        let _ = self.type_line("/exit");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // Drop kills + reaps if it is still running.
        let screen = self.screen.lock().unwrap().clone();
        screen
    }
}

impl Drop for Session {
    /// Never leave a claude process behind, also when the driver bails with an error.
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn wait_until(
    hooks_log: &Path,
    task: &str,
    what: &str,
    timeout: Duration,
    done: impl Fn(&[hook::LoggedEvent]) -> bool,
) -> Result<Vec<hook::LoggedEvent>> {
    let start = Instant::now();
    loop {
        let events = if hooks_log.exists() {
            hook::load_events(&util::read_jsonl(hooks_log)?, Some(task))
        } else {
            Vec::new()
        };
        if done(&events) {
            return Ok(events);
        }
        if start.elapsed() > timeout {
            eprintln!("TIMEOUT waiting for {what}");
            return Ok(events);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn wait_for(hooks_log: &Path, task: &str, event: &str, count: usize, timeout: Duration) -> Result<Vec<hook::LoggedEvent>> {
    wait_until(hooks_log, task, &format!("{count}x {event}"), timeout, |events| {
        events.iter().filter(|e| e.event.hook_event_name == event).count() >= count
    })
}

/// The turn that handled the 2nd prompt ended: a Stop after the 2nd UserPromptSubmit.
/// When steering merges both prompts into one turn there is only ONE Stop in total.
fn second_prompt_handled(events: &[hook::LoggedEvent]) -> bool {
    let Some(second) = events.iter().filter(|e| e.event.hook_event_name == "UserPromptSubmit").nth(1) else {
        return false;
    };
    events.iter().any(|e| e.event.hook_event_name == "Stop" && e.ts_ms > second.ts_ms)
}

fn print_timeline(events: &[hook::LoggedEvent]) {
    if events.is_empty() {
        println!("0 events for that task id");
    }
    for t in status::timeline(events) {
        println!("{:>14}  {:<18} -> {:?}", t.ts_ms, t.event, t.status);
    }
}

fn screen_path(hooks_log: &Path, task: &str) -> PathBuf {
    hooks_log.with_extension(format!("{task}.screen.txt"))
}

fn steer(cwd: &str, settings: &str, hooks_log: &Path, delay: u64) -> Result<()> {
    let task = format!("steer-{}", util::now_ms());
    let mut s = Session::start(cwd, settings, hooks_log, &task, &[])?;
    s.boot()?;
    s.type_line(FIRST)?;
    std::thread::sleep(Duration::from_secs(delay));
    s.type_line(SECOND)?;
    let events = wait_until(hooks_log, &task, "Stop after the 2nd prompt", Duration::from_secs(240), second_prompt_handled)?;
    let screen = s.finish();
    let out = screen_path(hooks_log, &task);
    std::fs::write(&out, &screen)?;
    println!("task: {task}  screen: {}", out.display());
    print_timeline(&events);
    println!("mid-run delivery (pty+hooks): {:?}", delivery::classify_hooks(&events));
    match events.iter().rev().find_map(|e| e.event.transcript_path.clone()) {
        Some(path) => {
            let lines = util::read_jsonl(Path::new(&path))?;
            println!("transcript: {path}");
            println!("mid-run delivery (transcript): {:?}", delivery::classify_transcript(&lines));
        }
        None => println!("mid-run delivery (transcript): no transcript_path in hook payloads"),
    }
    Ok(())
}

fn resume(cwd: &str, settings: &str, hooks_log: &Path) -> Result<()> {
    let task = format!("resume-{}", util::now_ms());
    let mut s = Session::start(cwd, settings, hooks_log, &task, &[])?;
    s.boot()?;
    s.type_line(SECRET_SET)?;
    let events = wait_for(hooks_log, &task, "Stop", 1, Duration::from_secs(120))?;
    let sid = events
        .iter()
        .find(|e| e.event.hook_event_name == "SessionStart")
        .and_then(|e| e.event.session_id.clone())
        .context("no SessionStart with session_id: hooks are not firing")?;
    let screen1 = s.finish(); // simulates closing the app
    std::fs::write(hooks_log.with_extension(format!("{task}.first.screen.txt")), &screen1)?;

    let mut s2 = Session::start(cwd, settings, hooks_log, &task, &["--resume", sid.as_str()])?;
    s2.boot()?;
    s2.type_line(SECRET_ASK)?;
    let events = wait_for(hooks_log, &task, "Stop", 2, Duration::from_secs(120))?;
    let screen = s2.finish();
    let out = screen_path(hooks_log, &task);
    std::fs::write(&out, &screen)?;

    let resumed = events
        .iter()
        .any(|e| e.event.hook_event_name == "SessionStart" && e.event.source.as_deref() == Some("resume"));
    let transcript_path = events
        .iter()
        .rev()
        .find_map(|e| e.event.transcript_path.clone())
        .context("no transcript_path in hook payloads")?;
    let answer = transcript::last_assistant_text(&util::read_jsonl(Path::new(&transcript_path))?).unwrap_or_default();
    println!("task: {task}  screen: {}", out.display());
    print_timeline(&events);
    println!("session id: {sid}");
    println!("SessionStart source=resume seen: {resumed}");
    println!("answer after resume: {answer:?}");
    println!("S4 {}", if resumed && answer.contains("JABUTICABA") { "PASS" } else { "FAIL" });
    Ok(())
}

// ---------------------------------------------------------------- S2 prompts

const S2_PROMPTS: [&str; 3] = [
    "Liste os arquivos e diga quantos são.",
    "Crie hello.txt com o texto oi.",
    "Execute `sleep 1 && echo ok` e diga o resultado.",
];

fn prompts(cwd: &str, settings: &str, hooks_log: &Path, list: &[String]) -> Result<()> {
    let list: Vec<String> = if list.is_empty() { S2_PROMPTS.iter().map(|p| p.to_string()).collect() } else { list.to_vec() };
    let task = format!("prompts-{}", util::now_ms());
    let mut s = Session::start(cwd, settings, hooks_log, &task, &[])?;
    s.boot()?;
    let mut stops = 0;
    for (i, p) in list.iter().enumerate() {
        s.type_line(p)?;
        // A slash command may never run the model (no Stop): give it time, then move on.
        if p.starts_with('/') {
            std::thread::sleep(Duration::from_secs(8));
            continue;
        }
        stops += 1;
        let events = wait_for(hooks_log, &task, "Stop", stops, Duration::from_secs(180))?;
        if events.iter().filter(|e| e.event.hook_event_name == "Stop").count() < stops {
            bail!("prompt {} did not finish (no Stop); not typing the rest", i + 1);
        }
    }
    let screen = s.finish();
    let out = screen_path(hooks_log, &task);
    std::fs::write(&out, &screen)?;
    let events = hook::load_events(&util::read_jsonl(hooks_log)?, Some(&task));
    let sid = events.iter().find_map(|e| e.event.session_id.clone()).unwrap_or_default();
    let tp = events.iter().rev().find_map(|e| e.event.transcript_path.clone()).unwrap_or_default();
    println!("task: {task}  screen: {}", out.display());
    print_timeline(&events);
    println!("session id: {sid}");
    println!("transcript: {tp}");
    Ok(())
}

// ---------------------------------------------------------------- S3 status accuracy

const S3_OK: &str = "Responda apenas OK.";
// `date` / `uname -s` are auto-approved as read-only by Claude Code 2.1.285 (no dialog), so the
// dialog steps use a write confined to the sandbox cwd. Only this exact command is approved.
const S3_ALLOW: &str = "Use o Bash para rodar exatamente `touch s3-allowed.txt` e diga se deu certo.";
const S3_DENY: &str = "Use o Bash para rodar exatamente `touch s3-denied.txt` e diga se deu certo.";
const S3_SLEEP: &str = "Execute `sleep 2 && echo a` três vezes, uma chamada por vez.";

struct Check {
    run: usize,
    step: String,
    ts_ms: u64,
    hook: Option<status::Status>,
    hook_later: Option<status::Status>,
    screen: status::Status,
    excerpt: String,
    recent: String,
}

impl Check {
    fn ok(&self) -> bool {
        self.hook == Some(self.screen)
    }
}

fn status_at(events: &[hook::LoggedEvent], ts_ms: u64) -> Option<status::Status> {
    events.iter().filter(|e| e.ts_ms <= ts_ms).fold(None, |cur, e| status::next_status(cur, &e.event))
}

/// Last `n` non-blank lines of the grid.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

struct S3<'a> {
    run: usize,
    hooks_log: &'a Path,
    task: String,
    checks: Vec<Check>,
}

impl S3<'_> {
    fn events(&self) -> Result<Vec<hook::LoggedEvent>> {
        Ok(if self.hooks_log.exists() {
            hook::load_events(&util::read_jsonl(self.hooks_log)?, Some(&self.task))
        } else {
            Vec::new()
        })
    }

    /// Samples the grid and the hook-derived status at the same instant. `settle` lets the
    /// screen sit before sampling; hooks are read 1s later so late writes with ts <= sample count.
    fn check(&mut self, s: &mut Session, step: &str, settle: Duration) -> Result<()> {
        std::thread::sleep(settle);
        let ts_ms = util::now_ms();
        let grid = s.grid();
        let screen = screen::status(&grid, s.exited());
        std::thread::sleep(Duration::from_secs(1));
        let events = self.events()?;
        let recent = events
            .iter()
            .filter(|e| e.ts_ms + 15_000 >= ts_ms && e.ts_ms <= ts_ms + 1000)
            .map(|e| {
                let extra = e.event.message.clone().or(e.event.tool_name.clone()).unwrap_or_default();
                format!("{} {} {}", e.ts_ms, e.event.hook_event_name, extra)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let c = Check {
            run: self.run,
            step: step.to_string(),
            ts_ms,
            hook: status_at(&events, ts_ms),
            hook_later: status_at(&events, ts_ms + 1000),
            screen,
            excerpt: tail_lines(&grid, 25),
            recent,
        };
        println!(
            "  {:<28} ts={} hook={:<10} screen={:<10} {}",
            c.step,
            c.ts_ms,
            format!("{:?}", c.hook),
            format!("{:?}", c.screen),
            if c.ok() { "match" } else { "MISMATCH" }
        );
        if self.run == 1 && step == "e working #1" {
            println!("  working screen (run 1):\n```\n{}\n```", tail_lines(&grid, 12));
        }
        self.checks.push(c);
        Ok(())
    }
}

fn busy(g: &str) -> bool {
    screen::has_spinner(g) || screen::has_permission_prompt(g)
}

/// Prompt → Working → (permission) → answer → Idle. `answer` is "Yes" or "No".
fn s3_permission_step(r: &mut S3, s: &mut Session, name: &str, prompt: &str, command: &str, answer: &str) -> Result<()> {
    s.type_line(prompt)?;
    if s.wait_screen("spinner", Duration::from_secs(15), screen::has_spinner) {
        r.check(s, &format!("{name} working"), Duration::from_millis(300))?;
    }
    if !s.wait_screen("permission prompt", Duration::from_secs(60), screen::has_permission_prompt) {
        r.check(s, &format!("{name} NO PROMPT"), Duration::ZERO)?;
        return Ok(());
    }
    r.check(s, &format!("{name} permission"), Duration::from_millis(1500))?;
    let grid = s.grid();
    if r.run == 1 {
        println!("  permission dialog (run 1):\n```\n{}\n```", tail_lines(&grid, 20));
    }
    // Allow only the exact harmless command we asked for; anything else is denied.
    let wanted = grid.lines().any(|l| {
        let t = l.trim();
        t == command
    });
    let label = if answer == "Yes" && wanted { "Yes" } else { "No" };
    match screen::option_key(&grid, label) {
        Some(k) => s.key(k.to_string().as_bytes())?,
        None => {
            eprintln!("no '{label}' option on screen; sending Esc");
            s.key(b"\x1b")?;
        }
    }
    println!("  ({name}: answered {label})");
    std::thread::sleep(Duration::from_millis(700));
    if screen::has_spinner(&s.grid()) && !screen::has_permission_prompt(&s.grid()) {
        r.check(s, &format!("{name} after-{label} working"), Duration::from_millis(200))?;
    }
    s.wait_screen("idle after answer", Duration::from_secs(90), |g| !busy(g));
    r.check(s, &format!("{name} after-{label} idle"), Duration::from_millis(2000))?;
    Ok(())
}

fn s3_run(run: usize, cwd: &str, settings: &str, hooks_log: &Path) -> Result<Vec<Check>> {
    let sandbox = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/make-sandbox.sh");
    let out = std::process::Command::new("bash").arg(sandbox).arg(cwd).output()?;
    if !out.status.success() {
        bail!("make-sandbox failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let task = format!("s3-{run}-{}", util::now_ms());
    println!("run {run}: task {task}");
    let mut r = S3 { run, hooks_log, task: task.clone(), checks: Vec::new() };
    let mut s = Session::start(cwd, settings, hooks_log, &task, &[])?;
    s.boot()?;

    // a) boot
    s.wait_screen("input box", Duration::from_secs(20), |g| g.contains('❯'));
    r.check(&mut s, "a boot", Duration::from_secs(2))?;

    // b) plain reply
    s.type_line(S3_OK)?;
    if s.wait_screen("spinner", Duration::from_secs(15), screen::has_spinner) {
        r.check(&mut s, "b working", Duration::from_millis(200))?;
    }
    s.wait_screen("idle", Duration::from_secs(60), |g| !busy(g));
    r.check(&mut s, "b idle", Duration::from_secs(2))?;

    // c) allow once, d) deny
    s3_permission_step(&mut r, &mut s, "c", S3_ALLOW, "touch s3-allowed.txt", "Yes")?;
    s3_permission_step(&mut r, &mut s, "d", S3_DENY, "touch s3-denied.txt", "No")?;

    // e) allowlisted multi-tool turn
    s.type_line(S3_SLEEP)?;
    if s.wait_screen("spinner", Duration::from_secs(15), screen::has_spinner) {
        for i in 0..4 {
            if !screen::has_spinner(&s.grid()) {
                break;
            }
            r.check(&mut s, &format!("e working #{i}"), Duration::from_millis(if i == 0 { 300 } else { 1500 }))?;
        }
    }
    s.wait_screen("idle", Duration::from_secs(120), |g| !busy(g));
    r.check(&mut s, "e idle", Duration::from_secs(2))?;

    // f) idle notification after ~60s without input
    let idle_note = |ev: &[hook::LoggedEvent]| {
        ev.iter().any(|e| {
            e.event.hook_event_name == "Notification"
                && e.event.message.as_deref().is_some_and(|m| m.contains(status::IDLE_NOTIFICATION))
        })
    };
    std::thread::sleep(Duration::from_secs(60));
    wait_until(hooks_log, &task, "idle Notification", Duration::from_secs(30), idle_note)?;
    r.check(&mut s, "f idle-notification", Duration::from_secs(1))?;

    // g) exit
    s.type_line("/exit")?;
    let start = Instant::now();
    while !s.exited() && start.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(200));
    }
    wait_for(hooks_log, &task, "SessionEnd", 1, Duration::from_secs(5))?;
    r.check(&mut s, "g exit", Duration::from_millis(500))?;

    let events = r.events()?;
    let count = |n: &str| events.iter().filter(|e| e.event.hook_event_name == n).count();
    let notes: Vec<String> = events
        .iter()
        .filter(|e| e.event.hook_event_name == "Notification")
        .filter_map(|e| e.event.message.clone())
        .collect();
    println!("  hook counts: PermissionRequest={} Notification={} {:?}", count("PermissionRequest"), count("Notification"), notes);
    let screen = s.finish();
    std::fs::write(screen_path(hooks_log, &task), &screen)?;
    Ok(r.checks)
}

fn s3(cwd: &str, settings: &str, hooks_log: &Path, runs: usize) -> Result<()> {
    let mut all = Vec::new();
    for run in 1..=runs {
        all.extend(s3_run(run, cwd, settings, hooks_log)?);
    }
    println!("\n| run | checkpoints | mismatches |\n|---|---|---|");
    for run in 1..=runs {
        let mine: Vec<&Check> = all.iter().filter(|c| c.run == run).collect();
        println!("| {run} | {} | {} |", mine.len(), mine.iter().filter(|c| !c.ok()).count());
    }
    println!("| all | {} | {} |", all.len(), all.iter().filter(|c| !c.ok()).count());
    for c in all.iter().filter(|c| !c.ok()) {
        println!(
            "\n### MISMATCH run {} step '{}' ts={}: hook={:?} (hook 1s later={:?}) screen={:?}",
            c.run, c.step, c.ts_ms, c.hook, c.hook_later, c.screen
        );
        println!("hook events (last 15s):\n```\n{}\n```", c.recent);
        println!("screen (ANSI-free grid, last 25 lines):\n```\n{}\n```", c.excerpt);
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: spike-claude-pty <cwd> <settings.json> <hooks.jsonl> <delay-secs> [steer|resume|status [runs]|prompts [prompt...]]";
    let cwd = args.first().context(usage)?;
    let settings = args.get(1).context(usage)?;
    let hooks_log = PathBuf::from(args.get(2).context(usage)?);
    let delay: u64 = args.get(3).context(usage)?.parse()?;
    match args.get(4).map(String::as_str).unwrap_or("steer") {
        "steer" => steer(cwd, settings, &hooks_log, delay),
        "resume" => resume(cwd, settings, &hooks_log),
        "status" => s3(cwd, settings, &hooks_log, args.get(5).map(|r| r.parse()).transpose()?.unwrap_or(3)),
        "prompts" => prompts(cwd, settings, &hooks_log, &args[5..]),
        other => bail!("unknown mode {other}"),
    }
}
