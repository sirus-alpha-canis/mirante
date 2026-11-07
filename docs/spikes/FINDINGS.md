# Week-1 spike findings

Environment: macOS 26.5.2 (arm64), Claude Code 2.1.285 (`/opt/homebrew/bin/claude`), Codex CLI 0.167.0 (logged in with ChatGPT from Task 10 on), Rust 1.99.1. Date 2026-11-07.
Raw evidence: `docs/spikes/raw/task8-outputs.md`, `docs/spikes/raw/s3-status-check.md`, `docs/spikes/raw/task9-outputs.md`, `docs/spikes/raw/task10-outputs.md`.

## Summary

| Spike | Path | Result | Evidence | Decision |
|---|---|---|---|---|
| S1 Claude | Interactive PTY + hooks | **Steered** (2/2 runs). Asked for 5 steps, then sent "stop, say BANANA" 6 s later. Claude stopped after step 1 or 2 and answered BANANA in the same turn. | The transcript records `queue-operation enqueue` and then `remove` with `reason: absorbed_mid_turn` at the next tool boundary. Transcript classifier → Steered. | ✅ Mid-run steering works |
| S1 Claude | Headless `claude -p` stream-json | **Steered** (2/2). The message was absorbed during step 1. There was a single `result` with `num_turns: 2` and `result: "BANANA"`. | Same `absorbed_mid_turn` record. Stream and transcript classifiers both say Steered. | ✅ Works, but see S2/terms |
| S1 Codex | app-server `turn/steer` | **Steered** (2/2 runs). Asked for 5 steps, then sent `turn/steer` "stop, say BANANA" 8 s after `turn/start`. Codex stopped after step 1 (run 1) or step 2 (run 2) and answered BANANA in the same turn. | `turn/steer` returned the same `turnId`. The steer appeared as a `userMessage` item inside the running turn, then `agentMessage` "BANANA" (`phase: final_answer`), then the only `turn/completed`. `classify_codex` → Steered. | ✅ Mid-run steering is a first-class protocol call |
| S2 Quota | Interactive vs `-p`, same 3 prompts | Interactive used 277,482 tokens (6 calls). Headless used 184,833 (6 calls, 3 processes). Cache reads were 91.5% and 68.9% of those totals. | Headless writes a fresh ~18–20k-token cache in every process. Interactive writes its cache once but reads a larger context. The 5-hour plan meter moved 0.35→0.36 for all of Task 9, too coarse to compare modes. | **Inconclusive on cost.** The decision is driven by the terms of use (below), not by tokens. |
| S3 Status | Hooks only | **4 mismatches in 51 checkpoints** over 3 scripted runs. | Mismatch 1 (3×): after a permission is **denied**, no hook fires, so the status stays `needs_you`. Mismatch 2 (1×): a stray `PreToolUse` from a side agent arrives after `Stop`. Everything else matched, including `PermissionRequest` firing 6 of 6 times and the idle Notification. | ✅ Usable, but **must fuse hooks + transcript tail** (below) |
| S4 Resume | `claude --resume <id>` | **PASS** (2/2). The secret "JABUTICABA" was recalled after the process was killed and resumed. | `SessionStart` with `source=resume`, the same session id and the same transcript file. | ✅ Persistence via native resume |
| S4 Codex | app-server `thread/resume` | **PASS** (1/1). The secret was set in app-server process A. A was reaped, and a new process B ran `thread/resume` by id and answered "JABUTICABA". | `thread/resume` returned the same thread id with no error. | ✅ Persistence via native thread resume |

## Driver decision per agent

- **Claude Code: interactive PTY + hooks + transcript tail. Recommended; pending the founder's decision.**
  - **Terms.** Anthropic's consumer terms (§3.7) ban "automated or non-human means… except… where we otherwise explicitly permit it". The Claude Code legal page bans third parties from routing requests through users' Pro/Max credentials or handling their tokens. It allows "an end user… signing in to the unmodified Claude Code binary with their own Claude subscription". The interactive mode is a human-attended, unmodified binary with the user's own login, so it fits that allowance cleanly.
  - **Future-proof.** The docs say `--bare` "will become the default for `-p`". Bare mode does not use the subscription login, so a `-p`-based driver would stop working for subscription users.
  - **Capability.** The interactive mode was proven to steer mid-run (S1), resume (S4) and give reliable status once hooks are fused with the transcript (S3).
  - **Trade-off.** The live plan meter (`rate_limit_event`) is only available in `-p` stream-json. In interactive mode we lose it and would need another way to show quota.
