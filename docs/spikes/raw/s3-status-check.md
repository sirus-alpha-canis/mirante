# S3 status accuracy check (spike-claude-pty `status` mode)

Run on 2026-11-07, Claude Code 2.1.285 (/opt/homebrew/bin/claude), `--permission-mode manual`,
settings allowlist Bash(sleep:*), Bash(echo:*). Each run starts from a fresh sandbox
(`scripts/make-sandbox.sh ~/.mirante-spike/s3`). Command:
`spike-claude-pty ~/.mirante-spike/s3 ~/.mirante-spike/settings.json ~/.mirante-spike/hooks.jsonl 0 status 3`.

## Method

- The PTY output feeds a vt100 emulator (40x120); "screen" is the rendered grid, ANSI-free.
- Screen status (`spike_kit::screen::status`): process exited ⇒ Ended; "Do you want to proceed?"
  ⇒ NeedsYou; a spinner line (`<glyph> <Verb>… (…)`; the post-turn `✻ Baked for 3s` has no `…`)
  ⇒ Working; otherwise Idle.
- Hook status = `status::next_status` folded over the task's hook events with `ts_ms <= sample ts`
  (log read 1s after sampling so late writes still count). `hook 1s later` is shown for lag.
- Script per run: a) boot; b) "Responda apenas OK."; c) `touch s3-allowed.txt` → dialog → **1. Yes**
  (allow once; only if the dialog shows exactly that command); d) `touch s3-denied.txt` → dialog →
  **4. No**; e) `sleep 2 && echo a` x3 (allowlisted), 4 working samples; f) 60s+ idle until the idle
  Notification; g) `/exit`.
- Deviation from the brief: `date` and `uname -s` never showed a dialog. Claude Code 2.1.285
  auto-approves them as read-only even in manual mode (exploratory run: PreToolUse/PostToolUse, no
  PermissionRequest, no prompt). The dialog steps therefore use `touch` confined to the sandbox cwd.
  `s3-denied.txt` was never created.

## Summary

| run | checkpoints | mismatches |
|---|---|---|
| 1 | 17 | 1 |
| 2 | 17 | 2 |
| 3 | 17 | 1 |
| all | 51 | 4 |

Per step (all 3 runs): a, b, c-working, c-permission, c-after-Yes-working, d-working,
d-permission, e x5, f, g matched 3/3. `c after-Yes idle` matched 2/3, `d after-No idle` 0/3.

PermissionRequest: **fires**, once per dialog (2 per run, 6/6), ~20ms after the PreToolUse.
No permission Notification ("needs your permission") fired. The dialogs were answered within
~4s, so a delayed Notification never got a chance. Notification fired only for idle:
"Claude is waiting for your input", ~60s after the last Stop, mapped to Idle (3/3 match).

## Mismatches

1. **Deny leaves the hook status stuck at NeedsYou (3/3, systematic).** After choosing "No", the turn
   ends with `[Request interrupted by user for tool use]` (the transcript has `turn_duration` but no
   `stop_hook_summary`). No Stop, PostToolUse, or other hook fires, so hook status stays NeedsYou
   while the screen is idle at the input box. It recovers only at the next UserPromptSubmit.
   Fix candidates: PostToolUseFailure/other events if any exist, the transcript tail (a
   `turn_duration` line after a PermissionRequest), or the screen.
2. **Stray PreToolUse after Stop (run 2; also run 3, where it fell between checkpoints).**
   PreToolUse(Bash) with the same session_id arrived ~1.7s after Stop. No PostToolUse followed,
   and nothing for it appears in the main transcript. It flips hook status to Working while the
   screen is idle. This looks like background/side-agent tool use reported under the session.
   The hook log keeps only a subset of fields, so it can't tell which agent. Next step: also log
   `agent_id`/`agent_type` (or the raw payload) and ignore tool events from non-main agents.

Mismatch details (verbatim from the run; screen = last 25 grid lines):

