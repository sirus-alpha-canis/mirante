//! Plays a JSON-RPC script against `codex app-server` over stdio, logging all traffic
//! with timestamps. $CWD/$THREAD/$TURN placeholders are filled from captured ids
//! (S1 Codex). An optional preset thread id fills $THREAD instead, which is how the
//! S4 resume check replays `thread/resume` against a fresh app-server process.
//!
//! Same conventions as `spike-claude-stream`: `MIRANTE_CODEX_BIN` overrides the binary,
//! nested-session and cmux variables are removed (auth is kept), a guard reaps the
//! child on every exit path and the reader thread is joined before classifying.
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use spike_kit::{codex, delivery, util};

fn wait_capture(
    out: &Path,
    pick: fn(&[Value], &[&str]) -> Option<String>,
    pointers: &[&str],
    timeout: Duration,
) -> Result<Option<String>> {
    let start = Instant::now();
    loop {
        if out.exists() {
            if let Some(id) = pick(&util::read_jsonl(out)?, pointers) {
                return Ok(Some(id));
            }
        }
        if start.elapsed() > timeout {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Owns the child; dropping it reaps codex so an early `?` return cannot orphan it.
struct ChildGuard {
    child: Child,
}

impl ChildGuard {
    fn wait_for(&mut self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Close stdin, give codex 5s to exit on its own, then SIGTERM (2s), then kill.
        drop(self.child.stdin.take());
        if self.wait_for(Duration::from_secs(5)) {
            return;
        }
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status();
        if self.wait_for(Duration::from_secs(2)) {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: spike-codex <cwd> <script.jsonl> <out.jsonl> <wait-secs> [thread-id]";
    let cwd = args.first().context(usage)?.clone();
    let script = util::read_jsonl(Path::new(args.get(1).context(usage)?))?;
    let out = PathBuf::from(args.get(2).context(usage)?);
    let wait: u64 = args.get(3).context(usage)?.parse()?;
    let preset_thread = args.get(4).cloned();
    if out.exists() {
        // The classifier reads the whole log: never mix runs.
        std::fs::remove_file(&out)?;
    }

    let mut cmd = Command::new(util::codex_bin());
    cmd.arg("app-server")
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for k in util::codex_env_to_scrub() {
        cmd.env_remove(k);
    }
    let mut guard = ChildGuard {
        child: cmd.spawn().context("failed to start codex app-server")?,
    };
    let mut stdin = guard.child.stdin.take().context("no stdin")?;
    let stdout = guard.child.stdout.take().context("no stdout")?;
    let out_reader = out.clone();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let parsed: Value = serde_json::from_str(&line).unwrap_or(Value::String(line.clone()));
            let _ = util::append_jsonl(
                &out_reader,
                &json!({"ts": util::now_ms(), "dir": "out", "line": parsed}),
            );
        }
    });

    for step in &script {
        std::thread::sleep(Duration::from_millis(
            step["delay_ms"].as_u64().unwrap_or(0),
        ));
        let template = step["msg"].to_string();
        let thread = match (&preset_thread, template.contains("$THREAD")) {
            (Some(id), true) => Some(id.clone()),
            (None, true) => wait_capture(
                &out,
                codex::capture,
                &codex::THREAD_POINTERS,
                Duration::from_secs(15),
            )?,
            (_, false) => None,
        };
        let turn = if template.contains("$TURN") {
            wait_capture(
                &out,
                codex::capture_last,
                &codex::TURN_POINTERS,
                Duration::from_secs(15),
            )?
        } else {
            None
        };
        let msg = codex::fill(&template, &cwd, thread.as_deref(), turn.as_deref())
            .map_err(anyhow::Error::msg)?;
        writeln!(stdin, "{msg}")?;
        stdin.flush()?;
        util::append_jsonl(
            &out,
            &json!({"ts": util::now_ms(), "dir": "in", "line": serde_json::from_str::<Value>(&msg)?}),
        )?;
    }

    std::thread::sleep(Duration::from_secs(wait));
    drop(stdin);
    drop(guard); // reaps codex (stdin already closed), so the reader sees EOF
    let _ = reader.join();

    let log = util::read_jsonl(&out)?;
    if let Some(id) = codex::capture(&log, &codex::THREAD_POINTERS) {
        println!("thread id: {id}");
    }
    for v in log
        .iter()
        .filter(|v| v["dir"] == "out" && v["line"].get("error").is_some())
    {
        println!(
            "json-rpc error (id {}): {}",
            v["line"]["id"], v["line"]["error"]
        );
    }
    let completed = log
        .iter()
        .filter(|v| v["dir"] == "out" && v["line"]["method"] == "turn/completed")
        .count();
    println!("turn/completed seen: {completed}");
    for v in log
        .iter()
        .filter(|v| v["dir"] == "out" && v["line"]["method"] == "item/completed")
    {
        let item = &v["line"]["params"]["item"];
        if item["type"] == "agentMessage" {
            println!("agentMessage @{}: {}", v["ts"], item["text"]);
        }
    }
    println!(
        "mid-run delivery (codex): {:?}",
        delivery::classify_codex(&log, "BANANA")
    );
    Ok(())
}
