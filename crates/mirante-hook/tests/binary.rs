use std::io::Write;
use std::process::{Command, Stdio};

use mirante_ipc::test_dir::TestDir;

fn run(home: &std::path::Path, stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mirante-hook"))
        .env("MIRANTE_HOME", home)
        .env("MIRANTE_TASK_ID", "t")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn exits_zero_and_prints_nothing_on_stdout_even_when_home_is_unusable() {
    let d = TestDir::new();
    let out = run(&d.path().join("a".repeat(200)), &[0xff, b'{', b'}']);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("lost"));
}

#[test]
fn non_utf8_stdin_is_spooled_lossily_not_dropped() {
    let d = TestDir::new();
    let home = d.path().join("h");
    let out = run(&home, &[b'{', 0xff, b'}']);
    assert!(out.status.success() && out.stdout.is_empty());
    let text = std::fs::read_to_string(home.join("hook-spool.jsonl")).unwrap();
    assert!(text.contains('\u{fffd}'));
}
