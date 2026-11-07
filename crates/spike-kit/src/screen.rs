//! Status as a human would read it off the Claude Code TUI (2.1.285), used to check the
//! hook-derived status. Markers were taken from real screens (docs/spikes/raw/s3-status-check.md).
use crate::status::Status;

/// Permission dialog for a tool outside the allowlist.
pub const PERMISSION_MARKER: &str = "Do you want to proceed?";
const SPINNER_GLYPHS: [char; 7] = ['·', '✢', '✳', '✶', '✻', '✽', '*'];

/// A spinner line: `<glyph> <Verb>… (...)` while a turn runs; the ellipsis must be followed
/// by the timing suffix ` (`, so a markdown bullet like `* Loading…` does not count. After
/// the turn the same glyph prefixes `<Verb>ed for 3s`, which has no ellipsis.
pub fn has_spinner(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with(SPINNER_GLYPHS) && t.contains("… (")
    })
}

/// Index of the last permission dialog: a `Do you want to …?` line (Bash: "proceed", Edit /
/// Write: "make this edit", …) followed later by a numbered `1.` option line.
fn dialog_start(lines: &[&str]) -> Option<usize> {
    let is_option_one = |l: &str| l.trim_start().trim_start_matches('❯').trim_start().starts_with("1. ");
    (0..lines.len()).rev().find(|&i| {
        let t = lines[i].trim();
        t.starts_with("Do you want to") && t.ends_with('?') && lines[i + 1..].iter().any(|l| is_option_one(l))
    })
}

pub fn has_permission_prompt(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    dialog_start(&lines).is_some()
}

/// Screen-derived status; `exited` = the claude process is gone.
pub fn status(text: &str, exited: bool) -> Status {
    if exited {
        Status::Ended
    } else if has_permission_prompt(text) {
        Status::NeedsYou
    } else if has_spinner(text) {
        Status::Working
    } else {
        Status::Idle
    }
}

/// The digit of the numbered dialog option whose label is exactly `label` (e.g. "Yes", "No"),
/// ignoring longer labels such as "Yes, and don't ask again ...". Lines may carry a `❯` cursor.
pub fn option_key(text: &str, label: &str) -> Option<char> {
    let lines: Vec<&str> = text.lines().collect();
    // Only the live dialog counts: history above it may contain look-alike "1. Yes" lines.
    let start = dialog_start(&lines)?;
    lines[start + 1..].iter().find_map(|l| {
        let t = l.trim().trim_start_matches('❯').trim();
        let (num, rest) = t.split_once(". ")?;
        let digit = num.parse::<u8>().ok().filter(|d| *d < 10)?;
        (rest.trim() == label).then(|| char::from(b'0' + digit))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real grid excerpts (vt100-rendered, Claude Code 2.1.285, run 1 of the S3 check).
    const DIALOG: &str = "\
❯ Use o Bash para rodar exatamente touch s3-allowed.txt e diga se deu certo.
  Creating empty file s3-allowed.txt
  ⎿  $ touch s3-allowed.txt
 Bash command
   touch s3-allowed.txt
   Create empty file s3-allowed.txt
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /home/u/s3 from this project
   3. Yes, and switch to auto mode · auto mode handles these prompts for you
   4. No
 Esc to cancel · Tab to amend";
    const IDLE: &str = "\
❯ Use o Bash para rodar exatamente touch s3-denied.txt e diga se deu certo.
  Ran 1 shell command
  ⎿  Interrupted · What should Claude do instead?
✻ Brewed for 1s · done 1:47 PM
❯
  ⏸ manual mode on · ← for agents";
    const WORKING: &str = "\
⏺ Waiting 2 seconds then printing a
  ⎿  $ sleep 2 && echo a
✶ Skedaddling… (2s · ↓ 195 tokens · thought for 1s)
❯
  ⏸ manual mode on · ← for agents";

    #[test]
    fn dialog_is_needs_you() {
        assert_eq!(status(DIALOG, false), Status::NeedsYou);
    }

    #[test]
    fn spinner_with_ellipsis_is_working() {
        assert!(has_spinner(WORKING));
        assert_eq!(status(WORKING, false), Status::Working);
    }

    #[test]
    fn finished_turn_summary_and_tool_running_line_are_not_spinners() {
        assert!(!has_spinner(IDLE));
        assert!(!has_spinner("  ⎿  Running…"));
        assert_eq!(status(IDLE, false), Status::Idle);
    }

    #[test]
    fn exited_process_is_ended_whatever_the_screen() {
        assert_eq!(status(DIALOG, true), Status::Ended);
    }

    const EDIT_DIALOG: &str = "\
 Edit file
   src/main.rs
 Do you want to make this edit to main.rs?
 ❯ 1. Yes
   2. Yes, allow all edits during this session
   3. No
 Esc to cancel";

    #[test]
    fn edit_dialog_is_needs_you() {
        assert!(has_permission_prompt(EDIT_DIALOG));
        assert!(has_permission_prompt(DIALOG));
        assert_eq!(status(EDIT_DIALOG, false), Status::NeedsYou);
        assert_eq!(option_key(EDIT_DIALOG, "No"), Some('3'));
    }

    #[test]
    fn question_without_numbered_option_is_not_a_dialog() {
        assert!(!has_permission_prompt("Do you want to proceed?\nsure"));
    }

    #[test]
    fn option_key_ignores_lookalikes_above_the_dialog() {
        let screen = format!("1. Yes\n4. No\n{EDIT_DIALOG}");
        assert_eq!(option_key(&screen, "Yes"), Some('1'));
        assert_eq!(option_key(&screen, "No"), Some('3'));
        assert_eq!(option_key("1. Yes\n4. No\nno dialog here", "Yes"), None);
    }

    #[test]
    fn bullet_without_timing_suffix_is_not_a_spinner() {
        assert!(!has_spinner("* Loading…"));
        assert!(has_spinner("* Loading… (2s)"));
    }

    #[test]
    fn option_key_matches_exact_labels_only() {
        assert_eq!(option_key(DIALOG, "Yes"), Some('1'));
        assert_eq!(option_key(DIALOG, "No"), Some('4'));
        assert_eq!(option_key(IDLE, "Yes"), None);
    }
}
