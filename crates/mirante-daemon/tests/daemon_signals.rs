//! The `mirante-daemon` binary ends cleanly on SIGTERM / SIGINT: sessions stopped, socket
//! removed, exit 0 (the same path as `daemon.shutdown`).
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mirante_ipc::test_dir::TestDir;
use mirante_ipc::{Client, Paths};

fn run_until_signal(sig: libc::c_int) {
    let d = TestDir::new();
    let paths = Paths::new(&d.path().join("home"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_mirante-daemon"))
        .env(mirante_ipc::paths::HOME_ENV, paths.home())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Client::connect(&paths.socket()).is_err() {
        assert!(Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: kill with a valid pid of our own child.
    assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, sig) }, 0);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("daemon ignored signal {sig}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    assert!(!paths.socket().exists(), "socket must be removed");
}

#[test]
fn sigterm_shuts_the_daemon_down_cleanly() {
    run_until_signal(libc::SIGTERM);
}

#[test]
fn sigint_shuts_the_daemon_down_cleanly() {
    run_until_signal(libc::SIGINT);
}
