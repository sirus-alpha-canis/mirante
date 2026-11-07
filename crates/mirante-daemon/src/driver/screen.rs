//! Reading Claude Code's TUI (2.1.285) off the emulated screen. Used ONLY for UI actions
//! (which key answers this dialog, is a dialog in the way of typed text, is the trust
//! prompt up, is the input box there). Status never comes from here: it comes from hooks
//! and the transcript (`mirante_core::status`). Dialog matching ported from
//! `spike_kit::screen`, whose fixtures are real 2.1.285 grids.
//!
//! Live dialogs sit at the bottom of the screen and end with a key-hint footer ("Esc to
//! cancel", "Enter to confirm · Esc to exit"). Only the last [`BOTTOM_ROWS`] non-blank
//! rows are searched and a footer is required, so the same words in the conversation above
//! (a quoted dialog, prose asking "Do you want to …?") never count as a dialog.

/// How many non-blank rows at the bottom of the screen a live dialog can occupy.
pub const BOTTOM_ROWS: usize = 20;

/// The last [`BOTTOM_ROWS`] non-blank lines of the screen.
fn bottom(grid: &str) -> Vec<&str> {
    let lines: Vec<&str> = grid.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(BOTTOM_ROWS)..].to_vec()
}

fn is_footer(l: &str) -> bool {
    l.contains("Esc to")
}

/// `(digit, label)` of a numbered option line (`❯ 1. Yes`, `  2. No`).
fn option(l: &str) -> Option<(char, &str)> {
    let t = l.trim().trim_start_matches('❯').trim();
    let (num, rest) = t.split_once(". ")?;
    let digit = num.parse::<u8>().ok().filter(|d| (1..10).contains(d))?;
    Some((char::from(b'0' + digit), rest.trim()))
}

/// In the bottom rows: the index of the last permission-dialog header (`Do you want to …?`)
/// that is followed by a `1.` option and then a footer.
fn dialog_start(lines: &[&str]) -> Option<usize> {
    let footer = lines.iter().rposition(|l| is_footer(l))?;
    (0..footer).rev().find(|&i| {
        let t = lines[i].trim();
        t.starts_with("Do you want to")
            && t.ends_with('?')
            && lines[i + 1..footer].iter().any(|l| option(l).is_some_and(|(d, _)| d == '1'))
    })
}

/// A permission dialog is open at the bottom of the screen.
pub fn has_permission_prompt(grid: &str) -> bool {
    dialog_start(&bottom(grid)).is_some()
}

/// Any selection dialog at the bottom of the screen: an option list with the cursor on an
/// option (`❯ N.`) and a footer below it. Covers dialogs Mirante does not know (plan
/// approval, model pickers): typed text must never land in one.
pub fn has_any_dialog(grid: &str) -> bool {
    let lines = bottom(grid);
    let Some(footer) = lines.iter().rposition(|l| is_footer(l) || l.contains("Enter to confirm")) else {
        return false;
    };
    lines[..footer].iter().any(|l| l.trim_start().starts_with('❯') && option(l).is_some())
}

/// The digit of the live permission dialog's option whose label is exactly `label`
/// ("Yes" / "No"). Only options below the dialog header and above its footer count, and
/// only an exact label matches, so "Yes, and always allow …" and "Yes, and switch to auto
/// mode …" are never chosen (FINDINGS rule 6).
pub fn option_key(grid: &str, label: &str) -> Option<char> {
    let lines = bottom(grid);
    let start = dialog_start(&lines)?;
    let footer = lines.iter().rposition(|l| is_footer(l))?;
    lines[start + 1..footer].iter().find_map(|l| option(l).filter(|(_, t)| *t == label).map(|(d, _)| d))
}

pub const TRUST_YES: &str = "Yes, I trust this folder";

/// The digit of the trust dialog's exact "Yes, I trust this folder" option, when the
/// dialog is live: that option and a key-hint footer below it in the bottom rows.
pub fn trust_key(grid: &str) -> Option<char> {
    let lines = bottom(grid);
    let i = lines.iter().position(|l| option(l).is_some_and(|(_, t)| t == TRUST_YES))?;
    let footer = lines[i + 1..].iter().any(|l| is_footer(l) || l.contains("Enter to confirm"));
    footer.then(|| option(lines[i]).map(|(d, _)| d)).flatten()
}

/// Claude's workspace-trust dialog (new folder) is live. Its default option is "No, exit".
pub fn is_trust_dialog(grid: &str) -> bool {
    trust_key(grid).is_some()
}

/// Typed text would land in a dialog (permission, trust, or any other selection).
pub fn dialog_open(grid: &str) -> bool {
    has_permission_prompt(grid) || is_trust_dialog(grid) || has_any_dialog(grid)
}

/// The input box is on screen: a `❯` / `>` prompt line in the bottom rows that is not a
/// dialog option.
pub fn has_input_prompt(grid: &str) -> bool {
    bottom(grid).iter().any(|l| {
        let t = l.trim_start();
        (t.starts_with('❯') || t.starts_with('>')) && option(l).is_none()
    })
}

/// The ready fast path: Mirante launches with `--permission-mode manual`, whose footer
/// reads "manual mode on".
pub fn has_manual_mode_footer(grid: &str) -> bool {
    bottom(grid).iter().any(|l| l.contains("manual mode on"))
}

