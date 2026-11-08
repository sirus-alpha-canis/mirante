# Cockpit dogfood checklist

Run against the real daemon and real agents, on a throwaway repo first, then on a real one.
Tick each line on the day you try it; write what went wrong next to it. The spec's success
metric is two consecutive weeks of using this instead of cmux / plain terminals.

This sits on top of the CLI dogfood (`2026-11-07-plan-2b-dogfood.md`, results in
`2026-11-07-plan-2b-results.md`): the same daemon, now seen through the app. Section I lists
what that run left for the founder to try by hand.

Setup: `cargo install --path crates/mirante-daemon`, `crates/mirante-hook`,
`crates/mirante-cli`; build and open the app (`apps/cockpit/README.md`). Keep
`mirante watch` open in a terminal the first day to compare.

- [ ] The app finds `mirante-daemon` in `~/.cargo/bin`, `~/.local/bin`, `/opt/homebrew/bin`
      or `/usr/local/bin`, then on your login shell's PATH (or at `$MIRANTE_DAEMON_BIN`):
      `cargo install` puts it in the first. The CLI and the daemon must be the same build.
- [ ] Optionally point the daemon at the real agents explicitly (PATH `claude` may be a cmux
      shim; Codex lives under nvm), in your shell profile so the app sees them too:
      `export MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude` and
      `export MIRANTE_CODEX_BIN=~/.nvm/versions/node/v22.14.0/bin/codex` (adjust the version).
      Without them the daemon looks on PATH (skipping cmux shims), then in `~/.local/bin`,
      `/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin` and nvm's newest Node that has
      the agent.
- [ ] Open the app from Finder or the Dock (not from a terminal) and use **Start daemon**:
      the app takes PATH and `MIRANTE_*` from your login shell (`$SHELL -l -i`, 3 s limit;
      nothing else from it) and the daemon finds `claude`, `codex` and `node`. Check with
      `mirante doctor`: the `agents` lines name each binary and where it came from
      (`MIRANTE_CLAUDE_BIN`, `PATH` or `well-known directory`). If your shell takes longer
      than 3 s to start, the app falls back to its own PATH plus the usual install
      directories; then `mirante daemon restart` from a shell.
- [ ] Leave `MIRANTE_CLAUDE_INLINE_ANSWERS` **unset** (the default). Claude cards then offer
      **Answer in terminal** instead of Allow/Deny; see C.

## A. Start and connect

- [ ] With no daemon running, the banner says it is not reachable; **Start daemon** starts it
      and the banner disappears within 2 s.
- [ ] First real render: home (V3) shows the rail (Projects, Agents, Memory), the project
      feed and the board; nothing overlaps or is cut at the minimum window size (960×600)
      and at full screen. Note anything that looks different from the prototype.
- [ ] `mirante daemon restart` while the app is open: the banner shows, then the app
      reconnects by itself; tasks and feeds are intact (no duplicate lines).
- [ ] Quit and reopen the app: every task, its feed (last 200 events) and pending
      permission cards are back.

## B. Dispatch (V3 home)

- [ ] Add a repo by absolute path; it appears under Projects.
- [ ] `@claude <repo>/<new-branch> <goal>` creates the worktree and starts Claude; the card
      appears at once and moves to Working.
- [ ] A new folder: the trust card appears; **Trust folder** continues, **Decline** ends the
      session (try both, on two tasks).
- [ ] `@codex <repo>/<branch> <goal>`: the card shows the **auto-edits** label.
- [ ] A typo (`@claud`, missing branch, unknown repo) shows the reason and calls nothing.

## C. Permissions

- [ ] Claude asks for a command with the daemon's inline answers off (default): the card shows
      tool and command and **Answer in terminal** (no Allow/Deny); clicking it opens the
      task's Terminal tab, focused, where answering in Claude's TUI clears the card.
- [ ] With `MIRANTE_CLAUDE_INLINE_ANSWERS=1` (only after Plan 2b Task 21 §H's evidence check
      passed, and with the daemon restarted from a shell that exports it): Claude asks for a
      command: the card shows tool and command; **Allow once** runs it, the card leaves;
      **Deny** stops it, the status leaves Needs you within 2 s.
- [ ] Codex asks for write access outside the sandbox: the card says
      **session-wide grant**.