- **Codex: `codex app-server` JSON-RPC over stdio. Verified (Task 10).**
  - **Protocol.** The real names match the plan: `initialize` / `initialized`, `thread/start`, `turn/start`, `turn/steer` (`threadId` + `expectedTurnId` + `input`), `turn/interrupt` (`threadId` + `turnId`) and `thread/resume`. The notifications are `turn/started`, `turn/completed`, `item/started` / `item/completed` (with `agentMessage`, `userMessage` and `commandExecution` items), plus `thread/status/changed` (`active` / `idle`). The full table is in `raw/task10-outputs.md`.
  - **Steering** is explicit and acknowledged: the server echoes the `turnId`, and a wrong `expectedTurnId` fails. That is stronger than Claude, where steering has to be inferred from the transcript.
  - **Status** comes straight from typed notifications. No hook/transcript fusion is needed for Codex.
  - **Resume** is native (`thread/resume` by id across processes).
  - **Approvals** are JSON-RPC server→client requests (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`), so Mirante can answer them from its own UI. They were not exercised: the spike used `approvalPolicy: "never"` inside a `workspace-write` sandbox limited to the throwaway repo, with network off. That setting is **spike-only**.
  - **Launch hygiene** mirrors Claude: use the real binary (`MIRANTE_CODEX_BIN`; PATH `codex` may be the cmux shim) and scrub every `CMUX_*` var. Codex auth lives in `~/.codex/auth.json` and is never touched.
  - **Open:** the terms-of-use review for driving Codex with a ChatGPT login through app-server. The app-server is the documented IDE-integration surface, but it is not yet reviewed for Mirante's use.

## Rules learned (inputs for Plan 2)

1. **Status = hooks fused with the transcript.**
   - Hooks are fast, but:
     - no hook fires when the user denies a permission (the turn ends as "[Request interrupted by user for tool use]");
     - side agents emit tool events after `Stop`.
   - Fix:
     - Treat the transcript's `system` lines with subtype `turn_duration` / `stop_hook_summary` as authoritative turn ends.
     - Filter hook events by agent identity: log `agent_id` / `agent_type` from the payload.
2. **Steering is confirmed from the transcript, not from `UserPromptSubmit`.**
   - `UserPromptSubmit` fires when the message is *enqueued*, not when it is delivered, so the hooks-only classifier wrongly said "Steered" for a message that was really queued.
   - The rule, by **file order** (timestamps are not monotonic):
     - enqueue during a turn, then `remove` with `reason: absorbed_mid_turn` before the turn ends → **Steered**;
     - `dequeue`, then a user line with `promptSource: "queued"` → **Queued**.
   - Headless prompt lines carry `promptSource: "sdk"`. Interactive prompts carry `origin.kind: "human"`.
3. **The idle Notification is not "needs you".** `Notification` with message "Claude is waiting for your input" maps to `idle`. Other Notifications and `PermissionRequest` map to `needs_you`.
4. **Launching Claude from Mirante:**
   - Use the real binary. `claude` on PATH may be a wrapper shim; cmux installs one.
   - Scrub the session env vars so Claude doesn't think it's nested:
     - `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_MESSAGING_*`, `CLAUDE_PID`, …;
     - `CMUX_CLAUDE_*` / `CMUX_AGENT_*`.
     - Never scrub auth variables.
   - Pass `--permission-mode manual`. The user's global `defaultMode` may be `auto`, and `--settings` is **additive** to `~/.claude`: the user's own hooks, skills and MCP servers still load.
5. **Trust dialog.** On a new folder the default option is **"No, exit"**. Mirante must show this decision to the user, never auto-accept it silently. (Task 21: the options carry **no digits**; accepting is Down, then Enter; trust is kept per repository. See below.)
6. **Permission prompts.**
   - Read-only commands such as `date` and `git ls-files` are auto-approved by Claude even in manual mode.
   - For real prompts, the `PermissionRequest` hook fires reliably.
   - Answering from the UI requires:
     - matching the dialog's options exactly;
     - never selecting "always allow" or "auto mode";
     - searching only *below* the dialog header.
7. **Quota display.** Show the plan meter from `-p` `rate_limit_event` only if we ever use `-p`. For interactive sessions, quota visibility needs another source; this is an open item.

8. **Codex: `turn/completed` does not mean every tool process has finished.** In S1 Codex run 2 the `passo-2` command was already in flight when the steer landed. Codex answered BANANA and sent `turn/completed`, and that command's `item/completed` arrived 1.3 s **after** `turn/completed`. The status engine must:
   - track open `commandExecution` items by id;
   - accept item events after `turn/completed` without reopening the turn;
   - surface a "turn done, a background command is still finishing" state rather than "idle".
9. **Codex steer timing.** A steer lands at the next model step, not mid-command. A tool call already issued keeps running. If Mirante must stop that call too, it needs `turn/interrupt`, not `turn/steer`.

## Changes to the spec / Plan 2 inputs

- **Claude driver:** interactive PTY. The `stream-json` path stays as a documented fallback.
- **Status engine:** a hooks + transcript-tail fusion with agent-id filtering. It must cover the deny path and side agents, which are the two S3 failure classes.
- **Steer UI:** "steer now" is real for Claude: typed messages are absorbed at the next tool boundary. "Queue" is the native behaviour when no tool boundary comes before the turn ends. The UI should show which one happened, based on the transcript. For Codex, "steer now" is `turn/steer` (acked with the turn id) and "stop now" is `turn/interrupt`.
- **Resume:** store the native `session_id` from `SessionStart` and relaunch with `--resume`. For Codex, store the thread id and call `thread/resume`.
- **Open:**
  - ~~Codex S1/S4 (needs login)~~ done in Task 10: steering and resume both work through app-server;
  - Codex terms review for app-server with a ChatGPT login;
  - the founder's decision on interactive vs headless for Claude;
  - quota visibility in interactive mode;
  - legal confirmation that orchestrating the user's own interactive sessions is "ordinary, individual usage".

## Plan 2b Task 21: live facts (2026-11-07, Claude Code 2.1.285, Codex 0.167.0)

Full evidence, screen excerpts and the inline-answers verdict: `docs/dogfood/2026-11-07-plan-2b-results.md`.

### Claude TUI

- **Trust dialog.**
  - What it shows:
    - `Accessing workspace:` / the path / `Quick safety check: …` / `Security guide`;
    - then `❯ No, exit` / `  Yes, I trust this folder`, with **no digits**;
    - footer `Enter to confirm · Esc to cancel`.
  - Answering it:
    - a typed digit does nothing;
    - Down, then Enter, accepts;
    - the first Down sent right after the dialog appears can be lost.
  - `SessionStart` fires only after the answer.
  - Trust is kept per repository: a later worktree of a trusted repo boots without the dialog.
- **Permission dialog box.**
  - Border: a `─` row at column 0 spanning the full width (checked at 70, 120 and 200 columns).
  - Title and the `Tip: …` line: indent 1.
  - Bash: the command, then the hook's `tool_input.description`, both at indent 3.
  - The Tip wraps at narrow widths.
  - Titles:
    - `Bash command`; a side agent adds ` · from the general-purpose agent`;
    - `Edit file`;
    - Write: `Create file` (new file) and `Overwrite file` (existing file).
  - Write and Edit put the path at indent 1, followed by a `╌` preview or diff box. Their options are `Yes` / `Yes, and switch to accept edits …` / `No`.
- **Status line.** There is no "esc to interrupt" text. The running line is the spinner (`✶ Recombobulating… (4s · ↓ 124 tokens)` / `(running PreToolUse hook · 1s)`). The footer is `⏸ manual mode on · ← for agents`. "Esc to" appears only in dialog footers.
- **Input box.**
  - Bracketed paste lands as typed lines; 40 lines collapse to `[Pasted text #1 +39 lines]`.
  - A pasted bare `2` stays in the box.
  - Ctrl-U clears one line's text or joins an empty line, so n pasted lines need 2n − 1 Ctrl-U after Ctrl-E. A collapsed paste goes with one Ctrl-U.
  - A dim prompt suggestion can sit in the box after a turn.
- **Interrupt.** Esc during a streamed answer writes only the user line `[Request interrupted by user]` (with `interruptedMessageId`). No Stop hook and no `turn_duration` follow, so the line itself must end the turn.
- **Hook and transcript timing for a permission.** `PreToolUse` (carries `tool_use_id`) → dialog drawn (~35 ms) → `PermissionRequest` (**no** `tool_use_id`, ~90 ms) → assistant `tool_use` line in the transcript (~130 ms, i.e. ~95 ms after the dialog). The Stop hook can also come before the turn's last assistant text is in the transcript.
- **Side agents.**
  - Their lines go to `~/.claude/projects/<dir>/<session>/subagents/agent-<agentId>.jsonl` (`isSidechain: true`, `agentId`), plus a `.meta.json` file. They are not in the main transcript.
  - A backgrounded agent's completion starts a new main turn.
- **`/compact <args>`.** Runs with its arguments (`<command-args>`) and writes no prompt or assistant line, so no turn opens.
- **User config inside sessions.** The user's statusline, hooks, skills and MCP servers load inside Mirante's sessions. The user's Serena MCP writes `.serena/` into every worktree.

### Codex app-server

- **Approval requests.**
  - Command approval request: `item/commandExecution/requestApproval`, params `kind, threadId, turnId, itemId, startedAtMs, environmentId, reason, command, cwd, commandActions, proposedExecpolicyAmendment, availableDecisions`.
  - `availableDecisions` is `["accept", {"acceptWithExecpolicyAmendment":…}, "cancel"]`, yet `{"decision":"decline"}` is accepted (the item completes `"status":"declined"`).
  - File change outside the worktree: `item/fileChange/requestApproval` with `reason: null, grantRoot: null`. The paths are in the earlier `item/started` `fileChange` item (`changes[].{path, kind.type, diff}`; for `add`, `diff` is the raw content).
- **Missing thread.** `thread/resume` of an unknown id fails with error `-32600` `"no rollout found for thread id <id>"`.
- **Interrupt.** `turn/interrupt` ends the turn, but the command already running keeps running under app-server until it finishes.
- **Background jobs.** A command's background job (`nohup … &`) is reaped when the command returns.
- **Sandbox.** The effective sandbox reported matches auto-edits: workspace-write, network off, worktree-only writes. Violations are logged by seatbelt (`operation_not_permitted`).
