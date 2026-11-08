//! Plays a JSON-RPC script against any ACP agent over stdio and logs every line in both
//! directions with ms timestamps (`{ts, dir, line}`; `dir` is `in`, `out`, `note` or
//! `exit`). `session/request_permission` is answered per `--answer` (never an
//! `*_always` option). When the script sends `session/cancel`, every pending permission
//! request is answered `cancelled`, as the ACP spec requires of clients.
//!
//! usage: spike-acp --bin <path> [--arg a]... --cwd <dir> --script s.jsonl --out log.jsonl
//!        [--answer allow_once|reject_once|cancel|hold] [--answer-after-ms 300]
//!        [--env K=V]... [--session <id>] [--echo <path>] [--wait <secs>]
//!        [--allow-match TEXT]...   allow_once only for tool calls containing TEXT and
//!                                  no path outside /tmp/acp-spike; others reject_once
//!        [--hold-match TEXT]...    leave a permission whose tool call contains TEXT pending
//!
//!        spike-acp --report <log.jsonl>   (re-print the summary of an earlier run)
//!
//! Script lines (JSONL), run in order:
//!   {"delay_ms": N, "msg": {...}}          send one message ($CWD, $SESSION, $ECHO filled)
//!   {"wait": "response", "id": N}          until the agent answers request N
//!   {"wait": "permission"}                 until one more request_permission arrives
//!   {"wait": "idle"}                       until every session/prompt is answered
//!   {"wait": "match", "text": "..."}       until a new agent line contains the text
//!   {"sleep_ms": N}
//!   {"shell": "lsof -p $PID"}              run a probe ($PID = agent pid), output logged as a note
//!   {"kill": true}                         SIGKILL the agent's process group (crash test)
//! Every wait takes an optional "timeout_ms" (default 120000).
//!
//! Hygiene as `spike-codex`: nested-session and `CMUX_*` variables removed, auth kept, the
//! agent runs in its own process group and a guard reaps the whole group on every exit.
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use spike_kit::acp::{self, Answer, Reply};
use spike_kit::util;

#[derive(Clone)]
struct Log {
    path: PathBuf,
    lines: Arc<Mutex<Vec<Value>>>,
}

impl Log {
    fn push(&self, dir: &str, line: Value) {
        let v = json!({"ts": util::now_ms(), "dir": dir, "line": line});
        let _ = util::append_jsonl(&self.path, &v);
        self.lines.lock().unwrap().push(v);
    }
    fn snapshot(&self) -> Vec<Value> {
        self.lines.lock().unwrap().clone()
    }
    fn len(&self) -> usize {
        self.lines.lock().unwrap().len()
    }
}

type Stdin = Arc<Mutex<Option<ChildStdin>>>;

fn send(stdin: &Stdin, log: &Log, msg: &Value) -> Result<()> {
    let mut g = stdin.lock().unwrap();
    let w = g.as_mut().context("agent stdin closed")?;
    writeln!(w, "{msg}")?;
    w.flush()?;
    log.push("in", msg.clone());
    Ok(())
}

/// Owns the child; dropping it reaps the agent's whole process group.
struct ChildGuard {
    child: Child,
    stdin: Stdin,
    log: Log,
    exited: bool,
}

