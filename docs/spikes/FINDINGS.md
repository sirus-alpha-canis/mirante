# Week-1 spike findings

Environment: macOS 26.5.2 (arm64), Claude Code 2.1.285 (`/opt/homebrew/bin/claude`), Codex CLI 0.167.0 (installed, **not logged in**), Rust 1.99.1. Date 2026-11-07.
Raw evidence: `docs/spikes/raw/task8-outputs.md`, `docs/spikes/raw/s3-status-check.md`, `docs/spikes/raw/task9-outputs.md`.

## Summary

| Spike | Path | Result | Evidence | Decision |
|---|---|---|---|---|
| S1 Claude | Interactive PTY + hooks | **Steered** (2/2 runs). Asked for 5 steps, then sent "stop, say BANANA" 6 s later. Claude stopped after step 1 or 2 and answered BANANA in the same turn. | The transcript records `queue-operation enqueue` and then `remove` with `reason: absorbed_mid_turn` at the next tool boundary. Transcript classifier → Steered. | ✅ Mid-run steering works |
| S1 Claude | Headless `claude -p` stream-json | **Steered** (2/2). The message was absorbed during step 1. There was a single `result` with `num_turns: 2` and `result: "BANANA"`. | Same `absorbed_mid_turn` record. Stream and transcript classifiers both say Steered. | ✅ Works, but see S2/terms |
| S1 Codex | app-server `turn/steer` | **Not run.** Codex is installed but not logged in. | — | Deferred to the start of Plan 2 |
| S2 Quota | Interactive vs `-p`, same 3 prompts | Interactive used 277,482 tokens (6 calls). Headless used 184,833 (6 calls, 3 processes). Cache reads were 91.5% and 68.9% of those totals. | Headless writes a fresh ~18–20k-token cache in every process. Interactive writes its cache once but reads a larger context. The 5-hour plan meter moved 0.35→0.36 for all of Task 9, too coarse to compare modes. | **Inconclusive on cost.** The decision is driven by the terms of use (below), not by tokens. |
| S3 Status | Hooks only | **4 mismatches in 51 checkpoints** over 3 scripted runs. | Mismatch 1 (3×): after a permission is **denied**, no hook fires, so the status stays `needs_you`. Mismatch 2 (1×): a stray `PreToolUse` from a side agent arrives after `Stop`. Everything else matched, including `PermissionRequest` firing 6 of 6 times and the idle Notification. | ✅ Usable, but **must fuse hooks + transcript tail** (below) |
| S4 Resume | `claude --resume <id>` | **PASS** (2/2). The secret "JABUTICABA" was recalled after the process was killed and resumed. | `SessionStart` with `source=resume`, the same session id and the same transcript file. | ✅ Persistence via native resume |

## Driver decision per agent

- **Claude Code: interactive PTY + hooks + transcript tail. Recommended; pending the founder's decision.**
  - **Terms.** Anthropic's consumer terms (§3.7) ban "automated or non-human means… except… where we otherwise explicitly permit it". The Claude Code legal page bans third parties from routing requests through users' Pro/Max credentials or handling their tokens. It allows "an end user… signing in to the unmodified Claude Code binary with their own Claude subscription". The interactive mode is a human-attended, unmodified binary with the user's own login, so it fits that allowance cleanly.
  - **Future-proof.** The docs say `--bare` "will become the default for `-p`". Bare mode does not use the subscription login, so a `-p`-based driver would stop working for subscription users.
  - **Capability.** The interactive mode was proven to steer mid-run (S1), resume (S4) and give reliable status once hooks are fused with the transcript (S3).
  - **Trade-off.** The live plan meter (`rate_limit_event`) is only available in `-p` stream-json. In interactive mode we lose it and would need another way to show quota.
- **Codex: `codex app-server` JSON-RPC (planned).** Unverified, because Codex was not logged in. The first task of Plan 2 is to run `spike-codex` once the founder runs `codex login --device-auth`.

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
5. **Trust dialog.** On a new folder the default option is **"No, exit"**. Mirante must show this decision to the user, never auto-accept it silently.
6. **Permission prompts.**
   - Read-only commands such as `date` and `git ls-files` are auto-approved by Claude even in manual mode.
   - For real prompts, the `PermissionRequest` hook fires reliably.
   - Answering from the UI requires:
     - matching the dialog's options exactly;
     - never selecting "always allow" or "auto mode";
     - searching only *below* the dialog header.
7. **Quota display.** Show the plan meter from `-p` `rate_limit_event` only if we ever use `-p`. For interactive sessions, quota visibility needs another source; this is an open item.

## Changes to the spec / Plan 2 inputs

- **Claude driver:** interactive PTY. The `stream-json` path stays as a documented fallback.
- **Status engine:** a hooks + transcript-tail fusion with agent-id filtering. It must cover the deny path and side agents, which are the two S3 failure classes.
- **Steer UI:** "steer now" is real for Claude: typed messages are absorbed at the next tool boundary. "Queue" is the native behaviour when no tool boundary comes before the turn ends. The UI should show which one happened, based on the transcript.
- **Resume:** store the native `session_id` from `SessionStart` and relaunch with `--resume`.
- **Open:**
  - Codex S1/S4 (needs login);
  - the founder's decision on interactive vs headless for Claude;
  - quota visibility in interactive mode;
  - legal confirmation that orchestrating the user's own interactive sessions is "ordinary, individual usage".
