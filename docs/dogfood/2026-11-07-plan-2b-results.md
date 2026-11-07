# Plan 2b, Task 21: verification and real-agent results

Date: 2026-11-07. Run by the implementer agent against the checklist in `2026-11-07-plan-2b-dogfood.md` (Day 0 sections A–H). The founder still owns the two-week log and the items marked NOT-RUN below.

## Environment

- macOS 26.5.2 (arm64), Rust 1.99.1, Claude Code 2.1.285 (`/opt/homebrew/bin/claude`, "Update available" banner shown), Codex CLI 0.167.0 (`~/.nvm/versions/node/v22.14.0/bin/codex`, ChatGPT login).
- Debug binaries from `target/debug` on PATH; base commit `660e256`.
- `MIRANTE_HOME=~/.mirante-spike/m21home`. The scratchpad path was too long for the socket (156 bytes, limit 104). A symlink to it was refused (`mirante: … is a symlink`, the home check working as intended).
- Sandbox repos made with `scripts/make-sandbox.sh`: `~/.mirante-spike/m21-claude`, `m21-claude2` and `m21-codex` (branch `master`). No real project repo was touched.
- `MIRANTE_CLAUDE_INLINE_ANSWERS` was unset for the whole run (daemon log: `claude inline answers off`).
- Memory: the kernel pressure level was 2 (`kern.memorystatus_vm_pressure_level`), and `mirante watch` showed `memory: elevated · free 651M of 18.0G · swap 9.6G/11.0G · new sessions and queued messages paused`. `mirante wake` was refused with "memory is tight". From the fourth daemon start on, the daemon ran with `MIRANTE_MEMGUARD=off`, so `doctor` then said `memory: not sampled yet`.
- Observation-only wrappers, used for some daemon starts:
  - `MIRANTE_HOOK_BIN` → a script that logged each hook payload with a timestamp, then exec'd the real `mirante-hook`;
  - `MIRANTE_CODEX_BIN` → a script that tee'd Codex's stdout (JSON-RPC) to a log.
- Screens were read through a pyte-rendered PTY running `mirante attach`. Answers given in the terminal were digits typed into that attach: `1` (Yes) for `touch` only, and the "No" digit for Write/Edit.
- Every daemon was stopped at the end. `pgrep` found no `mirante-daemon`, `codex app-server`, attach clients or sandbox processes left.

## Automated verification (Steps 1–3)

| Step | Result |
|---|---|
| `cargo build --workspace && cargo test --workspace` | **PASS**: 558 passed, 2 ignored (cli 15, e2e 8, core 159, daemon 270, daemon bin 1, daemon_signals 2, hook 16, hook binary 2, ipc 24, spike-kit 61). The plan's "464" predates the later robustness rounds. |
| `cargo clippy -p mirante-ipc -p mirante-hook -p mirante-daemon -p mirante-cli --all-targets` (plus `-p mirante-core`) | **PASS**: no output. |
| `git diff --stat 2b8030d -- crates/spike-kit crates/mirante-core` | **Not empty, as expected**: `c70bc1f` (migration 0004 `permission_allowed_session`, model/store) and this run's status fix `0078362`. |
| Real-agent test, Claude (`MIRANTE_TEST_TRUST=1`) | **FAIL at first, then PASS** (`claude_turn_permission_stop_and_resume ... ok`, 12.9 s). See bugs 1, 5, 6 and the test fixes. |
| Real-agent test, Codex | **FAIL at first, then PASS** (`codex_steer_and_resume ... ok`, 13.5 s). The test's event order was wrong; see the test fixes. |

## Day 0 checklist

PASS = seen working live. FAIL = seen broken live (fixed here or open, as noted). NOT-RUN = not done, with the reason.

**Setup**
| Item | Result |
|---|---|
| Release build + `~/.local/bin` symlinks | NOT-RUN: debug binaries on PATH, sandbox home (the run must not touch `~/.mirante`) |
| `MIRANTE_*_BIN` | PASS |
| `daemon start` / `status` | PASS: `running: pid … live sessions 0 (v0.1.0)` |
| Inline answers unset → `allow` says answer in the terminal | PASS (D) |

