# Plan 2b dogfood: replace cmux with `mirante`

Spec §6 metrics this run feeds (plus the memory guard of Tasks 18–19): the founder uses it instead of cmux for 2 consecutive weeks; context recreated by hand after closing and reopening = 0; wrong status = 0 in 50 hand-checked transitions; mid-run steering works on both agents or the fallback is explicit. (The "3+ external developers" metric needs an installable build and is out of scope here.)

## Setup (once)

- [ ] `cargo build --release --workspace`, then put the three binaries in one directory on PATH. The CLI finds `mirante-daemon` next to itself, and the daemon finds `mirante-hook` next to itself:
      `mkdir -p ~/.local/bin && ln -sf $PWD/target/release/{mirante,mirante-daemon,mirante-hook} ~/.local/bin/`
- [ ] In the shell profile, point at the real agents (PATH `claude` / `codex` may be cmux shims):
      `export MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude` and `export MIRANTE_CODEX_BIN=<real codex path>`.
- [ ] Open a new shell (the daemon inherits those variables when it starts), then `mirante daemon start` and `mirante daemon status` → `running`.
- [ ] Leave `MIRANTE_CLAUDE_INLINE_ANSWERS` **unset** until the first check of section H passes. With it unset (the default), `mirante allow|deny` on a Claude card types nothing: it prints "answer this dialog in the terminal (mirante attach <task>)" and offers to attach; Codex cards are answered as usual. Screen text can never prove which request a Claude dialog is, so typing the answer needs transcript evidence that is only trusted once H confirms it on the real Claude.

## Day 0 acceptance (one sitting, about 45 minutes)

Keep `mirante watch` open in one terminal for the whole run.

**A. Dispatch**
- [ ] `mirante new <repo> --agent claude --branch <yours> "<goal>"`: the trust question is shown and you answer it; `ready`; the board shows `working`, then `idle`.
- [ ] Same with `--agent codex`: no trust question; `ready`; `working` → `idle`.
- [ ] Both worktrees are under `~/.mirante/worktrees/<repo>/<branch>`, on the branch you named, based on the repo's current branch.

**B. Status: 50 hand-checked transitions (target: 0 wrong)**

