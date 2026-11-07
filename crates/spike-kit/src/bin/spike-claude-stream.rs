//! Drives `claude -p` with stream-json in/out. Sends a 2nd message while the 1st
//! turn runs and classifies whether it steered the running turn (S1 headless).
//!
//! Same conventions as `spike-claude-pty`: `MIRANTE_CLAUDE_BIN` overrides the binary,
//! nested-session/cmux variables are removed, and the permission mode is pinned to
//! `manual` so only the explicit allowlist below can run.
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use spike_kit::{delivery, stream, util};

const FIRST: &str = "Execute o comando `sleep 3 && echo passo-N` cinco vezes, uma chamada por vez, com N de 1 a 5. Ao terminar, diga FIM.";
const SECOND: &str =
    "Mudança de planos: pare o que está fazendo e responda apenas a palavra BANANA.";

fn send(stdin: &mut impl Write, out: &Path, text: &str) -> Result<()> {
    let line = stream::user_line(text);
    writeln!(stdin, "{line}")?;
    stdin.flush()?;
    util::append_jsonl(
        out,
        &json!({"ts": util::now_ms(), "dir": "in", "line": serde_json::from_str::<Value>(&line)?}),
    )?;
    Ok(())
}

/// Gives claude `grace` to exit on its own (stdin closed), then kills it.
fn reap(child: &mut Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// `~/.claude/projects/<cwd-slug>/<session_id>.jsonl`, found by name (no slug guessing).
fn find_transcript(session_id: &str) -> Option<PathBuf> {
    let projects = PathBuf::from(std::env::var("HOME").ok()?).join(".claude/projects");
    std::fs::read_dir(projects)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(format!("{session_id}.jsonl")))
        .find(|p| p.exists())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: spike-claude-stream <cwd> <out.jsonl> <delay-secs>";
    let cwd = args.first().context(usage)?.clone();
    let out = PathBuf::from(args.get(1).context(usage)?);
    let delay: u64 = args.get(2).context(usage)?.parse()?;
    if out.exists() {
        // The classifier reads the whole log: never mix runs.
        std::fs::remove_file(&out)?;
    }

    let mut cmd = Command::new(util::claude_bin());
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "manual",
        "--allowedTools",
        "Bash(sleep:*)",
        "Bash(echo:*)",
    ])
    .current_dir(&cwd)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit());
    for k in util::env_to_scrub() {
        cmd.env_remove(k);
    }
    let mut child = cmd.spawn().context("failed to start claude")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;

    let (tx, rx) = mpsc::channel::<()>();
    let out_reader = out.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let parsed: Value = serde_json::from_str(&line).unwrap_or(Value::String(line.clone()));
            let is_result = parsed["type"] == "result";
            let _ = util::append_jsonl(
                &out_reader,
                &json!({"ts": util::now_ms(), "dir": "out", "line": parsed}),
            );
            if is_result {
                let _ = tx.send(());
            }
        }
    });

    send(&mut stdin, &out, FIRST)?;
    std::thread::sleep(Duration::from_secs(delay));
    send(&mut stdin, &out, SECOND)?;

    // If the 2nd message is absorbed into the running turn there may be only ONE result,
    // so after the first one wait a shorter grace for a possible second.
    let mut results = 0;
    while results < 2 {
        let timeout = Duration::from_secs(if results == 0 { 240 } else { 60 });
        match rx.recv_timeout(timeout) {
            Ok(()) => results += 1,
            Err(_) => break,
        }
    }
    drop(stdin);
    reap(&mut child, Duration::from_secs(5));

    let log = util::read_jsonl(&out)?;
    println!("results seen: {results}");
    println!(
        "mid-run delivery (stream-json): {:?}",
        delivery::classify_stream(&log, "BANANA")
    );

    let session_id = log
        .iter()
        .filter(|v| v["dir"] == "out")
        .find_map(|v| v["line"]["session_id"].as_str().map(String::from));
    match session_id.as_deref() {
        Some(sid) => match find_transcript(sid) {
            Some(path) => {
                println!("session id: {sid}");
                println!("transcript: {}", path.display());
                println!(
                    "mid-run delivery (transcript): {:?}",
                    delivery::classify_transcript(&util::read_jsonl(&path)?)
                );
            }
            None => {
                println!("mid-run delivery (transcript): no {sid}.jsonl under ~/.claude/projects")
            }
        },
        None => println!("mid-run delivery (transcript): no session_id in the stream"),
    }
    Ok(())
}
