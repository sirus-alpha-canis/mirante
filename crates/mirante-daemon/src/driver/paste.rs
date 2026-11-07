//! Turning a user's message into the bytes typed into Claude Code's input box.
//!
//! A message is sent as one bracketed paste (`ESC[200~` … `ESC[201~`), so newlines stay
//! inside the message instead of submitting it early, and the text is sanitized first:
//! nothing in it can act as a terminal control (no escape sequences, so a smuggled
//! `ESC[201~` cannot end the paste early; no control characters). Two prefixes are special:
//! - `/…` is a Claude slash command. It is typed as plain keystrokes (no paste) so Claude
//!   runs it as the command the user wrote. It must be one line.
//! - `!…` is Claude's bash mode, which runs a shell command with no permission prompt.
//!   Mirante refuses it: a relayed message must never run a shell command unasked; the user
//!   attaches and types it.

pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// The text with every terminal control removed: CRLF and lone CR become LF; CSI, OSC and
/// other escape sequences are dropped whole; C0 controls except LF and TAB, DEL and C1
/// controls (U+0080–U+009F) are dropped; TAB becomes 4 spaces. All other characters,
/// including non-ASCII and emoji, are kept.
pub fn sanitize_for_pty(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            '\x1b' => match chars.next() {
                // CSI: parameters and intermediates up to a final byte @..~.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC, DCS, SOS, PM, APC: a string up to BEL or ST (ESC \).
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Any other escape: ESC plus one character.
                _ => {}
            },
            c if (c as u32) < 0x20 || c == '\x7f' => {}
            c if (0x80..=0x9f).contains(&(c as u32)) => {}
            c => out.push(c),
        }
    }
    out
}

/// `text` sanitized and framed as one bracketed paste.
pub fn bracketed(text: &str) -> Vec<u8> {
    let mut out = PASTE_START.to_vec();
    out.extend_from_slice(sanitize_for_pty(text).as_bytes());
    out.extend_from_slice(PASTE_END);
    out
}

/// The bytes to type for a message (Enter is sent separately), or why it is refused.
pub fn typed_message(text: &str) -> Result<Vec<u8>, String> {
    let clean = sanitize_for_pty(text);
    let lead = clean.trim_start();
    if lead.trim().is_empty() {
        return Err("the message is empty once control characters are removed".into());
    }
    if lead.starts_with('!') {
        return Err("a message starting with `!` would run a shell command in Claude's bash mode without a \
                    permission prompt; Mirante does not send it (attach and type it yourself)"
            .into());
    }
    if lead.starts_with('/') {
        let line = lead.trim_end();
        if line.contains('\n') {
            return Err("a slash command must be one line".into());
        }
        return Ok(line.as_bytes().to_vec());
    }
    Ok(bracketed(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_and_smuggled_paste_end_are_removed_newlines_and_emoji_kept() {
        let framed = bracketed("a\r\nb\x1b[201~c\td\x00e🙂");
        assert_eq!(framed, "\x1b[200~a\nbc    de🙂\x1b[201~".as_bytes());
    }

    #[test]
    fn every_kind_of_control_is_dropped() {
        assert_eq!(sanitize_for_pty("x\ry"), "x\ny");
        assert_eq!(sanitize_for_pty("t\x1b]0;title\x07u\x1b]8;;http://e\x1b\\v"), "tuv");
        assert_eq!(sanitize_for_pty("p\x1bPq data\x1b\\r"), "pr");
        assert_eq!(sanitize_for_pty("a\x1bcb\x1b"), "ab");
        assert_eq!(sanitize_for_pty("1\u{9b}2\u{85}3\x7f4\x085"), "12345");
        assert_eq!(sanitize_for_pty("ação ✓ 日本"), "ação ✓ 日本");
    }

    #[test]
    fn slash_commands_are_keystrokes_and_bang_is_refused() {
        assert_eq!(typed_message("  /compact keep the plan\n").unwrap(), b"/compact keep the plan".to_vec());
        assert!(typed_message("/compact\nmore").unwrap_err().contains("one line"));
        assert!(typed_message("!rm -rf build").unwrap_err().contains("attach and type it yourself"));
        assert!(typed_message("\x1b[31m\x00").unwrap_err().contains("empty"));
        assert_eq!(typed_message("use tabs\nthen test").unwrap(), b"\x1b[200~use tabs\nthen test\x1b[201~".to_vec());
    }
}