For each step, write down what you see in `mirante attach` and what the board says. Run the Claude steps twice and the Codex steps once:
- a plain reply;
- a command that needs permission, allowed with `mirante allow` (Claude: only once `MIRANTE_CLAUDE_INLINE_ANSWERS=1` is on after section H; before that, `mirante allow` offers `mirante attach` and you answer there);
- another one, denied with `mirante deny` (same rule);
- one answered inside `mirante attach` (`mirante perms` must empty itself);
- a multi-tool turn;
- 60 s idle (Claude's idle notification must stay `idle`);
- a slash command (`/context`) and `/compact`;
- a Codex command still running when its turn completes: the board shows `working [turn done, 1 command(s) still finishing]`, then `idle`;
- `mirante stop`.

Export the trail for the log:
`sqlite3 ~/.mirante/mirante.sqlite "select seq, substr(task_id,-8), json_extract(payload,'$.from'), json_extract(payload,'$.to'), json_extract(payload,'$.cause') from events where kind='status' order by seq"`

| # | agent | step | seen in attach | board | ok? |
|---|---|---|---|---|---|
| 1 | | | | | |

**C. Talking mid-run**
- [ ] Claude: start a long multi-step task, then `mirante say <task> "stop, say BANANA"` → `sent now`. The answer is BANANA in the same turn, and the trail has `message_delivery` with `"delivery":"steered"`.
- [ ] Claude: `mirante say --queue <task> "<next>"` during a turn → `queued (#1 …)`; it is delivered right after the turn ends.
- [ ] `mirante say` while a permission dialog is open → refused with "waits on a dialog" (nothing is typed into the dialog).
- [ ] Codex: the same steer (BANANA) and queue checks; `mirante interrupt <task>` stops a running command.

**D. Permissions**
- [ ] Every dialog appears in `mirante perms` and in the board's NEEDS column.
- [ ] Claude, inline answers off (default): `mirante allow <perm>` exits non-zero with "answer this dialog in the terminal (mirante attach <task>) … nothing was typed" and offers to attach; answering `y` attaches, you answer in the TUI, and the card leaves `mirante perms`. Nothing was typed into the dialog by Mirante (watch it in a second `mirante attach`).
- [ ] Claude, inline answers on (`MIRANTE_CLAUDE_INLINE_ANSWERS=1`, only after section H's first check passed; `mirante daemon restart` from a shell that exports it): `mirante allow` picks the plain "Yes" (watch it in attach: never "always allow", never "auto mode").
- [ ] A Codex approval (ask for something outside the worktree, or with network) → card → `mirante deny` → Codex continues without it.

**E. Context survives**
- [ ] Mid-conversation, `mirante daemon stop`; `mirante ls` (restarts the daemon) shows the task `ended`; `mirante resume <task>` prints `ready (resumed)`; ask about something from before → remembered. Do this for both agents.
- [ ] `kill -9` the daemon (`pgrep mirante-daemon`) during a turn, then `mirante ls` and `mirante resume`: remembered.
- [ ] Reboot the machine once during the two weeks; resume everything; nothing recreated by hand.

**F. Cleanup**
- [ ] `mirante archive` on a task with uncommitted changes is refused; after committing, it succeeds and the branch still exists.
- [ ] `kill -9` the daemon while `mirante archive` runs; after the restart the task is `archived` (clean worktree) or back to `active` (dirty worktree, `archive_abandoned`), never stuck in `archiving`.
- [ ] Make a checkout under `~/.mirante/worktrees/` that no task owns (`git -C <repo> worktree add ~/.mirante/worktrees/<repo>/stray`): the next daemon start reports it (`worktree_orphan`) and `mirante doctor` lists it; nothing deletes it.
- [ ] Queue a message (`mirante say --queue`) during a turn, then `kill -9` the daemon: after the restart the trail has `queue_dropped` with that text.

**G. Memory**
- [ ] `mirante ls` shows a `MEM` column for live tasks (e.g. `1.1G/4.0G`); `mirante watch` shows the `memory:` line (level, free, swap). On a normal day the level stays `normal` even with swap well used (the kernel's own pressure level decides on macOS).
- [ ] `mirante hibernate <idle task>` → `hibernated`; `ls` says `hibernated: mirante wake`; `mirante wake <task>` → `ready (resumed)` and the agent remembers the conversation. `mirante hibernate` on a task with a pending permission is refused.
- [ ] `mirante doctor` → `orphan processes: none` after a normal day; start a dev server from an agent, `kill -9` the daemon, restart it: `doctor` lists the server (ppid 1, cwd in the worktree); `mirante doctor --kill <pid>` asks first, then ends it.
- [ ] Optional, apps closed first: the stress procedure in `docs/dogfood/memory-stress.md` (Task 18).

**H. Real-run checks the driver review left open (first live confirmation; fix the code if one fails)**

The first check gates Claude inline answers. Until it passes, keep `MIRANTE_CLAUDE_INLINE_ANSWERS` unset (Mirante never types a Claude answer; `mirante allow|deny` says "answer this dialog in the terminal" and offers `mirante attach`).

- [ ] **Answer evidence on Claude 2.1.285 (gates `MIRANTE_CLAUDE_INLINE_ANSWERS=1`).** With inline answers still off, ask Claude for a command that needs permission and, while the dialog is open: (1) in the session transcript (`~/.claude/projects/<dir>/<session>.jsonl`, the `transcript_path` of the hooks), the assistant line with that command's `tool_use` block (its `id`) is **already written** before the dialog renders — check with `tail -f` on the transcript next to `mirante attach`: the line must be there while the dialog is on screen; (2) the `PermissionRequest` hook payload carries that same `tool_use_id`, or the `PreToolUse` right before it does (record which: `sqlite3 ~/.mirante/mirante.sqlite "select payload from events where kind='hook' order by seq desc limit 5"` or the daemon log); (3) a side agent (Task tool) asking for a command: record whether its tool_use lines land in the main transcript with `isSidechain`/`agentId` (the hub only answers a side-agent card when they do and the `agentId` equals the hook's `agent_id`) or in a separate file (then side-agent cards always go to the terminal: safe). Only after (1) and (2) hold, the dogfood may enable `MIRANTE_CLAUDE_INLINE_ANSWERS=1` (export it, `mirante daemon restart`, `mirante daemon status` → the daemon log says `claude inline answers on`). If (1) fails (the line is written only after the answer), inline answers stay off for good: every Claude card is refused with "the transcript does not show tool call … (yet)".
- [ ] **Evidence refusals.** With inline answers on: (a) `kill -STOP` the daemon for 2 s while a hook fires (or `mirante daemon stop` then start while a dialog is open), then `mirante allow` on a card created before: refused with "a hook event missed the daemon since this card was created" and nothing typed; (b) a card whose tool call already finished in the terminal: refused ("already has a result" / settled), nothing typed.
- [ ] **Ctrl-U sequence.** In `mirante attach`, paste a 3-line text into Claude's input box without pressing Enter, then type Ctrl-E and six Ctrl-U (what `claude::clear_input(3)` sends since Task 21 before re-pasting a message whose delivery was stopped after its paste; four were not enough on 2.1.285). The box must be empty, and a large paste shown as `[Pasted text …]` must go too. If not, change `clear_input` to the sequence that works and keep the "never Enter, Esc or a digit" rule.
- [ ] **Requeued message not duplicated.** Steer a message while a permission dialog is about to open (e.g. right after asking for a command that needs permission); answer the dialog; the message arrives once (trail: `message_requeued`, then one `message_sent`), not twice.
- [ ] **Trust answer.** On a new repo, `mirante resume`/`new` shows the trust prompt; answering it moves the cursor to "Yes, I trust this folder" (Down) and confirms with Enter only once the screen shows the cursor there (the 2.1.285 dialog has no digits; Task 21). Claude trusts per repository: later worktrees of a trusted repo skip the dialog.
- [ ] **Bracketed paste.** A multi-line `mirante say` arrives as one message (not one per line); `mirante say <task> "/compact keep the plan"` runs `/compact` with "keep the plan" (only `/compact ` is typed, the arguments are pasted); messages starting with `!`, `#` or `&` are refused.
- [ ] **Dialog titles.** Every permission dialog Mirante answers with `mirante allow`/`deny` shows the tool-box title `screen::titles` expects ("Bash command", "Edit file"; confirm "Edit notebook", "Read file", "Fetch"). A dialog whose title is not listed is refused with "answer it in `mirante attach`" (safe, but add the title).
- [ ] **Write dialog title.** Ask Claude to create a new file and to overwrite an existing one with the Write tool; record the box title of each dialog. `screen::titles("Write")` accepts only "Create file" and "Overwrite file" (confirmed live in Task 21; never "Edit file", which belongs to Edit); a title used by two tools must not be listed for both.
- [ ] **Box border and box lines.** In a live permission dialog, confirm the `─` row drawn right above the tool-box title (the border `screen::dialog_matches` anchors the title to: it must start at column 0 and span at least 80% of the terminal's width, or the dialog is never matched — resize the terminal narrow and wide while attached and check both), the "Tip: …" line at the title's indentation right below it, command and description lines indented deeper than the title, and a long command wrapped by Claude with its continuation at the same indentation. Also check a Bash dialog shows the hook's `tool_input.description` right below the command (Mirante requires it when the hook had one), and an Edit dialog shows only the path (a diff in the box would make Edit cards unanswerable from Mirante: safe, but adjust the box rule).
- [ ] **Status line "Esc to …" capitalisation.** While a turn runs, record the exact status-line text below the input box (e.g. "esc to interrupt" vs "Esc to interrupt"). A permission dialog's footer is found by "Esc to"; `screen::dialog_start` already refuses a question with the input prompt or a border between it and the footer, but if the status line uses the same capitalisation, check that a dialog quoted in the conversation is not reported as live (`mirante perms` shows no card, `mirante say` is not refused for a dialog).
- [ ] **Bracketed paste of a bare digit.** With a permission dialog open in `mirante attach`, paste (do not type) a single `2` as a bracketed paste (`printf '\e[200~2\e[201~'` from a terminal that sends it raw). It must not select option 2. Mirante never types while a dialog is open or a card is pending, and slash command names with a digit are refused, but slash command arguments (`/model 2`) arrive as such a paste, so confirm it.
- [ ] **Lone-card rule.** With two dialogs pending in one session (main agent plus a side agent), `mirante allow` on either is refused with "answer this dialog in the terminal" / "answer them in `mirante attach`" and the list of pending cards; a second dialog from the same agent marks its older card `permission_superseded`.
- [ ] **Interrupt during a dialog.** With a permission dialog open, `mirante interrupt <task>` is refused ("dialog open — answer or deny it first") and the dialog stays open. Also: send a long `mirante say` and, while it is being typed, ask for a command that opens a dialog and run `mirante interrupt` at once: the interrupt waits for the typist and is refused, no Esc reaches the dialog.
- [ ] **NativeGone text.** Delete a Codex thread's rollout file (under `~/.codex/sessions/`), then `mirante resume <task>`: the error says the conversation is gone. Record the exact JSON-RPC code and message app-server sent (daemon stderr / `resume_failed` event) and adjust `codex::is_native_gone` if it is neither "no rollout found" nor -32603 with "not found"/"rollout".
- [ ] **Session grant.** Ask Codex to write outside the worktree so it sends a `grantRoot` request: the card says "for the rest of the session"; `mirante allow` prints `[allowed: write access under <root> for the rest of the session]` in attach, `mirante perms` history shows `allowed_session`, the trail has `session_grant`, and `mirante ls` MODE shows `auto-edits(net:off, +1 granted root)`.
- [ ] **Effective sandbox.** `mirante ls` MODE for a Codex task reads `auto-edits(net:off)` (or what your Codex config really grants); `mirante attach` starts with the `effective sandbox:` line.

## Two-week log

| date | tasks dispatched | opened cmux? (why) | wrong statuses | context recreated by hand | steering fallback needed |
|---|---|---|---|---|---|
| | | | | | |

## Verdict

- [ ] 14 consecutive days without cmux
- [ ] 0 context recreated by hand
- [ ] 0 wrong statuses in the 50 checked
- [ ] steering worked on both agents (or the fallback used is written down above)
