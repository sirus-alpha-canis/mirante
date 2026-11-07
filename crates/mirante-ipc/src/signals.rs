//! Unix signals as channel messages, without blocking them in the signal mask (a blocked
//! mask is inherited by spawned agents, which would then ignore Ctrl-C in their PTY).
//!
//! A self-pipe: the handler (installed with `SA_RESTART`, so other threads' blocking reads
//! are not interrupted) writes the signal number to a non-blocking pipe; one reader thread
//! routes it to every [`subscribe`]r of that signal. Handlers reset to the default on
//! `exec`, so children are unaffected.
use std::io;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};

pub use libc::{SIGHUP, SIGINT, SIGTERM, SIGUSR1, SIGUSR2, SIGWINCH};

static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static ROUTES: Mutex<Vec<(libc::c_int, mpsc::Sender<libc::c_int>)>> = Mutex::new(Vec::new());
static INIT: OnceLock<Result<(), String>> = OnceLock::new();

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
unsafe fn errno_ptr() -> *mut libc::c_int {
    libc::__error()
}

#[cfg(target_os = "linux")]
unsafe fn errno_ptr() -> *mut libc::c_int {
    libc::__errno_location()
}

extern "C" fn on_signal(sig: libc::c_int) {
    // Async-signal-safe: one write(2) to a non-blocking pipe; errno is preserved.
    // SAFETY: errno_ptr points at this thread's errno; `b` lives across the write.
    unsafe {
        let saved = *errno_ptr();
        let fd = WRITE_FD.load(Ordering::Relaxed);
        if fd >= 0 {
            let b = sig as u8;
            libc::write(fd, (&b as *const u8).cast(), 1);
        }
        *errno_ptr() = saved;
    }
}

fn set_flags(fd: libc::c_int, nonblock: bool) -> io::Result<()> {
    // SAFETY: fcntl on a pipe end we own.
    unsafe {
        if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
            return Err(io::Error::last_os_error());
        }
        if nonblock {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            if fl == -1 || libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) == -1 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

fn init() -> io::Result<()> {
    let r = INIT.get_or_init(|| {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: fds has room for the two descriptors pipe(2) writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        let [r, w] = fds;
        set_flags(r, false).and_then(|()| set_flags(w, true)).map_err(|e| e.to_string())?;
        WRITE_FD.store(w, Ordering::SeqCst);
        std::thread::Builder::new()
            .name("mirante-signals".into())
            .spawn(move || loop {
                let mut b = 0u8;
                // SAFETY: reads one byte into `b` from the pipe's read end, owned here.
                let n = unsafe { libc::read(r, (&mut b as *mut u8).cast(), 1) };
                if n == 1 {
                    let sig = libc::c_int::from(b);
                    let mut routes = ROUTES.lock().unwrap_or_else(|p| p.into_inner());
                    routes.retain(|(s, tx)| *s != sig || tx.send(sig).is_ok());
                } else if n == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return;
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    });
    r.clone().map_err(io::Error::other)
}

/// A channel that receives each of `signals` as it arrives (until the receiver is
/// dropped). Installs the process-wide handler for them: their default action (e.g.
/// terminate) no longer happens, the subscriber decides.
pub fn subscribe(signals: &[libc::c_int]) -> io::Result<mpsc::Receiver<libc::c_int>> {
    init()?;
    let (tx, rx) = mpsc::channel();
    {
        let mut routes = ROUTES.lock().unwrap_or_else(|p| p.into_inner());
        routes.extend(signals.iter().map(|s| (*s, tx.clone())));
    }
    for &sig in signals {
        // SAFETY: a zeroed sigaction is valid; the handler is an extern "C" fn(c_int).
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);
            if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_raised_signal_arrives_on_the_channel_of_its_subscribers_only() {
        let usr1 = subscribe(&[SIGUSR1]).unwrap();
        let usr2 = subscribe(&[SIGUSR2]).unwrap();
        // SAFETY: raise has no preconditions; SIGUSR1 is handled above.
        unsafe { libc::raise(SIGUSR1) };
        assert_eq!(usr1.recv_timeout(Duration::from_secs(2)).unwrap(), SIGUSR1);
        assert!(usr2.recv_timeout(Duration::from_millis(100)).is_err());
        // SAFETY: as above.
        unsafe { libc::raise(SIGUSR2) };
        assert_eq!(usr2.recv_timeout(Duration::from_secs(2)).unwrap(), SIGUSR2);
    }
}
