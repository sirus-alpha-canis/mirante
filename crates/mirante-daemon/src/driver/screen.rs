//! Reading Claude Code's TUI (2.1.285) off the emulated screen. Used ONLY for UI actions
//! (which key answers this dialog, is a dialog in the way of typed text, is the trust
//! prompt up, is the input box there). Status never comes from here: it comes from hooks
//! and the transcript (`mirante_core::status`). Dialog matching ported from
//! `spike_kit::screen`, whose fixtures are real 2.1.285 grids.
//!
//! Live dialogs sit at the bottom of the screen and end with a key-hint footer ("Esc to
//! cancel", "Enter to confirm · Esc to exit") in the last [`FOOTER_ROWS`] non-blank rows.
//! Only the last [`BOTTOM_ROWS`] non-blank rows are searched for the dialog, so the same
//! words in the conversation above (a quoted dialog, prose asking "Do you want to …?")
//! never count as a dialog.

use std::path::Path;

/// How many non-blank rows at the bottom of the screen a live dialog can occupy.
pub const BOTTOM_ROWS: usize = 20;
/// The key-hint footer of a live dialog is one of the last this many non-blank rows (a
/// status line may sit below it).
pub const FOOTER_ROWS: usize = 2;
/// How far above its question a dialog's tool box may start (a long, wrapped command).
const BOX_ROWS: usize = 40;

/// Cursor glyphs Claude Code draws on the selected option (and on the input prompt).
const CURSORS: [char; 3] = ['❯', '›', '>'];

fn non_blank(grid: &str) -> Vec<&str> {
    grid.lines().filter(|l| !l.trim().is_empty()).collect()
}

/// The last [`BOTTOM_ROWS`] non-blank lines of the screen.
fn bottom(grid: &str) -> Vec<&str> {
    let lines = non_blank(grid);
    lines[lines.len().saturating_sub(BOTTOM_ROWS)..].to_vec()
}

fn is_footer(l: &str) -> bool {
    l.contains("Esc to")
}

/// The index of the last line matching `pred` among the last [`FOOTER_ROWS`] lines.
fn footer_at_bottom(lines: &[&str], pred: impl Fn(&str) -> bool) -> Option<usize> {
    let from = lines.len().saturating_sub(FOOTER_ROWS);
    (from..lines.len()).rev().find(|&i| pred(lines[i]))
}

/// `(digit, label)` of a numbered option line (`❯ 1. Yes`, `  2. No`).
fn option(l: &str) -> Option<(char, &str)> {
    let t = l.trim().trim_start_matches(CURSORS).trim();
    let (num, rest) = t.split_once(". ")?;
    let digit = num.parse::<u8>().ok().filter(|d| (1..10).contains(d))?;
    Some((char::from(b'0' + digit), rest.trim()))
}

fn starts_with_cursor(l: &str) -> bool {
    l.trim_start().starts_with(CURSORS)
}

fn is_question(l: &str) -> bool {
    let t = l.trim();
    t.starts_with("Do you want to") && t.ends_with('?')
}

/// In the bottom rows: the index of the last permission-dialog header (`Do you want to …?`)
/// that is followed by a `1.` option and then a footer at the very bottom.
fn dialog_start(lines: &[&str]) -> Option<usize> {
    let footer = footer_at_bottom(lines, is_footer)?;
    (0..footer).rev().find(|&i| {
        is_question(lines[i]) && lines[i + 1..footer].iter().any(|l| option(l).is_some_and(|(d, _)| d == '1'))
    })
}

/// A permission dialog is open at the bottom of the screen.
pub fn has_permission_prompt(grid: &str) -> bool {
    dialog_start(&bottom(grid)).is_some()
}

/// Any selection dialog at the bottom of the screen: an option list with the cursor on an
/// option (`❯ N.`) and a footer at the very bottom. Covers dialogs Mirante does not know
/// (plan approval, model pickers): typed text must never land in one.
pub fn has_any_dialog(grid: &str) -> bool {
    let lines = bottom(grid);
    let Some(footer) = footer_at_bottom(&lines, |l| is_footer(l) || l.contains("Enter to confirm")) else {
        return false;
    };
    lines[..footer].iter().any(|l| starts_with_cursor(l) && option(l).is_some())
}

/// The digit of the live permission dialog's option whose label is exactly `label`
/// ("Yes" / "No"). Only options below the dialog header and above its footer count, and
/// only an exact label matches, so "Yes, and always allow …" and "Yes, and switch to auto
/// mode …" are never chosen (FINDINGS rule 6).
pub fn option_key(grid: &str, label: &str) -> Option<char> {
    let lines = bottom(grid);
    let start = dialog_start(&lines)?;
    let footer = footer_at_bottom(&lines, is_footer)?;
    lines[start + 1..footer].iter().find_map(|l| option(l).filter(|(_, t)| *t == label).map(|(d, _)| d))
}

