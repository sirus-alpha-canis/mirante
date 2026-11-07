//! Terminal helpers for `attach`: raw mode (restored on drop), the window size, and the
//! reset of input modes an agent's TUI may have left on.
use std::io;

/// Turns off the input modes a TUI may have enabled on the user's terminal, so after
/// `attach` the shell does not receive mouse reports, focus events, bracketed-paste
/// markers or kitty key codes: bracketed paste (?2004), focus events (?1004), mouse
/// tracking and SGR mouse (?1000/?1002/?1003/?1006), the kitty keyboard protocol (`CSI <u`
/// pops it), application cursor keys (?1). Plus default attributes and a visible cursor.
pub const RESET_INPUT_MODES: &str =
    "\x1b[?2004l\x1b[?1004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[<u\x1b[?1l\x1b[0m\x1b[?25h";

/// The current attributes of the terminal on fd 0.
pub fn get_attrs() -> io::Result<libc::termios> {
    // SAFETY: termios is plain data; tcgetattr fills it for fd 0.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(t)
    }
}

/// Puts `attrs` (from [`get_attrs`]) back on fd 0. Safe to call from any thread.
pub fn restore(attrs: &libc::termios) {
    // SAFETY: attrs is a termios read by tcgetattr.
    unsafe {
        libc::tcsetattr(0, libc::TCSANOW, attrs);
    }
}

pub struct RawMode {
    orig: libc::termios,
}

impl RawMode {
    pub fn enable() -> io::Result<RawMode> {
        let orig = get_attrs()?;
        // SAFETY: termios is plain data copied from tcgetattr's result.
        unsafe {
            let mut raw = orig;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(0, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(RawMode { orig })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        restore(&self.orig);
    }
}

pub fn is_tty(fd: i32) -> bool {
    // SAFETY: isatty has no preconditions.
    unsafe { libc::isatty(fd) == 1 }
}

/// (rows, cols) of the terminal on stdout, or 40x120 when unknown.
pub fn size() -> (u16, u16) {
    // SAFETY: winsize is plain data filled by the ioctl.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            (ws.ws_row, ws.ws_col)
        } else {
            (40, 120)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reset_turns_off_every_input_mode_a_tui_may_leave_on() {
        for seq in ["?2004l", "?1004l", "?1000l", "?1002l", "?1003l", "?1006l", "<u", "?1l", "?25h"] {
            assert!(RESET_INPUT_MODES.contains(&format!("\x1b[{seq}")), "{seq}");
        }
        assert!(!RESET_INPUT_MODES.contains("?1049"), "must not leave or enter the alternate screen");
    }
}
