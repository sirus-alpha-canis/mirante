//! `mirante-mcp`: the stdio MCP server Mirante injects into one task's agent session (see
//! the library docs). It does nothing without `MIRANTE_TASK_ID`, like `mirante-hook`.
//! `--surface memory` (the default) serves repo memory, `--surface skills` serves
//! `skill_propose`; anything else exits 2. stdout carries only protocol messages; logs go
//! to stderr.
use std::io::{self, BufReader, Write};
use std::time::Duration;

use mirante_core::claude_launch::TASK_ENV;
use mirante_ipc::rpc::code;
use mirante_ipc::{Client, ClientError, Paths, RpcError};
use mirante_mcp::{read_line_capped, too_long_reply, Backend, Line, Server, Surface, MAX_LINE};
use serde_json::Value;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The daemon over its socket, one connection per call.
struct DaemonBackend {
    paths: Paths,
}

impl Backend for DaemonBackend {
    fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let unreachable = |e: io::Error| {
            eprintln!("mirante-mcp: the daemon is not reachable: {e}");
            mirante_mcp::unreachable(&e)
        };
        let mut c = Client::connect(&self.paths.socket()).map_err(unreachable)?;
        c.set_timeout(Some(CALL_TIMEOUT)).map_err(unreachable)?;
        c.call(method, params).map_err(|e| match e {
            ClientError::Rpc(e) => e,
            other => RpcError::new(code::INTERNAL, other.to_string()),
        })
    }
}

/// The `--surface` argument (`--surface x` or `--surface=x`), memory when absent.
fn surface(args: &[String]) -> Result<Surface, String> {
    let value = match args {
        [] => return Ok(Surface::Memory),
        [flag, v] if flag == "--surface" => v.as_str(),
        [one] => one.strip_prefix("--surface=").ok_or_else(|| format!("unknown argument {one}"))?,
        _ => return Err("usage: mirante-mcp [--surface memory|skills]".into()),
    };
    Surface::parse(value).ok_or_else(|| format!("unknown surface {value} (memory or skills)"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let surface = match surface(&args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mirante-mcp: {e}");
            std::process::exit(2);
        }
    };
    let task = match std::env::var(TASK_ENV) {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            eprintln!("mirante-mcp: {TASK_ENV} is not set; Mirante starts this server for its own tasks only");
            return;
        }
    };
    let paths = match Paths::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("mirante-mcp: {e}");
            return;
        }
    };
    let server = Server::with_surface(DaemonBackend { paths }, &task, surface);
    let mut input = BufReader::new(io::stdin().lock());
    let mut out = io::stdout().lock();
    loop {
        let reply = match read_line_capped(&mut input, MAX_LINE) {
            Ok(None) => break,
            Ok(Some(Line::Text(line))) => server.handle(&line),
            Ok(Some(Line::TooLong)) => Some(too_long_reply()),
            Err(e) => {
                eprintln!("mirante-mcp: reading stdin: {e}");
                break;
            }
        };
        if let Some(r) = reply {
            if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                break; // the client went away
            }
        }
    }
}