### MISMATCH run 1 step 'd after-No idle' ts=1794070178216: hook=Some(NeedsYou) (hook 1s later=Some(NeedsYou)) screen=Idle
hook events (last 15s):
```
1794070166654 PostToolUse Bash
1794070167778 Stop 
1794070169247 SubagentStop 
1794070171362 UserPromptSubmit 
1794070172941 PreToolUse Bash
1794070172963 PermissionRequest Bash
```
screen (ANSI-free grid, last 25 lines):
```
 ▐▛███▛█   Claude Code v2.1.285
▝▜██████▀  Opus 5.5 · Claude Team
 ▝▝   ▝▝   ~/.mirante-spike/s3
❯ Responda apenas OK.
⏺ OK
✻ Cogitated for 1s · done 1:49 PM
❯ Use o Bash para rodar exatamente touch s3-allowed.txt e diga se deu certo.
  Ran 1 shell command
⏺ Deu certo: o touch s3-allowed.txt rodou sem erro e o arquivo foi criado no diretório atual.
✻ Churned for 3s · done 1:49 PM
❯ Use o Bash para rodar exatamente touch s3-denied.txt e diga se deu certo.
  Ran 1 shell command
  ⎿  Interrupted · What should Claude do instead?
✻ Churned for 1s · done 1:49 PM
                                                                                                    ◐ medium · /effort
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  Opus 5.5 (1.0M context) effort medium | ●○○○○○○○○○○○○○○○○○○○ 45.9k / 1.0M 4.6%
  s3 | (master) | $0.36 | 0h 0m 28s | 5h ▓▓▓▒▒▒▒▒▒▒ 34% ↻2h30m | 7d ▓▓▓▓▒▒▒▒▒▒ 40% ↻3d18h
  ⏸ manual mode on · ← for agents
```

### MISMATCH run 2 step 'c after-Yes idle' ts=1794070283263: hook=Some(Working) (hook 1s later=Some(Working)) screen=Idle
hook events (last 15s):
```
1794070269957 UserPromptSubmit 
1794070271087 Stop 
1794070274709 UserPromptSubmit 
1794070276242 PreToolUse Bash
1794070276261 PermissionRequest Bash
1794070279718 PostToolUse Bash
1794070281185 Stop 
1794070282850 PreToolUse Bash
```
screen (ANSI-free grid, last 25 lines):
```
 ▐▛███▛█   Claude Code v2.1.285
▝▜██████▀  Opus 5.5 · Claude Team
 ▝▝   ▝▝   ~/.mirante-spike/s3
❯ Responda apenas OK.
⏺ OK
✻ Brewed for 1s · done 1:51 PM
❯ Use o Bash para rodar exatamente touch s3-allowed.txt e diga se deu certo.
  Ran 1 shell command
⏺ Deu certo: o touch s3-allowed.txt rodou sem erro e sem saída, então o arquivo s3-allowed.txt foi criado (ou teve a
  data atualizada, se já existia) em ~/.mirante-spike/s3.
✻ Cooked for 3s · done 1:51 PM
                                                                                                    ◐ medium · /effort
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  Opus 5.5 (1.0M context) effort medium | ●○○○○○○○○○○○○○○○○○○○ 45.8k / 1.0M 4.6%
  s3 | (master) | $0.34 | 0h 0m 21s | 5h ▓▓▓▒▒▒▒▒▒▒ 34% ↻2h28m | 7d ▓▓▓▓▒▒▒▒▒▒ 40% ↻3d18h
  ⏸ manual mode on · ← for agents
```

### MISMATCH run 2 step 'd after-No idle' ts=1794070291721: hook=Some(NeedsYou) (hook 1s later=Some(NeedsYou)) screen=Idle
hook events (last 15s):
```
1794070279718 PostToolUse Bash
1794070281185 Stop 
1794070282850 PreToolUse Bash
1794070284705 UserPromptSubmit 
1794070286342 PreToolUse Bash
1794070286358 PermissionRequest Bash
```
screen (ANSI-free grid, last 25 lines):
```
 ▐▛███▛█   Claude Code v2.1.285
▝▜██████▀  Opus 5.5 · Claude Team
 ▝▝   ▝▝   ~/.mirante-spike/s3
❯ Responda apenas OK.
⏺ OK
✻ Brewed for 1s · done 1:51 PM
❯ Use o Bash para rodar exatamente touch s3-allowed.txt e diga se deu certo.
  Ran 1 shell command
⏺ Deu certo: o touch s3-allowed.txt rodou sem erro e sem saída, então o arquivo s3-allowed.txt foi criado (ou teve a
  data atualizada, se já existia) em ~/.mirante-spike/s3.
✻ Cooked for 3s · done 1:51 PM
❯ Use o Bash para rodar exatamente touch s3-denied.txt e diga se deu certo.
  Ran 1 shell command
  ⎿  Interrupted · What should Claude do instead?
✻ Cooked for 1s · done 1:51 PM
                                                                                                    ◐ medium · /effort
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  Opus 5.5 (1.0M context) effort medium | ●○○○○○○○○○○○○○○○○○○○ 45.9k / 1.0M 4.6%
  s3 | (master) | $0.36 | 0h 0m 28s | 5h ▓▓▓▒▒▒▒▒▒▒ 34% ↻2h28m | 7d ▓▓▓▓▒▒▒▒▒▒ 40% ↻3d18h
  ⏸ manual mode on · ← for agents
```