pub const TRUST_YES: &str = "Yes, I trust this folder";

/// The digit of the trust dialog's exact "Yes, I trust this folder" option, when the
/// dialog is live: that option and a key-hint footer below it at the very bottom.
pub fn trust_key(grid: &str) -> Option<char> {
    let lines = bottom(grid);
    let footer = footer_at_bottom(&lines, |l| is_footer(l) || l.contains("Enter to confirm"))?;
    lines[..footer].iter().rev().find_map(|l| option(l).filter(|(_, t)| *t == TRUST_YES).map(|(d, _)| d))
}

/// Claude's workspace-trust dialog (new folder) is live. Its default option is "No, exit".
pub fn is_trust_dialog(grid: &str) -> bool {
    trust_key(grid).is_some()
}

/// Typed text would land in a dialog (permission, trust, or any other selection).
pub fn dialog_open(grid: &str) -> bool {
    has_permission_prompt(grid) || is_trust_dialog(grid) || has_any_dialog(grid)
}

/// The input box is on screen: a `❯` / `›` / `>` prompt line in the bottom rows that is not
/// a dialog option.
pub fn has_input_prompt(grid: &str) -> bool {
    bottom(grid).iter().any(|l| starts_with_cursor(l) && option(l).is_none())
}

/// The ready fast path: Mirante launches with `--permission-mode manual`, whose footer
/// reads "manual mode on".
pub fn has_manual_mode_footer(grid: &str) -> bool {
    bottom(grid).iter().any(|l| l.contains("manual mode on"))
}

/// The titles Claude Code (2.1.285) puts on the tool box of a permission dialog, per tool.
/// "Bash command" and "Edit file" are from real grids; the others are to be confirmed in
/// the Task 21 real run. A tool not listed here is never matched (answer it in attach).
fn titles(tool: &str) -> &'static [&'static str] {
    match tool {
        "Bash" => &["Bash command"],
        "Edit" | "MultiEdit" => &["Edit file"],
        "Write" => &["Create file", "Overwrite file", "Edit file"],
        "NotebookEdit" => &["Edit notebook"],
        "Read" => &["Read file"],
        "WebFetch" => &["Fetch"],
        _ => &[],
    }
}

const ALL_TITLES: [&str; 7] =
    ["Bash command", "Edit file", "Create file", "Overwrite file", "Edit notebook", "Read file", "Fetch"];

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// What the dialog box shows for `detail`: an absolute path inside the worktree is shown
/// relative to it (the whole relative path, never just the file name); anything else as is.
fn shown_detail(detail: &str, worktree: Option<&Path>) -> String {
    if let (true, Some(wt)) = (detail.starts_with('/'), worktree) {
        let canonical = std::fs::canonicalize(wt).ok();
        for root in std::iter::once(wt).chain(canonical.as_deref()) {
            if let Ok(rel) = Path::new(detail).strip_prefix(root) {
                if !rel.as_os_str().is_empty() {
                    return rel.to_string_lossy().into_owned();
                }
            }
        }
    }
    detail.to_string()
}

/// Is the dialog on screen exactly the one for this request (`tool` and its `detail`, a
/// command or a path)? All of it must hold:
/// - a permission dialog is live (question, options, footer at the very bottom);
/// - the nearest tool-box title above its question is one of `tool`'s titles, and no other
///   question sits between them;
/// - the box's first lines, whitespace removed (a command wraps across lines), are exactly
///   the whole detail, ending at a line boundary: a shared prefix, a longer command or a
///   file with the same name in another directory is another request;
/// - an empty detail never matches anything.
pub fn dialog_matches(grid: &str, tool: &str, detail: &str, worktree: Option<&Path>) -> bool {
    let needle = squash(&shown_detail(detail.trim(), worktree));
    if needle.is_empty() || titles(tool).is_empty() {
        return false;
    }
    let all = non_blank(grid);
    let offset = all.len().saturating_sub(BOTTOM_ROWS);
    let Some(question) = dialog_start(&all[offset..]).map(|i| i + offset) else {
        return false;
    };
    let from = question.saturating_sub(BOX_ROWS);
    let Some(title) = (from..question).rev().find(|&i| ALL_TITLES.contains(&all[i].trim())) else {
        return false;
    };
    if !titles(tool).contains(&all[title].trim()) || all[title + 1..question].iter().any(|l| is_question(l)) {
        return false;
    }
    let mut acc = String::new();
    for line in &all[title + 1..question] {
        acc.push_str(&squash(line));
        if acc == needle {
            return true;
        }
        if !needle.starts_with(&acc) {
            return false;
        }
    }
    false
}

