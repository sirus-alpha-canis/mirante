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

/// A drawn box border: a row of `─` (Claude Code draws one above a dialog's tool box and
/// around the input box; real 2.1.285 grids in `docs/spikes/raw/s3-status-check.md`).
fn is_border(l: &str) -> bool {
    let t = l.trim();
    t.chars().count() >= 3 && t.chars().all(|c| c == '─')
}

/// The input prompt (a cursor line that is not a numbered option).
fn is_input_prompt(l: &str) -> bool {
    starts_with_cursor(l) && option(l).is_none()
}

/// In the bottom rows: the index of the last permission-dialog header (`Do you want to …?`)
/// that is followed by a `1.` option and then a footer at the very bottom, with nothing
/// between the header and the footer that belongs outside a dialog (an input prompt or a
/// box border): a dialog quoted in the conversation above the input box, with an
/// "Esc to …" status line below, is not a live dialog.
fn dialog_start(lines: &[&str]) -> Option<usize> {
    let footer = footer_at_bottom(lines, is_footer)?;
    (0..footer).rev().find(|&i| {
        let below = &lines[i + 1..footer];
        is_question(lines[i])
            && below.iter().any(|l| option(l).is_some_and(|(d, _)| d == '1'))
            && !below.iter().any(|l| is_input_prompt(l) || is_border(l))
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
    bottom(grid).iter().any(|l| is_input_prompt(l))
}

/// The ready fast path: Mirante launches with `--permission-mode manual`, whose footer
/// reads "manual mode on".
pub fn has_manual_mode_footer(grid: &str) -> bool {
    bottom(grid).iter().any(|l| l.contains("manual mode on"))
}

/// The titles Claude Code (2.1.285) puts on the tool box of a permission dialog, per tool.
/// "Bash command" and "Edit file" are from real grids; the others are to be confirmed in
/// the Task 21 real run. A tool not listed here is never matched (answer it in attach).
/// Each title belongs to one tool only: `Write` never gets "Edit file" (a Write card must
/// not answer an Edit dialog of the same path).
fn titles(tool: &str) -> &'static [&'static str] {
    match tool {
        "Bash" => &["Bash command"],
        "Edit" | "MultiEdit" => &["Edit file"],
        "Write" => &["Create file", "Write file"],
        "NotebookEdit" => &["Edit notebook"],
        "Read" => &["Read file"],
        "WebFetch" => &["Fetch"],
        _ => &[],
    }
}

const ALL_TITLES: [&str; 7] =
    ["Bash command", "Edit file", "Create file", "Write file", "Edit notebook", "Read file", "Fetch"];

/// Glyphs that start conversation lines (Claude's messages, tool results, the turn summary)
/// or a prompt / option: never a line of a dialog's tool box.
const CHROME: [char; 5] = ['⏺', '⎿', '✻', '❯', '›'];

/// Leading spaces.
fn indent(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
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

/// One screen line against `card` from `pos`: every character must be the card's next one,
/// and a run of spaces must be a run of whitespace in the card (the same length when the
/// card's run is only spaces; never a newline). Where the card is then, or `None`.
fn walk_line(line: &[char], card: &[char], mut pos: usize) -> Option<usize> {
    let mut i = 0;
    while i < line.len() {
        if line[i] == ' ' {
            let run = line[i..].iter().take_while(|c| **c == ' ').count();
            let crun = card[pos..].iter().take_while(|c| c.is_whitespace() && **c != '\n').count();
            if crun == 0 || (card[pos..pos + crun].iter().all(|c| *c == ' ') && crun != run) {
                return None;
            }
            i += run;
            pos += crun;
        } else {
            if card.get(pos) != Some(&line[i]) {
                return None;
            }
            i += 1;
            pos += 1;
        }
    }
    Some(pos)
}

/// The screen lines show exactly `card`, character by character (see [`walk_line`]). At a
/// screen line break the card may have a newline (consumed: a newline in the card must
/// fall on a line break), one whitespace character the wrap took (consumed or not), or
/// nothing (a wrap inside a word). The card must end exactly at the end of the last line.
fn walk(lines: &[Vec<char>], card: &[char]) -> bool {
    let mut starts = vec![0usize];
    for (li, line) in lines.iter().enumerate() {
        let last = li + 1 == lines.len();
        let mut next = Vec::new();
        for &s in &starts {
            let Some(p) = walk_line(line, card, s) else {
                continue;
            };
            if last {
                if p == card.len() {
                    return true;
                }
                continue;
            }
            match card.get(p) {
                None => {}
                Some('\n') => next.push(p + 1),
                Some(c) if c.is_whitespace() => next.extend([p + 1, p]),
                Some(_) => next.push(p),
            }
        }
        next.sort_unstable();
        next.dedup();
        if next.is_empty() && !last {
            return false;
        }
        starts = next;
    }
    false
}

/// Is the dialog on screen exactly the one for this request (`tool`, its `detail` — a
/// command or a path — and the hook's `description`, when it had one)? All of it must hold:
/// - a permission dialog is live (question, options, footer at the very bottom, nothing of
///   the input box between them);
/// - the box: when a `─` border is drawn above the question, the nearest one is the box's
///   top and the title is the line right below it; with no border, the nearest known title
///   above the question. The title must be one of `tool`'s titles;
/// - every line between the title and the question belongs to the box: none starts with a
///   conversation or prompt glyph (⏺ ⎿ ✻ ❯ ›), none is a border or another question, and
///   all are indented deeper than the title (except one "Tip:" line right below the title,
///   at its indentation, as Claude Code 2.1.285 draws it);
/// - those lines are, character by character (see [`walk`]), the whole detail, then exactly
///   the description when the request has one (Claude shows it below the command), else
///   nothing: a shared prefix, a longer or multi-line command, different spacing or a file
///   with the same name in another directory is another request (a description is required
///   when known, so a wrapped command is never read as a command plus a description);
/// - an empty detail never matches anything.
pub fn dialog_matches(
    grid: &str,
    tool: &str,
    detail: &str,
    description: Option<&str>,
    worktree: Option<&Path>,
) -> bool {
    let needle: Vec<char> = shown_detail(detail.trim(), worktree).chars().collect();
    if needle.is_empty() || titles(tool).is_empty() {
        return false;
    }
    let all = non_blank(grid);
    let offset = all.len().saturating_sub(BOTTOM_ROWS);
    let Some(question) = dialog_start(&all[offset..]).map(|i| i + offset) else {
        return false;
    };
    let from = question.saturating_sub(BOX_ROWS);
    let title = match (from..question).rev().find(|&i| is_border(all[i])) {
        Some(border) => border + 1,
        None => match (from..question).rev().find(|&i| ALL_TITLES.contains(&all[i].trim())) {
            Some(t) => t,
            None => return false,
        },
    };
    if title >= question || !titles(tool).contains(&all[title].trim()) {
        return false;
    }
    let ti = indent(all[title]);
    let mut body = &all[title + 1..question];
    if body.first().is_some_and(|l| indent(l) == ti && l.trim_start().starts_with("Tip:")) {
        body = &body[1..];
    }
    let foreign = |l: &&str| is_question(l) || is_border(l) || l.trim_start().starts_with(CHROME) || indent(l) <= ti;
    if body.is_empty() || body.iter().any(foreign) {
        return false;
    }
    let base = body.iter().map(|l| indent(l)).min().unwrap_or(0);
    if indent(body[0]) != base {
        return false;
    }
    let lines: Vec<Vec<char>> = body.iter().map(|l| l[base..].trim_end().chars().collect()).collect();
    let desc: Option<Vec<char>> = description.map(str::trim).filter(|d| !d.is_empty()).map(|d| d.chars().collect());
    (1..=lines.len()).any(|j| {
        walk(&lines[..j], &needle)
            && match &desc {
                Some(d) => j < lines.len() && walk(&lines[j..], d),
                None => j == lines.len(),
            }
    })
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
    const DIALOG_DESC: &str = "Create empty file s3-allowed.txt";
    /// The same dialog as drawn in full (S3 run 1, `docs/spikes/raw/s3-status-check.md`): a
    /// `─` border above the title and a tip line right below it.
    const REAL_BOXED: &str = "\
❯ Use o Bash para rodar exatamente touch s3-allowed.txt e diga se deu certo.
⏺ Creating empty file s3-allowed.txt
  ⎿  $ touch s3-allowed.txt
────────────────────────────────────────────────────────────────────────────────
 Bash command
 Tip: auto mode handles these prompts for you — choose \"switch to auto mode\" below
   touch s3-allowed.txt
   Create empty file s3-allowed.txt
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /Users/u/.mirante-spike/s3 from this project
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
        assert!(dialog_matches(DIALOG, "Bash", "touch s3-allowed.txt", Some(DIALOG_DESC), None));
        assert!(!dialog_matches(DIALOG, "Bash", "rm -rf /", Some(DIALOG_DESC), None));
        assert!(dialog_matches(
            EDIT_DIALOG,
            "Edit",
            "/Users/u/.mirante/worktrees/app/feat/src/main.rs",
            None,
            Some(Path::new(WT_DIR))
        ));
        assert!(!dialog_matches(IDLE, "Bash", "touch s3-denied.txt", None, None));
        let wrapped = "Bash command\n  cargo test --workspace --all-\n  features -- --nocapture\nDo you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";
        assert!(dialog_matches(wrapped, "Bash", "cargo test --workspace --all-features -- --nocapture", None, None));
    }

    #[test]
    fn a_command_sharing_a_long_prefix_is_another_request() {
        let shown = "Bash command\n  cargo test --workspace --all-features -- --nocapture && rm -rf ~\nRun tests\nDo you want to proceed?\n❯ 1. Yes\n  2. No\nEsc to cancel";
        // The first 30+ characters are the same: still not this request.
        assert!(!dialog_matches(
            shown,
            "Bash",
            "cargo test --workspace --all-features -- --nocapture",
            Some("Run tests"),
            None
        ));
        assert!(
            !dialog_matches(DIALOG, "Bash", "touch s3", Some(DIALOG_DESC), None),
            "a prefix of the command is not the command"
        );
    }

    #[test]
    fn the_same_file_name_in_another_directory_is_another_request() {
        assert!(!dialog_matches(
            EDIT_DIALOG,
            "Edit",
            "/Users/u/.mirante/worktrees/app/feat/tests/main.rs",
            None,
            Some(Path::new(WT_DIR))
        ));
        assert!(!dialog_matches(
            EDIT_DIALOG,
            "Edit",
            "/Users/u/.mirante/worktrees/app/feat/main.rs",
            None,
            Some(Path::new(WT_DIR))
        ));
        assert!(!dialog_matches(EDIT_DIALOG, "Edit", "/elsewhere/src/main.rs", None, Some(Path::new(WT_DIR))));
        // Relative paths are compared as they are.
        assert!(dialog_matches(EDIT_DIALOG, "Edit", "src/main.rs", None, Some(Path::new(WT_DIR))));
    }

    #[test]
    fn an_empty_detail_never_matches() {
        assert!(!dialog_matches(DIALOG, "Bash", "", None, None));
        assert!(!dialog_matches(DIALOG, "Bash", " \n ", None, None));
    }

    #[test]
    fn the_dialog_title_must_be_the_requests_tool() {
        assert!(!dialog_matches(DIALOG, "Edit", "touch s3-allowed.txt", Some(DIALOG_DESC), None));
        assert!(!dialog_matches(EDIT_DIALOG, "Bash", "src/main.rs", None, Some(Path::new(WT_DIR))));
        assert!(
            !dialog_matches(DIALOG, "mcp__srv__run", "touch s3-allowed.txt", None, None),
            "unknown tools never match"
        );
    }

    #[test]
    fn a_stale_card_never_matches_the_dialog_shown_now() {
        // Card A ("touch a") is still pending in the hub while Claude shows dialog B.
        let b = "⏺ Bash(touch a)\n  ⎿  done\n Bash command\n   touch b\n   Create b\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel";
        assert!(!dialog_matches(b, "Bash", "touch a", None, None));
        assert!(!dialog_matches(b, "Bash", "touch a", Some("Create b"), None));
        assert!(dialog_matches(b, "Bash", "touch b", Some("Create b"), None));
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
    fn the_real_boxed_dialog_matches_with_its_border_tip_and_description() {
        assert!(dialog_matches(REAL_BOXED, "Bash", "touch s3-allowed.txt", Some(DIALOG_DESC), None));
        assert!(!dialog_matches(REAL_BOXED, "Bash", "touch s3-allowed.txt", None, None));
        assert_eq!(option_key(REAL_BOXED, "Yes"), Some('1'));
        // The title must be right below the nearest border: a title further up is not the box.
        let moved = REAL_BOXED.replace(" Bash command\n", "").replacen("────", " Bash command\n────", 1);
        assert!(!dialog_matches(&moved, "Bash", "touch s3-allowed.txt", Some(DIALOG_DESC), None));
    }

    fn boxed(body: &str) -> String {
        format!(" Bash command\n{body}\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel")
    }

    #[test]
    fn spacing_newlines_and_wraps_are_matched_character_by_character() {
        // Same spaces, or a run of whitespace for a run of spaces.
        assert!(dialog_matches(&boxed("   echo a  b"), "Bash", "echo a  b", None, None));
        assert!(!dialog_matches(&boxed("   echo a  b"), "Bash", "echo a b", None, None));
        assert!(dialog_matches(&boxed("   echo a    b"), "Bash", "echo a\tb", None, None));
        // A newline in the card falls on a line break, and only there.
        assert!(dialog_matches(&boxed("   cd x\n   make"), "Bash", "cd x\nmake", None, None));
        assert!(!dialog_matches(&boxed("   cd x make"), "Bash", "cd x\nmake", None, None));
        assert!(!dialog_matches(&boxed("   cd x\n   make"), "Bash", "cd x make", Some("make"), None));
        // A wrap takes one space, or none inside a word.
        assert!(dialog_matches(&boxed("   cargo test\n   --all"), "Bash", "cargo test --all", None, None));
        assert!(dialog_matches(&boxed("   cargo te\n   st"), "Bash", "cargo test", None, None));
        // The description may follow the command, never anything else.
        assert!(dialog_matches(&boxed("   ls\n   List files"), "Bash", "ls", Some("List files"), None));
        assert!(!dialog_matches(&boxed("   ls\n   List files"), "Bash", "ls", Some("List"), None));
        assert!(!dialog_matches(&boxed("   ls\n   List files"), "Bash", "ls", None, None));
        assert!(!dialog_matches(&boxed("   ls"), "Bash", "ls", Some("List files"), None));
    }

    #[test]
    fn lines_outside_the_box_end_it() {
        // Indented no deeper than the title, or a conversation glyph, inside the box.
        assert!(!dialog_matches(&boxed(" touch a"), "Bash", "touch a", None, None));
        assert!(!dialog_matches(&boxed("   touch a\n⏺ x"), "Bash", "touch a", Some("x"), None));
        assert!(!dialog_matches(&boxed("   touch a\n  ⎿  x"), "Bash", "touch a", Some("⎿  x"), None));
        // A tip line is skipped only right below the title and at its indentation.
        assert!(dialog_matches(&boxed(" Tip: t\n   touch a"), "Bash", "touch a", None, None));
        assert!(!dialog_matches(&boxed("   Tip: t\n   touch a"), "Bash", "touch a", None, None));
    }

    // Ported from the driver review's probes: each one showed a hole (the assertion was the
    // opposite) and must now hold.

    #[test]
    fn probe_p1_a_multi_line_command_starting_with_the_cards_command_is_another_request() {
        let g = " Bash command\n   touch a\n   rm -rf ~\n   Make a, clean\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel";
        assert!(!dialog_matches(g, "Bash", "touch a", None, None));
        assert!(!dialog_matches(g, "Bash", "touch a", Some("Make a, clean"), None));
        assert!(dialog_matches(g, "Bash", "touch a\nrm -rf ~", Some("Make a, clean"), None));
    }

    #[test]
    fn probe_p2_different_spacing_is_another_command() {
        let g =
            " Bash command\n   rm -rf build /\n   Clean\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel";
        assert!(!dialog_matches(g, "Bash", "rm -rf build/", Some("Clean"), None));
        assert!(dialog_matches(g, "Bash", "rm -rf build /", Some("Clean"), None));
        let g2 =
            " Bash command\n   chmod -R 777 . /\n   x\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel";
        assert!(!dialog_matches(g2, "Bash", "chmod -R 777 ./", Some("x"), None));
    }

    #[test]
    fn probe_p3_a_title_quoted_in_the_conversation_is_not_the_dialogs() {
        let g = "⏺ For reference the earlier prompt was:\n Bash command\n   touch a\n⏺ Now calling the MCP tool.\n Tool use\n   srv - wipe(target: \"prod\") (MCP)\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for srv - wipe\n   3. No\n Esc to cancel";
        assert!(has_permission_prompt(g));
        assert!(!dialog_matches(g, "Bash", "touch a", None, None));
    }

    #[test]
    fn probe_p5_other_tools_paths_and_a_quoted_dialog_above_a_status_line() {
        let g = " Bash command\n   touch s3-allowed.txt\n   x\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel";
        assert!(!dialog_matches(g, "Bash", "s3-allowed.txt", Some("x"), None));
        assert!(!dialog_matches(g, "Bash", "touch s3-allowed.txt.bak", Some("x"), None));
        assert!(!dialog_matches(g, "Read", "touch s3-allowed.txt", Some("x"), None));
        let e = " Edit file\n   src/main.rs\n Do you want to make this edit to main.rs?\n ❯ 1. Yes\n   2. No\n Esc to cancel";
        assert!(!dialog_matches(e, "Edit", "/other/src/main.rs", None, Some(Path::new("/wt"))));
        assert!(
            !dialog_matches(e, "Write", "/wt/src/main.rs", None, Some(Path::new("/wt"))),
            "Write card, Edit dialog"
        );
        let q = "⏺ It said:\n Bash command\n   touch a\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel\n────\n❯ \n────\n  ⏸ manual mode on";
        assert!(!has_permission_prompt(q) && !dialog_matches(q, "Bash", "touch a", None, None));
        // The status line below the input prompt says "Esc to …": still not a live dialog.
        let q2 = "⏺ It said:\n Bash command\n   touch a\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel\n❯ \n  Esc to interrupt";
        assert!(!has_permission_prompt(q2));
        assert!(!dialog_matches(q2, "Bash", "touch a", None, None));
        assert_eq!(option_key(q2, "Yes"), None);
    }

    #[test]
    fn tail_keeps_the_last_non_blank_lines() {
        assert_eq!(tail("a\n\nb\n  \nc\n", 2), "b\nc");
    }
}