**A. Dispatch**
| Item | Result |
|---|---|
| Claude `new`: trust shown and answered, ready, working → idle | **FAIL → fixed → PASS**. Before the fix, the trust dialog was never recognised, so `mirante new` said "still starting" forever (bug 1). After it, on a new repo: `Claude asks whether to trust the worktree folder:` [screen] `Trust this folder and continue? [y/N]` → y → `41c82fc2 ready.`; trail `boot trust_prompt → trust_answered → boot ready → working → idle`. |
| Codex `new`: no trust, ready, working → idle | PASS: `ecfd5fe6 ready.`, `codex:turn/start` → `codex:turn/completed`, answer "OK". |
| Worktrees under `<home>/worktrees/<repo>/<branch>`, on the branch, based on the current branch | PASS: `…/m21home/worktrees/m21-claude/c1`, branch `c1` from `master`. |

**B. Status (hand-checked transitions)**

122 status transitions across 4 tasks (`c1`, `c2`, `c3` Claude; `x1` Codex), each compared with the attach screen and the trail.

- **1 wrong**: Esc (`mirante interrupt`) during a streamed text answer left `working` until the next turn's Stop (seq 38→42). This is bug 2. It was fixed and re-checked twice live (`working → idle transcript:interrupted`, seq 244 and 251).
- **1 gap**: `/compact keep the plan` ran for about 20 s with the board `idle` and no transition. Compaction writes no prompt line or assistant line, so no turn opens (issue I1).

| Step | Result |
|---|---|
| Plain reply | PASS |
| Permission answered in attach (Claude, `touch`) | PASS: `needs_you → working hook:PostToolUse`, `permission_settled allowed_once` |
| Deny in attach (Write, Overwrite, Edit) | PASS: `needs_you → idle transcript:turn_duration`, `permission_settled denied` |
| `mirante perms` empties itself after an attach answer | PASS |
| Multi-tool turn (6 sequential Reads) | PASS |
| Idle for minutes (the idle Notification) | PASS: no spurious move away from `idle` |
| `/context` | PASS (local command, no turn); `/compact`: gap above |
| Codex command still running at `turn/completed` | PASS: `turn_ended` (seq 276), then the `passo-2` command completed (277), then `working → idle codex:item/completed` (278). The `[turn done …]` board detail was not captured visually. |
| `mirante stop` | PASS: `ended` |
| A side-agent dialog after the main turn's Stop | PASS: `idle → needs_you` (PermissionRequest), `→ idle` (PostToolUse) |

**C. Talking mid-run**
| Item | Result |
|---|---|
| Claude steer BANANA | PASS: stopped at step 3 of 6 ("BANANA / Parei no passo 3 de 6"), trail `message_delivery {"delivery":"steered"}` |
| Claude `say --queue` | PASS: `queued (#1 in line, delivered when the turn ends)`, then delivered after the turn; answer `QUEUED-1` |
| `say` while a dialog is open | PASS: `task c1 waits on a dialog: answer it first (mirante perms / mirante attach) (code 5)` |
| Codex steer BANANA and queue | PASS: steered after `passo-1`, "BANANA" in the same turn; `QUEUED-X` delivered after it |
| Codex `interrupt` stops a running command | **PARTIAL**: the turn is interrupted (`── turn interrupted ──`, `[command abandoned]`, status idle), but `sleep 30` kept running under `codex app-server` until it finished (issue I3) |

**D. Permissions**
| Item | Result |
|---|---|
| Each dialog in `mirante perms` and NEEDS | PASS (Bash, Write, Edit, side-agent Bash, Codex command, Codex file change) |
| Claude, inline off: `allow`/`deny` refused, nothing typed, answered in attach | PASS: `mirante: answer this dialog in the terminal (mirante attach c1): Mirante does not type Claude permission answers (inline answers are off; MIRANTE_CLAUDE_INLINE_ANSWERS=1 on the daemon turns them on); nothing was typed` / `mirante: not answered: answer it in \`mirante attach c1\``, exit 1. The dialog stayed open and unchanged. The `y` → attach path needs a TTY: NOT-RUN (founder). |
| Claude, inline on | NOT-RUN (run rule: inline answers stay off) |
| Codex approval → `deny` → Codex continues | PASS: `787ae09a command: denied`, the stream shows `[denied]`, and Codex said "a solicitação de permissão elevada foi rejeitada"; the same for a file change (`patch rejected by user`). |