/// Does the dialog on screen concern `detail` (a permission request's command or path)?
/// Compared without whitespace, so a command wrapped across lines still matches: the
/// first 30 characters of the detail, or, for an absolute path, its file name (the dialog
/// shows paths relative to the project). An empty detail matches any dialog.
pub fn dialog_matches(grid: &str, detail: &str) -> bool {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let detail = detail.lines().next().unwrap_or("").trim();
    let lines = bottom(grid);
    let Some(start) = dialog_start(&lines) else {
        return false;
    };
    if detail.is_empty() {
        return true;
    }
    // The tool box sits right above the question: search up to 15 lines before it.
    let from = start.saturating_sub(15);
    let hay = squash(&lines[from..=start].join("\n"));
    let head: String = squash(detail).chars().take(30).collect();
    let name = detail.starts_with('/').then(|| detail.rsplit('/').next().unwrap_or("")).filter(|n| !n.is_empty());
    hay.contains(&head) || name.is_some_and(|n| hay.contains(&squash(n)))
}

/// Last `n` non-blank lines.
pub fn tail(grid: &str, n: usize) -> String {
    let lines: Vec<&str> = grid.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real grid excerpts (vt100-rendered, Claude Code 2.1.285, S3 run 1).
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
  ⎿  Interrupted · What should Claude do instead?
✻ Brewed for 1s · done 1:47 PM
❯
  ⏸ manual mode on · ← for agents";
    const EDIT_DIALOG: &str = "\
 Edit file
   src/main.rs
 Do you want to make this edit to main.rs?
 ❯ 1. Yes
   2. Yes, allow all edits during this session
   3. No
 Esc to cancel";
    const TRUST: &str = "\
 Do you trust the files in this folder?
 /Users/u/.mirante/worktrees/app/feat
 ❯ 1. No, exit
   2. Yes, I trust this folder
 Enter to confirm · Esc to exit";
    const PLAN: &str = "\
 Would you like to proceed?
 ❯ 1. Yes, and auto-accept edits
   2. Yes, and manually approve edits
   3. No, keep planning
 ctrl-g to edit in editor · Esc to cancel";

    #[test]
    fn exact_one_time_options_only() {
        assert_eq!(option_key(DIALOG, "Yes"), Some('1'));
        assert_eq!(option_key(DIALOG, "No"), Some('4'));
        assert_eq!(option_key(EDIT_DIALOG, "No"), Some('3'));
        assert_eq!(option_key(IDLE, "Yes"), None);
    }

    #[test]
    fn lookalike_options_above_the_dialog_are_ignored() {
        let screen = format!("1. Yes\n4. No\n{EDIT_DIALOG}");
        assert_eq!(option_key(&screen, "No"), Some('3'));
        assert_eq!(option_key("1. Yes\n4. No\nno dialog here", "Yes"), None);
    }

    #[test]
    fn dialogs_need_a_footer_in_the_bottom_rows() {
        assert!(has_permission_prompt(DIALOG) && has_permission_prompt(EDIT_DIALOG));
        assert!(!has_permission_prompt(IDLE));
        // Prose that asks the same question is not a dialog: no footer.
        assert!(!has_permission_prompt("Do you want to proceed?\n1. Yes\n2. No\n❯ "));
        // A real dialog scrolled up above the bottom rows is history, not a live dialog.
        let scrolled = format!("{DIALOG}\n{}❯\n  ⏸ manual mode on", "output line\n".repeat(BOTTOM_ROWS));
        assert!(!has_permission_prompt(&scrolled) && !dialog_open(&scrolled));
    }

    #[test]
    fn the_trust_dialog_is_the_exact_option_with_a_footer() {
        assert!(is_trust_dialog(TRUST));
        assert_eq!(trust_key(TRUST), Some('2'));
        assert!(dialog_open(TRUST));
        // The question quoted in the conversation is not the dialog.
        let prose = "❯ Do you trust the files in this folder? asked the README\n  Yes, I trust this folder\n❯ \n  ⏸ manual mode on";
        assert!(!is_trust_dialog(prose));
        // Without a footer below the option there is no live dialog.
        assert!(!is_trust_dialog(" ❯ 1. No, exit\n   2. Yes, I trust this folder"));
    }

    #[test]
    fn plan_approval_has_no_plain_yes_but_still_blocks_typing() {
        assert_eq!(option_key(PLAN, "Yes"), None);
        assert!(!has_permission_prompt(PLAN));
        assert!(has_any_dialog(PLAN) && dialog_open(PLAN));
    }

    #[test]
    fn the_input_prompt_and_the_ready_fast_path() {
        assert!(has_input_prompt(IDLE) && has_manual_mode_footer(IDLE));
        assert!(!dialog_open(IDLE));
        assert!(has_input_prompt("> \n"));
        assert!(!has_input_prompt(" ❯ 1. Yes\n Esc to cancel"));
        assert!(!has_manual_mode_footer(TRUST));
    }

    #[test]
    fn dialog_matches_the_request_detail() {
        assert!(dialog_matches(DIALOG, "touch s3-allowed.txt"));
        assert!(!dialog_matches(DIALOG, "rm -rf /"));
        assert!(dialog_matches(EDIT_DIALOG, "/Users/u/.mirante/worktrees/app/feat/src/main.rs"));
        assert!(dialog_matches(DIALOG, ""));
        assert!(!dialog_matches(IDLE, "touch s3-denied.txt"));
        let wrapped = "Bash command\n  cargo test --workspace --all-\n  features -- --nocapture\nDo you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";
        assert!(dialog_matches(wrapped, "cargo test --workspace --all-features -- --nocapture"));
    }

    #[test]
    fn tail_keeps_the_last_non_blank_lines() {
        assert_eq!(tail("a\n\nb\n  \nc\n", 2), "b\nc");
    }
}
