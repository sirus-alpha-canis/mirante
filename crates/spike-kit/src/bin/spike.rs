use std::path::Path;

use anyhow::{bail, Context, Result};
use spike_kit::{delivery, hook, status, transcript, util};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("settings") => {
            let hook_bin = args.get(1).context("usage: spike settings <hook-bin-abs-path> <out.json>")?;
            let out = args.get(2).context("missing <out.json>")?;
            let s = hook::settings_json(hook_bin, &["Bash(sleep:*)", "Bash(echo:*)"]);
            if let Some(parent) = Path::new(out).parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(out, serde_json::to_string_pretty(&s)?)?;
            println!("wrote {out}");
        }
        Some("timeline") => {
            let log = args.get(1).context("usage: spike timeline <hooks.jsonl> [task-id]")?;
            let events = hook::load_events(&util::read_jsonl(Path::new(log))?, args.get(2).map(String::as_str));
            for t in status::timeline(&events) {
                println!("{:>14}  {:<18} -> {:?}", t.ts_ms, t.event, t.status);
            }
            println!("mid-run delivery (hooks): {:?}", delivery::classify_hooks(&events));
        }
        Some("usage") => {
            let path = args.get(1).context("usage: spike usage <transcript.jsonl> [session-id]")?;
            let u = transcript::sum_transcript(&util::read_jsonl(Path::new(path))?, args.get(2).map(String::as_str));
            println!("{u:?} total={}", u.total());
        }
        Some(cmd @ ("classify-stream" | "classify-codex")) => {
            let log = args.get(1).context("missing <log.jsonl>")?;
            let marker = args.get(2).map(String::as_str).unwrap_or("BANANA");
            let lines = util::read_jsonl(Path::new(log))?;
            let d = if cmd == "classify-stream" {
                delivery::classify_stream(&lines, marker)
            } else {
                delivery::classify_codex(&lines, marker)
            };
            println!("{d:?}");
        }
        _ => bail!("commands: settings | timeline | usage | classify-stream | classify-codex"),
    }
    Ok(())
}
