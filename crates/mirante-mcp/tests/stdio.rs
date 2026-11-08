//! The binary over real pipes: no task → silent exit 0; with a task it answers the MCP
//! handshake, and a call it cannot deliver (no daemon) is a tool error, not a crash.
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

fn run(task: Option<&str>, input: &str) -> (Option<i32>, String) {
    let home = mirante_ipc::test_dir::TestDir::new();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mirante-mcp"));
    cmd.env_clear().env("MIRANTE_HOME", home.path()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(t) = task {
        cmd.env("MIRANTE_TASK_ID", t);
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.code(), String::from_utf8(out.stdout).unwrap())
}

#[test]
fn without_a_task_it_prints_nothing_and_exits_0() {
    let (code, out) = run(None, "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
    assert_eq!((code, out.as_str()), (Some(0), ""));
}

#[test]
fn answers_the_handshake_and_reports_an_absent_daemon_as_a_tool_error() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_search","arguments":{"query":"spool"}}}"#,
        "\n"
    );
    let (code, out) = run(Some("t1"), input);
    assert_eq!(code, Some(0));
    let lines: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert_eq!(lines[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!((lines[1]["id"].clone(), lines[1]["result"]["isError"].clone()), (Value::from(2), Value::Bool(true)));
}