- [ ] Answer a request from the CLI (`mirante allow <perm>`): the card in the app
      disappears without a click.

## D. Notifications

- [ ] First notification ever: macOS asks for permission once (it may not ask, or show
      nothing, while the unsigned app runs from `target/`; copy it to `/Applications` and
      retry). Record what happened. After allowing, Mirante is listed in System Settings →
      Notifications.
- [ ] App in the background, a task needs you: one macOS notification.
- [ ] App focused on that task's detail (or its project feed / grid tile): no notification.
- [ ] App focused on another task's detail: a notification.
- [ ] Opening the app with tasks already waiting: no burst of notifications for them.

## E. Detail (V1)

- [ ] Click a card: the detail opens with Steps; Esc returns home.
- [ ] Terminal tab: Claude's TUI is live and typing works; resizing the window resizes it.
      Codex shows a read-only stream with the note.
- [ ] Terminal input: paste a multi-line text (arrives as one paste, not one line per Enter);
      type Portuguese with accents and dead keys (`ação`, `você`, `pão`, `é`, `ç`, `~` + `a`)
      and check each character lands once and right; Esc inside the terminal goes to Claude,
      not back home.
- [ ] Composer with accents (dead keys, Enter while composing does not send).
- [ ] **Steer now** mid-turn: the feed shows "you · steer", then "message steered" when the
      transcript confirms it. **Queue**: "you · queued", delivered after the turn.
- [ ] Queue three messages during a long turn: **Edit** one (Save / Cancel), **Move up** /
      **Move down** to reorder, **Remove** one; after the turn they are delivered in the
      order shown and with the edited text (compare with `mirante watch`).
- [ ] Diff tab after a turn: files, +/− counts and the patch match `git diff` in the
      worktree.
- [ ] Ready for review card: **Open PR** is disabled until the branch is pushed, then opens
      GitHub's PR page in the browser.

## F. Grid (V2) and command bar

- [ ] ⌘G with 3+ live agents: tiles show last lines and status; allow/deny inline works; the
      mini input steers (Enter) and queues (⌥Enter).
- [ ] ⌘K → type part of a task title → Enter opens it; `@claude …` in ⌘K pre-fills the
      dispatch box. Arrows move the selection; Esc closes it.

## G. Memory guard

- [ ] The left rail shows system pressure and the heaviest sessions.
- [ ] **Hibernate** on an idle task: the process ends, the task shows "hibernated";
      **Wake** resumes the same conversation (ask about something from before it slept).
- [ ] Follow `docs/dogfood/memory-stress.md` §B with the app open: the pressure level and the
      "hibernate suggested" notice appear in the panel.

## H. Look and feel

- [ ] Light and dark (⇧⌘J, and the OS setting with theme "system") are both readable.
- [ ] Every action above works with the keyboard alone (Tab, Enter, arrows in tabs and ⌘K).
- [ ] VoiceOver reads card titles, permission groups and the connection banner.

## I. Carried over from the Plan 2b run (by hand)

From "Left for the founder to try by hand" in `2026-11-07-plan-2b-results.md`; do them with
the app open and note what the app showed.

- [ ] The `y` → attach path of `mirante allow` / `deny` on a Claude card (real TTY).
- [ ] Inline answers: only after reading that file's verdict, export
      `MIRANTE_CLAUDE_INLINE_ANSWERS=1`, `mirante daemon restart`, then Plan 2b §H
      "Evidence refusals" (a) and (b) and the lone-card rule (main agent plus side agent).
- [ ] Timing races: a requeued message is not duplicated; `kill -9` during `archive`; the
      interrupt-during-typing race.
- [ ] A long Bash command that Claude wraps in its dialog, and the bracketed `2` paste into a
      live dialog.
- [ ] Codex `grantRoot` session grant, if Codex ever sends one.
- [ ] `mirante doctor --kill <pid>` on a real orphaned dev server.
- [ ] The memory stress procedure (`docs/dogfood/memory-stress.md`) and one full day at
      normal memory pressure (the Plan 2b run was at kernel level 2).
- [ ] A reboot during the two weeks: reopen the app, resume everything, nothing recreated by
      hand.
- [ ] The two-week log below, and the Plan 2b verdict.

## Log

| Date | Section | Result | Notes |
|---|---|---|---|