**E. Context survives**
| Item | Result |
|---|---|
| `stop` + `resume`, Claude | PASS: `ready (resumed)`, then "BANANA" recalled |
| `daemon stop` → `ls` (restarts the daemon) → `ended` → `resume`, both agents | PASS: Codex quoted the first message ("Responda apenas OK."); Claude recalled it |
| `kill -9` the daemon during a turn, then `ls` and `resume` | PASS: the Claude process died with the daemon (`pgrep -P` empty), `recovered … sessions_ended: 1, spooled_hooks: 1, queued_dropped: 1`, and the resumed session answered `GOIABA` (said in the killed turn) |
| Reboot | NOT-RUN (founder) |

**F. Cleanup**
| Item | Result |
|---|---|
| `archive` with uncommitted changes refused | PASS. The message is git's own: `git worktree remove … contains modified or untracked files, use --force to delete it (code 2)` (issue I7). A live session is refused first: `task c3 has a live session: stop it first`. |
| After committing: archived, branch kept | PASS: `41c82fc2 archived (branch c3 kept)`; `git branch --list c3` → `c3` |
| `kill -9` during `archive` | NOT-RUN: the window is too short to hit by script (the CLI restarted the daemon and the archive finished: `archiving → archived`) |
| Stray checkout → `worktree_orphan` + `doctor` | PASS: `orphan_worktrees: [".../worktrees/m21-claude/stray"]`; `doctor`: `orphan checkouts (no task owns them; Mirante never deletes them):` |
| Queued message + `kill -9` → `queue_dropped` | PASS: `queue_dropped {"cause":"mirante:daemon_restart","messages":["QUEUED-KILL: diga apenas Q."]}` |

**G. Memory**
| Item | Result |
|---|---|
| `ls` MEM column, `watch` memory line | PASS: `98M/4.0G` … `523M/4.0G`; `memory: elevated · free 651M of 18.0G · swap 9.6G/11.0G · new sessions and queued messages paused`. The machine really was at kernel level 2, so "normal on a normal day" could not be checked. |
| `hibernate` / `wake`, both agents | PASS: `hibernated (mirante wake c3 resumes it)`, `ls` shows `[hibernated: mirante wake]`; after `wake` Claude recalled the last number before the interrupt and Codex its first message. `hibernate` with a pending card is refused: `a session waiting on you is never hibernated`. Issue I2: the hibernated mark does not survive a daemon restart. |
| `doctor` orphan processes | PASS: `orphan processes: none`. `--kill`: NOT-RUN. Codex reaps the background job of its command (`nohup sleep 290 &` was gone when the command returned), and the run rules forbid allowing `sleep` on Claude. |
| Stress procedure | NOT-RUN |

