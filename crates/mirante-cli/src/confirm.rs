//! Typed confirmation of a person's decision on the controlling terminal (`/dev/tty`),
//! never on stdin: a plain pipe cannot answer it, and a process without a terminal (a
//! cron job, `setsid`) is refused before anything is asked.
//!
//! An intent check, not a security boundary: anything that can allocate a pseudo-terminal
//! (`script`, `expect`, Python's `pty.spawn`) can type the word, and an agent's tool calls
//! may run on the agent's own terminal. The boundary is the daemon's `require_person`: it
//! refuses Mirante-launched agents, including a process whose controlling terminal is a live
//! agent session's, and the CLI asks it (`dna.may_review`) before showing or asking anything.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};

/// Longest answer read (bytes); the rest of the line is discarded.
const MAX_ANSWER: usize = 256;

/// Where the question is asked (the test seam: tests pass a scripted terminal).
pub trait Tty {
    /// Writes `prompt`, then reads one line (with its newline, if any).
    fn ask(&mut self, prompt: &str) -> io::Result<String>;
}

/// The controlling terminal.
pub struct DevTty(File);

impl DevTty {
    /// Opens `/dev/tty`; fails when this process has no controlling terminal.
    pub fn open() -> Result<DevTty, String> {
        OpenOptions::new().read(true).write(true).open("/dev/tty").map(DevTty).map_err(no_tty)
    }
}

impl Tty for DevTty {
    fn ask(&mut self, prompt: &str) -> io::Result<String> {
        self.0.write_all(prompt.as_bytes())?;
        self.0.flush()?;
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while self.0.read(&mut byte)? == 1 {
            if line.len() < MAX_ANSWER {
                line.push(byte[0]);
            }
            if byte[0] == b'\n' {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&line).into_owned())
    }
}

/// The refusal when there is no terminal to ask on.
pub fn no_tty(e: io::Error) -> String {
    format!(
        "a review (memory or skills) needs you at a terminal: it asks on /dev/tty, and this process has none ({e}). \
Review in the cockpit, or run this in a terminal you opened yourself (Mirante-launched agents are refused \
either way)"
    )
}

/// Shows `summary` and asks for `word`, typed exactly; anything else decides nothing.
pub fn typed(tty: &mut dyn Tty, summary: &str, word: &str) -> Result<(), String> {
    let answer = tty
        .ask(&format!("{summary}\n\nType {word} to confirm (anything else cancels): "))
        .map_err(|e| format!("could not read the terminal: {e}; nothing was decided"))?;
    if answer.trim_end_matches(['\n', '\r']) == word {
        Ok(())
    } else {
        Err("not confirmed; nothing was decided".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Typed(Vec<String>, String);

    impl Tty for Typed {
        fn ask(&mut self, prompt: &str) -> std::io::Result<String> {
            self.1.push_str(prompt);
            Ok(self.0.remove(0))
        }
    }

    #[test]
    fn the_word_must_be_typed_exactly() {
        let mut tty = Typed(vec!["accept\n".into()], String::new());
        typed(&mut tty, "Proposal p1: Hook spool", "accept").unwrap();
        assert!(tty.1.contains("Proposal p1: Hook spool") && tty.1.contains("Type accept to confirm"), "{}", tty.1);
        for wrong in ["y\n", "yes\n", "", "Accept\n", "reject\n"] {
            let mut tty = Typed(vec![wrong.into()], String::new());
            let e = typed(&mut tty, "p", "accept").unwrap_err();
            assert!(e.contains("nothing was decided"), "{e}");
        }
    }

    #[test]
    fn no_terminal_is_a_refusal_that_says_where_to_review() {
        let e = no_tty(std::io::Error::from_raw_os_error(libc::ENXIO));
        assert!(e.contains("/dev/tty") && e.contains("cockpit") && e.contains("Mirante-launched agents"), "{e}");
    }
}