### MISMATCH run 3 step 'd after-No idle' ts=1794070411142: hook=Some(NeedsYou) (hook 1s later=Some(NeedsYou)) screen=Idle
hook events (last 15s):
```
1794070398154 PostToolUse Bash
1794070400277 Stop 
1794070402543 PreToolUse Bash
1794070403817 UserPromptSubmit 
1794070405834 PreToolUse Bash
1794070405849 PermissionRequest Bash
```
screen (ANSI-free grid, last 25 lines):
```
 ▐▛███▛█   Claude Code v2.1.285
▝▜██████▀  Opus 5.5 · Claude Team
 ▝▝   ▝▝   ~/.mirante-spike/s3
❯ Responda apenas OK.
⏺ OK
✻ Worked for 3s · done 1:53 PM
❯ Use o Bash para rodar exatamente touch s3-allowed.txt e diga se deu certo.
  Ran 1 shell command
⏺ Deu certo: o touch s3-allowed.txt terminou sem erro e criou o arquivo s3-allowed.txt no diretório atual.
✻ Sautéed for 5s · done 1:53 PM
❯ Use o Bash para rodar exatamente touch s3-denied.txt e diga se deu certo.
  Ran 1 shell command
  ⎿  Interrupted · What should Claude do instead?
✻ Churned for 2s · done 1:53 PM
                                                                                                    ◐ medium · /effort
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  Opus 5.5 (1.0M context) effort medium | ●○○○○○○○○○○○○○○○○○○○ 45.9k / 1.0M 4.6%
  s3 | (master) | $0.36 | 0h 0m 33s | 5h ▓▓▓▒▒▒▒▒▒▒ 34% ↻2h26m | 7d ▓▓▓▓▒▒▒▒▒▒ 40% ↻3d18h
  ⏸ manual mode on · ← for agents
```

## Permission dialog as seen on screen (run 1, step c)

```
 ▐▛███▛█   Claude Code v2.1.285
▝▜██████▀  Opus 5.5 · Claude Team
 ▝▝   ▝▝   ~/.mirante-spike/s3
❯ Responda apenas OK.
⏺ OK
✻ Cogitated for 1s · done 1:49 PM
❯ Use o Bash para rodar exatamente touch s3-allowed.txt e diga se deu certo.
⏺ Creating empty file s3-allowed.txt
  ⎿  $ touch s3-allowed.txt
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
 Bash command
 Tip: auto mode handles these prompts for you — choose "switch to auto mode" below
   touch s3-allowed.txt
   Create empty file s3-allowed.txt
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to ~/.mirante-spike/s3 from this project
   3. Yes, and switch to auto mode · auto mode handles these prompts for you
   4. No
 Esc to cancel · Tab to amend
```

## Working screen (run 1, step e)

```
⏺ Waiting 2 seconds then printing a
  ⎿  $ sleep 2 && echo a
✶ Skedaddling… (2s · ↓ 195 tokens · thought for 1s)
  ⎿  Tip: You can control how big a workflow is just by prompting. Ask for a small workflow, cap it with "use at most 5
     agents", or set a default with Dynamic workflow size in /config.
                                                                                                    ◐ medium · /effort
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  Opus 5.5 (1.0M context) effort medium | ●○○○○○○○○○○○○○○○○○○○ 46.1k / 1.0M 4.6%
  s3 | (master) | $0.37 | 0h 0m 35s | 5h ▓▓▓▒▒▒▒▒▒▒ 34% ↻2h30m | 7d ▓▓▓▓▒▒▒▒▒▒ 40% ↻3d18h
  ⏸ manual mode on · ← for agents
```

## Full per-checkpoint output