**H. Real-run checks** (facts in the next section)
| Item | Result |
|---|---|
| Answer evidence (gates inline answers) | Facts recorded; verdict below |
| Evidence refusals (inline on) | NOT-RUN (inline answers stay off in this run) |
| Ctrl-U sequence | **FAIL → fixed** (bug 3) |
| Requeued message not duplicated | NOT-RUN: a steer fired on the PreToolUse hook already found the dialog drawn (`waits on a dialog`), so the requeue window could not be hit by script |
| Trust answer | **FAIL → fixed** (bugs 1, 5) |
| Bracketed paste | PASS: a 3-line `say` arrived as one prompt; `/compact keep the plan` ran with `<command-args>keep the plan</command-args>`; `!ls` / `#nota` / `&bg` refused with their reasons |
| Dialog titles | PASS for "Bash command" and "Edit file"; "Read file", "Fetch" and "Edit notebook" not seen |
| Write dialog title | **FAIL → fixed**: the titles are "Create file" and "Overwrite file" (bug 4) |
| Box border and box lines | PASS, with limits (issue I5) |
| Status line "Esc to …" | PASS: there is no "esc to interrupt" text at all (below) |
| Bare digit paste | Done at the input prompt (the run rule forbids pasting into a dialog): `❯ 2` sits in the box, not submitted. The dialog variant is NOT-RUN. |
| Lone-card rule | NOT-RUN (only meaningful with inline answers on) |
| Interrupt during a dialog | PASS: `dialog open — answer or deny it first (mirante perms / mirante attach) (code 2)`, and the dialog stayed open. The typist-race part is NOT-RUN. |
| NativeGone text | PASS: recognised (below) |
| Session grant | NOT-RUN: Codex's file-change request outside the worktree came with `"grantRoot": null` |
| Effective sandbox | PASS: MODE `auto-edits(net:off)`; attach starts with `[mirante] effective sandbox: workspace-write, network off, writable: the worktree only` |

