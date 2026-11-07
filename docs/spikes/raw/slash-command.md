# Local slash commands: real capture (`/context`, `/cost`)

Run on 2026-11-07 with Claude Code 2.1.285 (`/opt/homebrew/bin/claude`, set through `MIRANTE_CLAUDE_BIN`). Each run used a fresh marker-guarded sandbox from `scripts/make-sandbox.sh "$HOME/.mirante-spike/slash-sandbox"` and the scoped spike settings (`~/.mirante-spike/settings.json`: the `mirante-hook` logger on every hook, allow-list `Bash(sleep:*)`, `Bash(echo:*)`). No permission was broadened.

```
cargo build -p spike-kit --bins
MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude ./target/debug/spike-claude-pty <sandbox> \
  "$HOME/.mirante-spike/settings.json" "$HOME/.mirante-spike/hooks.jsonl" 0 prompts "/context" "Responda apenas OK."
```

`prompts` mode now gives a prompt that starts with `/` a fixed 8 s instead of waiting for a `Stop`, because a local command may fire none.

## `/context` then "Responda apenas OK."

Hooks logged for the task, in order (ms since epoch):

| ts | hook |
|---|---|
| …726099 | `SessionStart` (`source: startup`) |
| …737551 | `UserPromptSubmit` (for "Responda apenas OK.", 14 ms after its transcript line) |
| …738809 | `Stop` |
| …740013 | `SessionEnd` |

**`/context` fired no hook at all**: no `UserPromptSubmit`, no `Stop`, no `Notification`. The command ran at 18:38:49.809 (transcript timestamps). The only `UserPromptSubmit` came 7.7 s later, at 18:38:57.551, for the second prompt.

Transcript lines that `/context` wrote, all with the same timestamp. Ids, cwd, session and branch are left out:

```json
{"type":"system","subtype":"local_command","isMeta":false,"isSidechain":false,"level":"info",
 "content":"<command-name>/context</command-name>\n            <command-message>context</command-message>\n            <command-args></command-args>"}
{"type":"system","subtype":"local_command","isMeta":false,"isSidechain":false,"level":"info",
 "content":"<local-command-stdout> \u001b[1mContext Usage\u001b[22m ... (ANSI grid)","commandRun":{"command":"context","args":""}}
{"type":"user","isMeta":true,"isSidechain":false,"promptId":"…",
 "message":{"role":"user","content":"## Context Usage\n\n**Model:** …\n**Tokens:** 31.3k / 1m (3%) …"}}
```

- None of the three lines has `origin`. The two wrappers are `system` lines, not `user` lines. The markdown copy is a `user` line with `isMeta: true`.
- No `stop_hook_summary` and no `turn_duration` followed. Bookkeeping lines (`last-prompt`, `mode`, `permission-mode`, `file-history-snapshot`) came next.

Then the next prompt was a normal turn: a `user` line with `origin.kind: "human"` and `promptSource: "typed"`, then attachments, an `assistant` "OK", `stop_hook_summary` 1 ms after the `Stop` hook, and `turn_duration`.

## `/cost` then "Responda apenas OK."

Only `SessionStart` (`startup`) fired. No transcript file was created. In 2.1.285, `/cost` seems to open an interactive panel that took the next typed line, so the second prompt was never submitted. The runner timed out waiting for its `Stop` and closed the session. So `/cost` also fires no hook. Its transcript shape was not captured.

## What the engine does with this (`crates/mirante-core/src/status.rs`)

- With no `UserPromptSubmit`, `slash_pending` is never set for `/context`. None of its lines is a prompt line: the wrappers are `system` lines, and the markdown copy has no `origin` and is `isMeta`. So no turn opens, and the status stays `Idle`. Fixture test: `fixture_real_context_command_opens_no_turn`.
- As a defense against other versions, `is_prompt` now also rejects `isMeta` lines, and `user` lines whose text starts with a local-command wrapper (`<command-name>`, `<command-message>`, `<command-args>`, `<local-command-stdout|stderr|caveat>`). It does this whether or not `slash_pending` is set, because the real capture shows a local command may set nothing. Test: `local_command_wrapper_user_lines_are_not_prompts`.
- One consequence: a command that runs the model and writes its prompt as a `<command-name>` wrapper does not open the turn from the transcript. Its turn opens on its first main-agent `PreToolUse`, if a `UserPromptSubmit` marked it as pending. A command that answers with text only may show no `Working` phase. Not yet captured: `/compact`, `/review` and custom commands (does `UserPromptSubmit` fire for them, and what shape is their prompt line?).