```
run 1: task s3-1-1794070146779
  a boot                       ts=1794070154803 hook=Some(Idle) screen=Idle       match
  b working                    ts=1794070156590 hook=Some(Working) screen=Working    match
  b idle                       ts=1794070159759 hook=Some(Idle) screen=Idle       match
  c working                    ts=1794070161651 hook=Some(Working) screen=Working    match
  c permission                 ts=1794070164796 hook=Some(NeedsYou) screen=NeedsYou   match
  (c: answered Yes)
  c after-Yes working          ts=1794070166733 hook=Some(Working) screen=Working    match
  c after-Yes idle             ts=1794070169909 hook=Some(Idle) screen=Idle       match
  d working                    ts=1794070171800 hook=Some(Working) screen=Working    match
  d permission                 ts=1794070174483 hook=Some(NeedsYou) screen=NeedsYou   match
  (d: answered No)
  d after-No idle              ts=1794070178216 hook=Some(NeedsYou) screen=Idle       MISMATCH
  e working #0                 ts=1794070180109 hook=Some(Working) screen=Working    match
  e working #1                 ts=1794070182626 hook=Some(Working) screen=Working    match
  e working #2                 ts=1794070185149 hook=Some(Working) screen=Working    match
  e working #3                 ts=1794070187671 hook=Some(Working) screen=Working    match
  e idle                       ts=1794070194334 hook=Some(Idle) screen=Idle       match
  f idle-notification          ts=1794070256367 hook=Some(Idle) screen=Idle       match
  g exit                       ts=1794070259342 hook=Some(Ended) screen=Ended      match
  hook counts: PermissionRequest=2 Notification=1 ["Claude is waiting for your input"]
run 2: task s3-2-1794070260447
  a boot                       ts=1794070268467 hook=Some(Idle) screen=Idle       match
  b working                    ts=1794070270255 hook=Some(Working) screen=Working    match
  b idle                       ts=1794070273276 hook=Some(Idle) screen=Idle       match
  c working                    ts=1794070275167 hook=Some(Working) screen=Working    match
  c permission                 ts=1794070277843 hook=Some(NeedsYou) screen=NeedsYou   match
  (c: answered Yes)
  c after-Yes working          ts=1794070279765 hook=Some(Working) screen=Working    match
  c after-Yes idle             ts=1794070283263 hook=Some(Working) screen=Idle       MISMATCH
  d working                    ts=1794070285152 hook=Some(Working) screen=Working    match
  d permission                 ts=1794070287994 hook=Some(NeedsYou) screen=NeedsYou   match
  (d: answered No)
  d after-No idle              ts=1794070291721 hook=Some(NeedsYou) screen=Idle       MISMATCH
  e working #0                 ts=1794070293612 hook=Some(Working) screen=Working    match
  e working #1                 ts=1794070296130 hook=Some(Working) screen=Working    match
  e working #2                 ts=1794070298659 hook=Some(Working) screen=Working    match
  e working #3                 ts=1794070301178 hook=Some(Working) screen=Working    match
  e idle                       ts=1794070309285 hook=Some(Idle) screen=Idle       match
  f idle-notification          ts=1794070371329 hook=Some(Idle) screen=Idle       match
  g exit                       ts=1794070374293 hook=Some(Ended) screen=Ended      match
  hook counts: PermissionRequest=2 Notification=1 ["Claude is waiting for your input"]
run 3: task s3-3-1794070375398
  a boot                       ts=1794070383420 hook=Some(Idle) screen=Idle       match
  b working                    ts=1794070385214 hook=Some(Working) screen=Working    match
  b idle                       ts=1794070390597 hook=Some(Idle) screen=Idle       match
  c working                    ts=1794070392491 hook=Some(Working) screen=Working    match
  c permission                 ts=1794070396286 hook=Some(NeedsYou) screen=NeedsYou   match
  (c: answered Yes)
  c after-Yes working          ts=1794070398225 hook=Some(Working) screen=Working    match
  c after-Yes idle             ts=1794070402364 hook=Some(Idle) screen=Idle       match
  d working                    ts=1794070404244 hook=Some(Working) screen=Working    match
  d permission                 ts=1794070407410 hook=Some(NeedsYou) screen=NeedsYou   match
  (d: answered No)
  d after-No idle              ts=1794070411142 hook=Some(NeedsYou) screen=Idle       MISMATCH
  e working #0                 ts=1794070413034 hook=Some(Working) screen=Working    match
  e working #1                 ts=1794070415563 hook=Some(Working) screen=Working    match
  e working #2                 ts=1794070418089 hook=Some(Working) screen=Working    match
  e working #3                 ts=1794070420608 hook=Some(Working) screen=Working    match
  e idle                       ts=1794070430135 hook=Some(Idle) screen=Idle       match
  f idle-notification          ts=1794070492172 hook=Some(Idle) screen=Idle       match
  g exit                       ts=1794070495157 hook=Some(Ended) screen=Ended      match
  hook counts: PermissionRequest=2 Notification=1 ["Claude is waiting for your input"]
```