Counts for the 56 items above (the 55 table rows plus B's one wrong transition):

| | Items |
|---|---|
| PASS | 41 (H "answer evidence" counted as PASS with the nuance in the verdict) |
| FAIL, fixed in this run and re-verified live | 5 (A Claude dispatch, B interrupted turn, H trust, H Ctrl-U, H Write title) |
| PARTIAL | 1 (Codex interrupt) |
| NOT-RUN | 9 |

Sub-steps not run inside counted rows: the `y` → attach path, `doctor --kill`, the typist race, and the bare-digit paste into a dialog.

## Section H facts (Claude Code 2.1.285 / Codex 0.167.0, ANSI stripped)

### Trust dialog

It has no digits, and the cursor starts on "No, exit". A typed `2` did nothing. Down, then Enter, accepts it.
```
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 Accessing workspace:

 /Users/…/.mirante-spike/m21home/worktrees/m21-claude/c1

 Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source
 project, or work from your team). If not, take a moment to review what's in this folder first.

 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm · Esc to cancel
```
- After Down, the cursor is on `❯ Yes, I trust this folder`.
- `SessionStart` fires only after the answer.
- Trust is kept per repository: a second worktree of a trusted repo (`c2`) booted without the dialog.
- In the real-agent test the first Down sent right after the dialog appeared was lost, so Down is retried (bug 5).

### Bash permission dialog

Main agent, 120 columns:
```
⏺ Cria h1
  ⎿  $ touch h1.txt
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 Bash command
 Tip: auto mode handles these prompts for you — choose "switch to auto mode" below

   touch h1.txt
   cria h1

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /Users/…/m21home/worktrees/m21-claude/c1
      from this project
   3. Yes, and switch to auto mode · auto mode handles these prompts for you
   4. No

 Esc to cancel · Tab to amend
```
- **Border.** A `─` row at column 0 that spans the full width: 70/70, 120/120 and 200/200 columns measured while resizing the attach.
- **Indentation.** The title and the "Tip:" line are at indent 1; the command and the hook's `tool_input.description` ("cria h1") are at indent 3, the description right below the command.
- **At 70 columns** the Tip wraps onto a second line at indent 1 (`auto mode" below`), which the box rule treats as foreign, so the dialog is not matched (safe; issue I5).
- **Long command wrap:** not exercised.

### Write and Edit dialogs

The title sits at indent 1, the path is also at indent 1, and a `╌` dashed preview or diff box follows.

| Tool | Title | Question |
|---|---|---|
| Write, new file | `Create file` | `Do you want to create novo.txt?` |
| Write, existing file | `Overwrite file` | `Do you want to overwrite README.md?` |
| Edit | `Edit file` | `Do you want to make this edit to README.md?` |

The options are `1. Yes` / `2. Yes, and switch to accept edits (auto-approve file edits and common file commands) for this session (shift+tab)` / `3. No`. Because the path is at the title's indentation, the box rule never matches Write or Edit dialogs (safe: those cards always go to attach).

### Side agent

A side agent's Bash dialog has the title `Bash command · from the general-purpose agent`, which is never in `titles`, so it is never matched.
- The Agent ran in the background: the main turn ended with `✻ Waiting for 1 background agent to finish` and a Stop.
- The side agent's lines are **not** in the main transcript. They are in `~/.claude/projects/<dir>/<session>/subagents/agent-<agentId>.jsonl`, with `isSidechain: true` and `agentId`, plus a `.meta.json` file (`{"agentType":"general-purpose","toolUseId":"toolu_012C…","requestShape":"background",…}`).
- The main transcript only has the Agent `tool_use` and its result.
- So side-agent cards always go to the terminal (the safe branch of the design).
- When the background agent finished, it produced a new turn (UserPromptSubmit … Stop).

### Running status line

There is no "esc to interrupt" or "Esc to" text. While a turn runs, the line above the input box reads like `✶ Recombobulating… (4s · ↓ 124 tokens)` or `✻ Coalescing… (running PreToolUse hook · 1s)`. The footer reads `⏸ manual mode on · ← for agents` (with `· /tasks to see subagents` while a side agent exists). After a turn: `✻ Brewed for 2s · done 7:18 PM`. "Esc to" therefore appears only in dialog footers, so the status line cannot collide with dialog detection.

### Other screen facts

- After an interrupt: `⎿  Interrupted · What should Claude do instead?`.
- The input box can show a dim prompt suggestion (`❯ Use a ferramenta Bash para rodar exatamente \`touch h2.txt\``) after a turn.
- The user's own statusline, hooks, skills and MCP servers load inside Mirante's sessions (`--settings` is additive). The user's Serena MCP creates `.serena/` in every Claude worktree (issue I6).

### Bracketed paste

- A multi-line paste stays in the box as lines (`❯ linha um / linha dois / linha tres`).
- A 40-line paste collapses to `❯ [Pasted text #1 +39 lines]`.
- A bracketed `2` at the input prompt lands as `❯ 2` and is not submitted.

### Ctrl-E / Ctrl-U on a 3-line paste

| Keys sent | Input box after |
|---|---|
| Ctrl-E, then Ctrl-U #1 | `linha um` / `linha dois` / (empty) |
| Ctrl-U #2 | `linha um` / `linha dois` |
| Ctrl-U #3 | `linha um` / (empty) |
| Ctrl-U #4 | `linha um` |
| Ctrl-U #5 | empty, with the hint `Ctrl+Y to paste deleted text` |
| Ctrl-U #6 | no effect |

So n lines need 2n − 1 Ctrl-U, and the old `clear_input(3)` (Ctrl-E + 4 Ctrl-U) left `linha um` in the box. A collapsed `[Pasted text …]` goes with a single Ctrl-U.

### Transcript and hook timing (gates inline answers)

Measured four times (`touch h4..h7`), with hooks logged by the tee wrapper and screen and transcript polled every ~10 ms:

| t | Event |
|---|---|
| 0 ms | `PreToolUse` hook, **with** `tool_use_id` (e.g. `toolu_014ohaTmfHWnf6u7TaVL3Kaa`) |
| ~35 ms | dialog drawn on screen |
| ~90 ms | `PermissionRequest` hook, **without** `tool_use_id` (keys: `cwd, effort, hook_event_name, permission_mode, permission_suggestions, prompt_id, scratchpad_dir, session_id, tool_input, tool_name, transcript_path`) |
| ~130 ms | the assistant `tool_use` line (same id) is in the transcript: 94–100 ms **after** the dialog was first seen, all 4 times |

Also seen:
- In `PreToolUse` and `PostToolUse` payloads, `agent_id` was absent for the main agent.
- The Stop hook can arrive before the turn's final assistant text is in the transcript: the real-agent test read the previous answer right after `turn_ended` and now polls for the line.
- An Esc during a streamed answer writes only `{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]},"interruptedMessageId":…}`, with no Stop hook and no `turn_duration` (bug 2).

### Codex approval request shapes

Live, from app-server stdout. Command, outside the sandbox:
```json
{"method":"item/commandExecution/requestApproval","id":1,"params":{"kind":"command","threadId":"…","turnId":"…","itemId":"exec-fa21…","startedAtMs":…,"environmentId":"local","reason":"O sandbox bloqueou o comando. Você autoriza executá-lo fora do sandbox para criar m21-outside2.txt?","command":"/bin/zsh -lc 'touch /Users/…/m21-outside2.txt'","cwd":"/Users/…/worktrees/m21-codex/x1","commandActions":[{"type":"unknown","command":"touch /Users/…/m21-outside2.txt"}],"proposedExecpolicyAmendment":["touch","/Users/…/m21-outside2.txt"],"availableDecisions":["accept",{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["touch","/Users/…/m21-outside2.txt"]}},"cancel"]}}
```
- `availableDecisions` has no `"decline"`, yet Mirante's `{"decision":"decline"}` was accepted. The item completed with `"status":"declined"` and the turn went on.
- Codex logged `exec_command failed: CreateProcess { message: "Rejected(\"rejected by user\")" }` and recorded the sandbox violation (`backend=seatbelt reason=operation_not_permitted`).

File change, outside the worktree:
```json
{"method":"item/fileChange/requestApproval","id":0,"params":{"threadId":"…","turnId":"…","itemId":"exec-7a74…","startedAtMs":…,"reason":null,"grantRoot":null}}
```
- The paths come from the earlier `item/started` `fileChange` item: `"changes":[{"path":"…/m21-out/grant.txt","kind":{"type":"add"},"diff":"x\n"}]`.
- The card read `add …/grant.txt (+0 -0)` (issue I4).
- `decline` led to `patch rejected by user`.

### Codex NativeGone

Resuming a random thread id gave this JSON-RPC error:
```json
{"code":-32600,"message":"no rollout found for thread id 47c23785-cd78-4db7-9327-1f053040a4dd"}
```
`codex::is_native_gone` recognised it. The CLI printed `the agent's conversation is gone (codex thread …: no rollout found for thread id …); \`mirante resume <task> --fresh\` starts a new one (code 2)`, and the trail recorded `resume_failed`.

### Other Codex facts

- `turn/interrupt` ends the turn, but the command already running keeps running under app-server until it finishes on its own.
- A command's background job (`nohup … &`) is gone as soon as the command returns.

## Inline-answers gate verdict

**The evidence the design relies on holds, but this run does not enable inline answers; that stays the founder's call, after the "Evidence refusals" check is run by hand.**

- **(1) holds with a nuance.** The checklist asks for the `tool_use` line to be written "before the dialog renders". Strictly, that is false: the line lands about 95 ms *after* the dialog is drawn and about 40 ms after the `PermissionRequest` card is created. The failure the checklist names as permanent ("the line is written only after the answer") did not happen. The line is present for the whole time the dialog is on screen, beyond the first ~0.1 s, which is what the hub checks at answer time (the card's id is the newest tool call with no result). An `allow` within ~0.1 s of the card would be refused "(yet)", which is safe.
- **(2) holds through `PreToolUse`.** `PermissionRequest` carries no `tool_use_id`; the `PreToolUse` ~90 ms before it carries the id, and the engine already borrows it (`tool_use_id_for`).
- **(3)** Side agents write to a separate file, so their cards always go to the terminal.
- **What turning it on would cover.** Main-agent Bash dialogs at terminal widths where the Tip line does not wrap (about 78+ columns). Edit, Write, side-agent and narrow-terminal dialogs still fail safe to `mirante attach`. The gain is small; the risk stays bounded by the remaining guards (lone card, hook-loss marker, card generation, box match).

## Bugs found and fixed (TDD: failing test first, separate commits)

| # | Commit | Bug |
|---|---|---|
| 1 | `2eece70` | The trust dialog was never recognised: the real dialog has no digits. Every Claude task on a new repo stayed "starting". The unnumbered dialog is now detected (adjacent options right above the footer, cursor on one), and accepting it is Down, then Enter only on "Yes". |
| 2 | `0078362` | An interrupted streamed turn stayed `working`. The user line `[Request interrupted by user]` is now a transcript turn end (`transcript:interrupted`). |
| 3 | `1e51b9b` | `clear_input` left the first line of a multi-line paste; it now sends Ctrl-E + 2n Ctrl-U. |
| 4 | `a373f9c` | Write titles are now "Create file" / "Overwrite file" (was "Create file" / "Write file"), with a real-grid test that Write boxes are never matched. |
| 5 | `0fec505` | The first trust Down can be lost right after the dialog shows. Down is retyped after 400 ms while the cursor stays on "No", and Enter only once the last Down is ≥ 400 ms old. |
| 6 | `676bae0` | `trust` reported `resumed: true` on a fresh start, because Claude's native id is known at launch. `Live.resumed` now records an actual resume. |

Real-agent test fixes (the tests could not pass as written):

| Commit | Fix |
|---|---|
| `441e398` | Claude: expect `AnswerInTerminal`, then answer the `touch` dialog through an attachment. |
| `fb549d2` | Claude: wait until the dialog is drawn; poll for the resumed answer's transcript line. |
| `72ffcc1` | Codex: wait for `idle` before `turn_ended` (event order). |

## Open issues (not fixed; for the founder or a later plan)

| ID | Issue |
|---|---|
| I1 | `/compact` runs for ~20 s with the board `idle`: no prompt or assistant line opens a turn. Decide whether compaction counts as `working` (the PreCompact hook could open it). |
| I2 | The hibernated mark lives only in daemon memory. After a daemon restart, `ls` no longer shows `[hibernated: mirante wake]` and `wake` says "not hibernated (mirante resume …)". `resume` works. |
| I3 | Codex `turn/interrupt` does not stop the running command; Mirante shows `[command abandoned]`. Consider killing the abandoned command's process tree, or say in the CLI that it keeps running. |
| I4 | A Codex file-change card shows `(+0 -0)` for an `add`, whose `diff` is the raw file content (`"x\n"`), not a unified diff. |
| I5 | At narrow widths the Tip line wraps onto a second line at the title's indentation, and the Bash box is no longer matched (safe refusal). Only relevant with inline answers on. Writes and Edits are never matched (path at title indent, preview box). |
| I6 | User-level config leaks into worktrees. The founder's Serena MCP creates `.serena/` in every Claude worktree, which makes `archive` refuse until it is removed. Consider ignoring known tool dirs, or documenting it. The user's statusline and hooks also run in Mirante sessions (expected; `--settings` is additive). |
| I7 | An archive of a dirty worktree is refused with git's raw message. A Mirante wording ("commit or remove … first") would be clearer. |
| I8 | On `daemon restart`, two sessions were recorded as `mirante:process_exited` rather than `mirante:daemon_shutdown` (seq 311/314). The status is right; only the cause differs. |
| I9 | Codex lists no `"decline"` in `availableDecisions`, but accepts it. Re-check after Codex upgrades (`"cancel"` would abort the turn). |

## Left for the founder to try by hand

1. **The `y` → attach path of `mirante allow`/`deny` on a Claude card** (needs a real TTY).
2. **Inline answers**, after reading the verdict: export `MIRANTE_CLAUDE_INLINE_ANSWERS=1`, `mirante daemon restart`, then run H "Evidence refusals" (a) and (b) and the lone-card rule (main agent plus side agent).
3. **Timing races:** requeued message not duplicated, `kill -9` during `archive`, and the interrupt-during-typing race.
4. **Never pasted into a dialog in this run:** a long wrapped Bash command dialog, and the bracketed `2` paste into a live dialog.
5. **Codex `grantRoot` session grant**, if Codex ever sends one (it sent `grantRoot: null` here).
6. **Memory:** `doctor --kill` on a real dev server, the stress procedure, and a day at normal memory pressure (this run was at kernel level 2).
7. **Reboot and the two-week log.**
