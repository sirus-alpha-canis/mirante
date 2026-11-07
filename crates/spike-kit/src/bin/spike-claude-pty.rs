//! Drives interactive `claude` in a PTY with scoped hooks.
//! steer:  sends a 2nd message while the 1st turn runs; classifies via hook timing (S1 + S3).
//! resume: sets a secret, exits, resumes by session id and asks for the secret (S4).
//!
//! `MIRANTE_CLAUDE_BIN` overrides the binary (default `claude` on PATH), e.g. to skip a
//! terminal-app wrapper shim that would inject its own hooks.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use spike_kit::{delivery, hook, status, transcript, util};

const FIRST: &str = "Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma chamada por vez, com N de 1 a 5. Ao terminar, diga FIM.";
const SECOND: &str = "Mudança de planos: pare o que está fazendo e responda apenas a palavra BANANA.";
const SECRET_SET: &str = "A palavra secreta é JABUTICABA. Responda apenas OK.";
const SECRET_ASK: &str = "Qual é a palavra secreta? Responda só a palavra.";

/// Inherited variables that make a child `claude` believe it is nested inside another
/// Claude Code session (or a terminal wrapper's integration). Auth variables are kept.
const NESTED_SESSION_ENV: [&str; 13] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    "CMUX_CLAUDE_PID",
];

struct Session {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    screen: Arc<Mutex<String>>,
}

impl Session {
    fn start(cwd: &str, settings: &str, hooks_log: &Path, task: &str, extra: &[&str]) -> Result<Session> {
        let pair = native_pty_system().openpty(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 })?;
        let bin = std::env::var("MIRANTE_CLAUDE_BIN").unwrap_or_else(|_| "claude".into());
        let mut cmd = CommandBuilder::new(bin);
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
        for k in NESTED_SESSION_ENV {
            cmd.env_remove(k);
        }
        // Terminal-wrapper (cmux) integration hooks would pollute the measurement.
        for (k, _) in std::env::vars() {
            if k.starts_with("CMUX_CLAUDE_") || k.starts_with("CMUX_AGENT_") {
                cmd.env_remove(k);
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("MIRANTE_TASK_ID", task);
        cmd.env("MIRANTE_HOOK_LOG", hooks_log);
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pair.master.take_writer()?));
        let screen = Arc::new(Mutex::new(String::new()));
        let (w2, s2) = (writer.clone(), screen.clone());
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
            }
        });
        Ok(Session { _master: pair.master, child, writer, screen })
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

    fn finish(mut self) -> String {
        let _ = self.type_line("/exit");
        std::thread::sleep(Duration::from_secs(3));
        let _ = self.child.kill();
        let _ = self.child.wait();
        let screen = self.screen.lock().unwrap();
        screen.clone()
    }
}

fn wait_until(
    hooks_log: &Path,
    task: &str,
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
        if done(&events) || start.elapsed() > timeout {
            return Ok(events);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn wait_for(hooks_log: &Path, task: &str, event: &str, count: usize, timeout: Duration) -> Result<Vec<hook::LoggedEvent>> {
    wait_until(hooks_log, task, timeout, |events| {
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
    let events = wait_until(hooks_log, &task, Duration::from_secs(240), second_prompt_handled)?;
    let screen = s.finish();
    let out = screen_path(hooks_log, &task);
    std::fs::write(&out, &screen)?;
    println!("task: {task}  screen: {}", out.display());
    print_timeline(&events);
    println!("mid-run delivery (pty+hooks): {:?}", delivery::classify_hooks(&events));
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

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: spike-claude-pty <cwd> <settings.json> <hooks.jsonl> <delay-secs> [steer|resume]";
    let cwd = args.first().context(usage)?;
    let settings = args.get(1).context(usage)?;
    let hooks_log = PathBuf::from(args.get(2).context(usage)?);
    let delay: u64 = args.get(3).context(usage)?.parse()?;
    match args.get(4).map(String::as_str).unwrap_or("steer") {
        "steer" => steer(cwd, settings, &hooks_log, delay),
        "resume" => resume(cwd, settings, &hooks_log),
        other => bail!("unknown mode {other}"),
    }
}