impl ChildGuard {
    fn wait_for(&mut self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        loop {
            if let Ok(Some(st)) = self.child.try_wait() {
                if !self.exited {
                    self.exited = true;
                    self.log.push("exit", json!({"code": st.code(), "status": st.to_string()}));
                }
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn signal_group(&self, sig: &str) {
        let _ = Command::new("kill").arg(sig).arg(format!("-{}", self.child.id())).status();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        drop(self.stdin.lock().unwrap().take());
        if !self.wait_for(Duration::from_secs(5)) {
            self.signal_group("-TERM");
            if !self.wait_for(Duration::from_secs(2)) {
                self.signal_group("-KILL");
                let _ = self.child.kill();
                self.wait_for(Duration::from_secs(2));
            }
        }
        // Children that outlive the agent itself (LSP servers, MCP servers) go too.
        self.signal_group("-KILL");
    }
}

struct Args {
    bin: String,
    args: Vec<String>,
    cwd: String,
    script: PathBuf,
    out: PathBuf,
    answer: Answer,
    answer_after: Duration,
    env: Vec<(String, String)>,
    session: Option<String>,
    echo: String,
    wait: u64,
    allow_match: Vec<String>,
    hold_match: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        bin: String::new(),
        args: vec![],
        cwd: String::new(),
        script: PathBuf::new(),
        out: PathBuf::new(),
        answer: Answer::RejectOnce,
        answer_after: Duration::from_millis(300),
        env: vec![],
        session: None,
        echo: std::env::current_exe()?.with_file_name("spike-mcp-echo").display().to_string(),
        wait: 3,
        allow_match: vec![],
        hold_match: vec![],
    };
    while let Some(flag) = it.next() {
        let mut val = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--bin" => a.bin = val()?,
            "--arg" => a.args.push(val()?),
            "--cwd" => a.cwd = val()?,
            "--script" => a.script = val()?.into(),
            "--out" => a.out = val()?.into(),
            "--answer" => a.answer = val()?.parse().map_err(anyhow::Error::msg)?,
            "--answer-after-ms" => a.answer_after = Duration::from_millis(val()?.parse()?),
            "--env" => {
                let kv = val()?;
                let (k, v) = kv.split_once('=').context("--env takes K=V")?;
                a.env.push((k.into(), v.into()));
            }
            "--session" => a.session = Some(val()?),
            "--echo" => a.echo = val()?,
            "--wait" => a.wait = val()?.parse()?,
            "--allow-match" => a.allow_match.push(val()?),
            "--hold-match" => a.hold_match.push(val()?),
            _ => bail!("unknown flag {flag}"),
        }
    }
    if a.bin.is_empty() || a.cwd.is_empty() || a.script.as_os_str().is_empty() || a.out.as_os_str().is_empty() {
        bail!("--bin, --cwd, --script and --out are required");
    }
    Ok(a)
}

fn wait_until(log: &Log, timeout: Duration, what: &str, pred: impl Fn(&[Value]) -> bool) -> Result<()> {
    let start = Instant::now();
    loop {
        if pred(&log.snapshot()) {
            return Ok(());
        }
        if start.elapsed() > timeout {
            log.push("note", json!({"timeout": what}));
            eprintln!("timeout waiting for {what}");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn is_perm_request(v: &Value) -> bool {
    v["dir"] == "out" && v["line"]["method"] == "session/request_permission" && v["line"].get("id").is_some()
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("--report") {
        let path = argv.get(2).context("usage: spike-acp --report <log.jsonl>")?;
        report(&util::read_jsonl(std::path::Path::new(path))?);
        return Ok(());
    }
    let a = parse_args()?;
    let script = util::read_jsonl(&a.script)?;
    if a.out.exists() {
        std::fs::remove_file(&a.out)?; // never mix runs
    }
    let log = Log { path: a.out.clone(), lines: Arc::new(Mutex::new(Vec::new())) };
    let stderr_path = a.out.with_extension("stderr");

    let mut cmd = Command::new(&a.bin);
    cmd.args(&a.args)
        .current_dir(&a.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(File::create(&stderr_path)?)
        .process_group(0);
    for k in util::codex_env_to_scrub() {
        cmd.env_remove(k);
    }
    for (k, v) in &a.env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().with_context(|| format!("failed to start {}", a.bin))?;
    let pid = child.id();
    std::fs::write(a.out.with_extension("pid"), pid.to_string())?;
    eprintln!("agent pid {pid}");
    let stdin: Stdin = Arc::new(Mutex::new(child.stdin.take()));
    let stdout = child.stdout.take().context("no stdout")?;
    let mut guard = ChildGuard { child, stdin: stdin.clone(), log: log.clone(), exited: false };
    log.push(
        "note",
        json!({"argv": std::iter::once(&a.bin).chain(&a.args).collect::<Vec<_>>(), "cwd": a.cwd, "pid": pid,
        "env_set": a.env.iter().map(|(k, _)| k).collect::<Vec<_>>(), "answer": format!("{:?}", a.answer)}),
    );

    let pending: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let reader = {
        let (log, stdin, pending, answer, after) =
            (log.clone(), stdin.clone(), pending.clone(), a.answer, a.answer_after);
        let (allow_match, hold_match) = (a.allow_match.clone(), a.hold_match.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
                let parsed: Value = serde_json::from_str(&line).unwrap_or(Value::String(line.clone()));
                log.push("out", parsed.clone());
                let (Some(method), Some(id)) = (parsed["method"].as_str(), parsed.get("id").cloned()) else { continue };
                if method == "session/request_permission" {
                    let options = parsed["params"]["options"].as_array().cloned().unwrap_or_default();
                    let tool_call = &parsed["params"]["toolCall"];
                    let gated = if hold_match.iter().any(|m| tool_call.to_string().contains(m.as_str())) {
                        Answer::Hold
                    } else {
                        acp::gate(answer, tool_call, &allow_match)
                    };
                    if gated != answer {
                        log.push("note", json!({"gate": format!("{answer:?} narrowed to {gated:?}")}));
                    }
                    match acp::choose(&options, gated) {
                        None => pending.lock().unwrap().push(id),
                        Some(reply) => {
                            pending.lock().unwrap().push(id.clone());
                            let (log, stdin, pending) = (log.clone(), stdin.clone(), pending.clone());
                            std::thread::spawn(move || {
                                std::thread::sleep(after);
                                let mut p = pending.lock().unwrap();
                                if p.contains(&id) {
                                    p.retain(|x| x != &id);
                                    let _ = send(
                                        &stdin,
                                        &log,
                                        &json!({"jsonrpc": "2.0", "id": id, "result": reply.result()}),
                                    );
                                }
                            });
                        }
                    }
                } else {
                    // The spike declares no fs/terminal capability: refuse anything else.
                    let _ = send(
                        &stdin,
                        &log,
                        &json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": format!("method not found: {method}")}}),
                    );
                }
            }
        })
    };

    for step in &script {
        let timeout = Duration::from_millis(step["timeout_ms"].as_u64().unwrap_or(120_000));
        if let Some(ms) = step["sleep_ms"].as_u64() {
            std::thread::sleep(Duration::from_millis(ms));
        } else if let Some(cmdline) = step["shell"].as_str() {
            let cmdline = cmdline.replace("$PID", &pid.to_string());
            let o = Command::new("/bin/sh").arg("-c").arg(&cmdline).output()?;
            log.push("note", json!({"shell": cmdline, "stdout": String::from_utf8_lossy(&o.stdout), "stderr": String::from_utf8_lossy(&o.stderr)}));
        } else if step["kill"] == true {
            guard.signal_group("-KILL");
            guard.wait_for(Duration::from_secs(5));
        } else if let Some(what) = step["wait"].as_str() {
            let from = log.len();
            match what {
                "response" => {
                    let id = step["id"].clone();
                    wait_until(&log, timeout, &format!("response {id}"), |l| {
                        l.iter()
                            .any(|v| v["dir"] == "out" && v["line"].get("method").is_none() && v["line"]["id"] == id)
                    })?
                }
                "permission" => wait_until(&log, timeout, "permission", |l| l[from..].iter().any(is_perm_request))?,
                "idle" => wait_until(&log, timeout, "idle", |l| {
                    acp::ground_truth(l).last().is_some_and(|p| {
                        p.status != spike_kit::status::Status::Working
                            && p.status != spike_kit::status::Status::NeedsYou
                    })
                })?,
                "match" => {
                    let text = step["text"].as_str().unwrap_or("").to_string();
                    wait_until(&log, timeout, &format!("match {text}"), |l| {
                        l[from..].iter().any(|v| v["dir"] == "out" && v["line"].to_string().contains(&text))
                    })?
                }
                other => bail!("unknown wait {other}"),
            }
        } else if step.get("msg").is_some() {
            std::thread::sleep(Duration::from_millis(step["delay_ms"].as_u64().unwrap_or(0)));
            let session = a.session.clone().or_else(|| acp::capture_session(&log.snapshot()));
            let text =
                acp::fill(&step["msg"].to_string(), &a.cwd, session.as_deref(), &a.echo).map_err(anyhow::Error::msg)?;
            let msg: Value = serde_json::from_str(&text)?;
            send(&stdin, &log, &msg)?;
            if msg["method"] == "session/cancel" {
                for id in std::mem::take(&mut *pending.lock().unwrap()) {
                    send(&stdin, &log, &json!({"jsonrpc": "2.0", "id": id, "result": Reply::Cancelled.result()}))?;
                }
            }
        } else {
            bail!("unknown script step {step}");
        }
    }

    std::thread::sleep(Duration::from_secs(a.wait));
    drop(guard);
    let _ = reader.join();

    report(&log.snapshot());
    Ok(())
}

/// Prints the summary of one run: session id, errors, permissions, stop reasons, update
/// kinds, the steer verdict and the protocol-only status timeline.
fn report(log: &[Value]) {
    println!("session id: {}", acp::capture_session(log).unwrap_or_else(|| "-".into()));
    for v in log.iter().filter(|v| v["dir"] == "out" && v["line"].get("error").is_some()) {
        println!("json-rpc error (id {}): {}", v["line"]["id"], v["line"]["error"]);
    }
    for v in log.iter().filter(|v| is_perm_request(v)) {
        let p = &v["line"]["params"];
        let opts: Vec<String> = p["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| format!("{}={}", o["optionId"].as_str().unwrap_or("?"), o["kind"].as_str().unwrap_or("?")))
            .collect();
        println!("permission @{}: {} [{}] options {:?}", v["ts"], p["toolCall"]["title"], p["toolCall"]["kind"], opts);
    }
    for v in log.iter().filter(|v| v["dir"] == "out" && v["line"]["result"].get("stopReason").is_some()) {
        println!("prompt {} @{}: stopReason {}", v["line"]["id"], v["ts"], v["line"]["result"]["stopReason"]);
    }
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for v in log.iter().filter(|v| v["dir"] == "out" && v["line"]["method"] == "session/update") {
        *kinds
            .entry(v["line"]["params"]["update"]["sessionUpdate"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;
    }
    println!("session/update kinds: {kinds:?}");
    println!("steer: {:?}", acp::classify_steer(log));
    println!("ground truth:");
    let t0 = log.first().and_then(|v| v["ts"].as_u64()).unwrap_or(0);
    for p in acp::ground_truth(log) {
        println!("  +{:>6} ms  {:<40} {:?}", p.ts_ms.saturating_sub(t0), p.event, p.status);
    }
}
