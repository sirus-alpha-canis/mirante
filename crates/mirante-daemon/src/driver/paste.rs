//! Turning a user's message into the bytes typed into Claude Code's input box.
//!
//! A message is sent as one bracketed paste (`ESC[200~` … `ESC[201~`), so newlines stay
//! inside the message instead of submitting it early, and the text is sanitized first:
//! nothing in it can act as a terminal control (no escape sequences, so a smuggled
//! `ESC[201~` cannot end the paste early; no control characters; no bidi or zero-width
//! characters that could hide a prefix). Some first characters are special:
//! - `/…` is a Claude slash command. Only `/` and the command name are typed as keystrokes,
//!   so Claude runs the command the user named; its arguments follow as a bracketed paste,
//!   so nothing in them can act as a key. It must be one line. The name is a letter, then
//!   letters, `:`, `_` or `-` (`[A-Za-z][A-Za-z:_-]*`): never a digit anywhere, because a
//!   digit typed as a keystroke selects a numbered option if a dialog opened under it
//!   (`/1`, `/2 x`). A command whose name has a digit is typed by the user in attach.
//! - `!…` is Claude's bash mode, which runs a shell command with no permission prompt.
//!   Mirante refuses it: a relayed message must never run a shell command unasked; the user
//!   attaches and types it.
//! - `#…` saves the line to Claude's memory files and `&…` hands it to a background run,
//!   both without a prompt: refused the same way.

pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// Invisible characters that reorder or hide text: bidi controls and zero-width characters.
fn invisible(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}'
        | '\u{061c}' | '\u{feff}')
}

/// The text with every terminal control removed: CRLF and lone CR become LF; CSI, OSC and
/// other escape sequences are dropped whole; C0 controls except LF and TAB, DEL and C1
/// controls (U+0080–U+009F) are dropped, and so are bidi controls and zero-width
/// characters; TAB becomes 4 spaces. All other characters, including non-ASCII and emoji,
/// are kept.
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
            c if (0x80..=0x9f).contains(&(c as u32)) || invisible(c) => {}
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

/// Lines in `text` once sanitized (what a paste of it leaves in the input box).
pub fn line_count(text: &str) -> usize {
    sanitize_for_pty(text).lines().count().max(1)
}

/// `[A-Za-z][A-Za-z:_-]*`: no digit anywhere (a typed digit can select a dialog option).
fn command_name_ok(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphabetic() || matches!(c, ':' | '_' | '-'))
}

/// The bytes to type for a message (Enter is sent separately), or why it is refused.
pub fn typed_message(text: &str) -> Result<Vec<u8>, String> {
    let clean = sanitize_for_pty(text);
    let lead = clean.trim_start();
    if lead.trim().is_empty() {
        return Err("the message is empty once control characters are removed".into());
    }
    match lead.chars().next() {
        Some('!') => {
            return Err("a message starting with `!` would run a shell command in Claude's bash mode without a \
                        permission prompt; Mirante does not send it (attach and type it yourself)"
                .into())
        }
        Some('#') => {
            return Err("a message starting with `#` would be saved to Claude's memory files without a prompt; \
                        Mirante does not send it (attach and type it yourself)"
                .into())
        }
        Some('&') => {
            return Err("a message starting with `&` would be handed to a background run without a prompt; \
                        Mirante does not send it (attach and type it yourself)"
                .into())
        }
        _ => {}
    }
    if let Some(rest) = lead.strip_prefix('/') {
        let line = rest.trim_end();
        if line.contains('\n') {
            return Err("a slash command must be one line".into());
        }
        let (name, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        if !command_name_ok(name) {
            return Err(format!(
                "`/{name}` is not a slash command Mirante types (a letter, then letters, `:`, `_`, `-`; no \
                 digits); attach and type it yourself"
            ));
        }
        let mut out = format!("/{name}").into_bytes();
        let args = args.trim();
        if !args.is_empty() {
            out.push(b' ');
            out.extend_from_slice(&bracketed(args));
        }
        return Ok(out);
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
        assert_eq!(typed_message("  /compact\n").unwrap(), b"/compact".to_vec());
        assert!(typed_message("/compact\nmore").unwrap_err().contains("one line"));
        assert!(typed_message("!rm -rf build").unwrap_err().contains("attach and type it yourself"));
        assert!(typed_message("\x1b[31m\x00").unwrap_err().contains("empty"));
        assert_eq!(typed_message("use tabs\nthen test").unwrap(), b"\x1b[200~use tabs\nthen test\x1b[201~".to_vec());
    }

    #[test]
    fn slash_command_arguments_are_pasted_not_typed() {
        // Only "/name " is typed; the arguments cannot act as keys (a digit, Esc, Tab, a
        // second slash command) because they arrive as one bracketed paste.
        assert_eq!(
            typed_message("  /compact keep the plan\n").unwrap(),
            [b"/compact ".as_slice(), b"\x1b[200~keep the plan\x1b[201~"].concat()
        );
        assert_eq!(typed_message("/mcp__srv:do-it x").unwrap()[..16], *b"/mcp__srv:do-it ");
        for bad in ["/", "/ compact", "/comp;act", "/../x", "/com\u{200b}pact!", "/é", "/mcp__srv:do-it_2 x", "/-x"] {
            assert!(typed_message(bad).unwrap_err().contains("slash command"), "{bad:?}");
        }
    }

    #[test]
    fn prefixes_that_act_without_a_prompt_are_refused() {
        // `#` writes to Claude's memory files; `&` hands the line to a background run.
        assert!(typed_message("# always use tabs").unwrap_err().contains("memory"));
        assert!(typed_message("  & build it").unwrap_err().contains("&"));
        // Hidden characters cannot smuggle a prefix past the check.
        assert!(typed_message("\u{200b}!rm -rf build").unwrap_err().contains("attach and type it yourself"));
        assert!(typed_message("\u{feff}\u{2066}#x").is_err());
        // Inside the text they are harmless.
        assert!(typed_message("fix issue #12 & ship").is_ok());
    }

    #[test]
    fn bidi_and_zero_width_characters_are_dropped() {
        assert_eq!(sanitize_for_pty("a\u{200b}b\u{200c}c\u{200d}d\u{2060}e\u{feff}f"), "abcdef");
        assert_eq!(sanitize_for_pty("x\u{202e}y\u{2066}z\u{2069}\u{200e}\u{200f}\u{61c}w"), "xyzw");
        assert_eq!(sanitize_for_pty("ação 🙂"), "ação 🙂");
    }

    #[test]
    fn line_count_of_the_sanitized_text() {
        assert_eq!(line_count("one"), 1);
        assert_eq!(line_count("a\r\nb\rc\n"), 3);
    }
}

/// Ported from the driver review's probe (`digit_names_are_keystrokes`), which showed digits
/// in a slash command name typed as keystrokes: each must now be refused.
#[cfg(test)]
mod probes {
    #[test]
    fn digit_names_are_refused_not_typed() {
        for bad in ["/1", "/2 x", "/9", "/a1", "/model2 x"] {
            let e = super::typed_message(bad).unwrap_err();
            assert!(e.contains("no digits") && e.contains("attach"), "{bad:?}: {e}");
        }
        // A digit in the arguments is pasted, never typed as a key (whether a bracketed paste
        // of a bare digit can select an option is a Task 21 real-run check).
        assert_eq!(super::typed_message("/model 2").unwrap(), [b"/model ".as_slice(), b"\x1b[200~2\x1b[201~"].concat());
    }
}