/// Last `n` non-blank lines.
pub fn tail(grid: &str, n: usize) -> String {
    let lines: Vec<&str> = grid.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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

    const WT_DIR: &str = "/Users/u/.mirante/worktrees/app/feat";

    #[test]
    fn dialog_matches_the_request_detail() {
        assert!(dialog_matches(DIALOG, "Bash", "touch s3-allowed.txt", None));
        assert!(!dialog_matches(DIALOG, "Bash", "rm -rf /", None));
        assert!(dialog_matches(
            EDIT_DIALOG,
            "Edit",
            "/Users/u/.mirante/worktrees/app/feat/src/main.rs",
            Some(Path::new(WT_DIR))
        ));
        assert!(!dialog_matches(IDLE, "Bash", "touch s3-denied.txt", None));
        let wrapped = "Bash command\n  cargo test --workspace --all-\n  features -- --nocapture\nDo you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";
        assert!(dialog_matches(wrapped, "Bash", "cargo test --workspace --all-features -- --nocapture", None));
    }

    #[test]
    fn a_command_sharing_a_long_prefix_is_another_request() {
        let shown = "Bash command\n  cargo test --workspace --all-features -- --nocapture && rm -rf ~\nRun tests\nDo you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";
        // The first 30+ characters are the same: still not this request.
        assert!(!dialog_matches(shown, "Bash", "cargo test --workspace --all-features -- --nocapture", None));
        assert!(!dialog_matches(DIALOG, "Bash", "touch s3", None), "a prefix of the command is not the command");
    }

    #[test]
    fn the_same_file_name_in_another_directory_is_another_request() {
        assert!(!dialog_matches(
            EDIT_DIALOG,
            "Edit",
            "/Users/u/.mirante/worktrees/app/feat/tests/main.rs",
            Some(Path::new(WT_DIR))
        ));
        assert!(!dialog_matches(
            EDIT_DIALOG,
            "Edit",
            "/Users/u/.mirante/worktrees/app/feat/main.rs",
            Some(Path::new(WT_DIR))
        ));
        assert!(!dialog_matches(EDIT_DIALOG, "Edit", "/elsewhere/src/main.rs", Some(Path::new(WT_DIR))));
        // Relative paths are compared as they are.
        assert!(dialog_matches(EDIT_DIALOG, "Edit", "src/main.rs", Some(Path::new(WT_DIR))));
    }

    #[test]
    fn an_empty_detail_never_matches() {
        assert!(!dialog_matches(DIALOG, "Bash", "", None));
        assert!(!dialog_matches(DIALOG, "Bash", " \n ", None));
    }

    #[test]
    fn the_dialog_title_must_be_the_requests_tool() {
        assert!(!dialog_matches(DIALOG, "Edit", "touch s3-allowed.txt", None));
        assert!(!dialog_matches(EDIT_DIALOG, "Bash", "src/main.rs", Some(Path::new(WT_DIR))));
        assert!(!dialog_matches(DIALOG, "mcp__srv__run", "touch s3-allowed.txt", None), "unknown tools never match");
    }

    #[test]
    fn a_stale_card_never_matches_the_dialog_shown_now() {
        // Card A ("touch a") is still pending in the hub while Claude shows dialog B.
        let b = "⏺ Bash(touch a)\n  ⎿  done\n Bash command\n   touch b\n   Create b\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel";
        assert!(!dialog_matches(b, "Bash", "touch a", None));
        assert!(dialog_matches(b, "Bash", "touch b", None));
    }

    #[test]
    fn the_footer_must_be_at_the_very_bottom() {
        // A dialog quoted in the conversation, with live output and the prompt below it.
        let quoted = format!("{DIALOG}\n  more output\n❯ \n  ⏸ manual mode on");
        assert!(!has_permission_prompt(&quoted) && !dialog_open(&quoted));
        assert_eq!(option_key(&quoted, "Yes"), None);
        let trust = format!("{TRUST}\n❯ \n  ⏸ manual mode on");
        assert!(!is_trust_dialog(&trust));
        // One status line below the footer is still a live dialog.
        assert!(has_permission_prompt(&format!("{DIALOG}\n  ⏵⏵ status")));
    }

    #[test]
    fn other_cursor_glyphs_mark_options_too() {
        let alt = "Bash command\n  ls\nDo you want to proceed?\n› 1. Yes\n  2. No\nEsc to cancel";
        assert_eq!(option_key(alt, "Yes"), Some('1'));
        assert!(has_any_dialog(alt));
        let gt = "Would you like to proceed?\n> 1. Yes, and auto-accept edits\n  2. No\nEsc to cancel";
        assert!(has_any_dialog(gt) && dialog_open(gt));
        assert!(has_input_prompt("› \n"));
    }

    #[test]
    fn tail_keeps_the_last_non_blank_lines() {
        assert_eq!(tail("a\n\nb\n  \nc\n", 2), "b\nc");
    }
}
