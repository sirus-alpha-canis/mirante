# Cockpit dogfood checklist

Run against the real daemon and real agents, on a throwaway repo first, then on a real one.
Tick each line on the day you try it; write what went wrong next to it. The spec's success
metric is two consecutive weeks of using this instead of cmux / plain terminals.

This sits on top of the CLI dogfood (`2026-11-07-plan-2b-dogfood.md`, results in
`2026-11-07-plan-2b-results.md`): the same daemon, now seen through the app. Section I lists
what that run left for the founder to try by hand.

Setup: `scripts/install.sh --app` (the four binaries linked into `~/.local/bin`, the
daemon started, `mirante doctor`, and the app built; `--install-app` also copies it to
`/Applications`; see the README's Quickstart), then `mirante app` to open it (the daemon has
no window of its own). Keep `mirante watch` open in a terminal the first day to compare.

- [ ] The app finds `mirante-daemon` in `~/.cargo/bin`, `~/.local/bin`, `/opt/homebrew/bin`
      or `/usr/local/bin`, then on your login shell's PATH (or at `$MIRANTE_DAEMON_BIN`):
      `install.sh` links it into `~/.local/bin`. The CLI and the daemon must be the same build.
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

- [ ] With no daemon running, the banner says it is not reachable and counts down to the
      next try; **Start daemon** starts it and the banner disappears within 2 s.
- [ ] First real render: Home is the inbox (**Needs you**, **Ready for review**, **Working**,
      **Idle** folded away, grouped by project) with the dispatch form on top and the rail
      (Projects, Sessions) on the left; nothing overlaps or is cut at the minimum window size
      (960×600) and at full screen.
- [ ] `mirante daemon restart` while the app is open: the banner shows, then the app
      reconnects by itself; tasks and their Steps are intact (no duplicate lines).
- [ ] The top bar's Orb-Dot mark + "Mirante" is the Home button (pressed on Home; ⌘1 too);
      there is no separate Home tab.
- [ ] After an update (`scripts/install.sh` with the old daemon still running): a banner says
      "Mirante was updated — restart the daemon…"; **Restart daemon** brings the new one up and
      the banner goes; live sessions come back (auto-resume on). `mirante doctor` says
      "restart with: mirante daemon restart" until then.
- [ ] Quit and reopen the app: it reopens where you were (view, project, open task and tab);
      every task, its steps and pending permission cards are back.

## B. Dispatch (Home's form)

- [ ] Add a repo with **Choose…** (the folder picker) or by pasting its path; it appears
      under Projects. A missing path or a file is refused there.
- [ ] Write a goal in Home's form: the branch chip appears as `mirante/<words-of-the-goal>`;
      pick the agent (Claude) and the repository with the chips; ⌘↵ dispatches. The worktree
      is created, the card appears at once under Working. The same goal again gets `-2`.
- [ ] **edit** the branch to an existing one: it is checked out as it is.
- [ ] A new folder: the trust card appears; **Trust folder** continues, **Decline** ends the
      session (try both, on two tasks).
- [ ] Agent **Codex**: the card shows the **auto-edits** label.
- [ ] The one-line form in the same box: `@claude <goal>` goes to the toolbar's repository on
      a branch Mirante names (the hint shows `→ <repo> · ⎇ mirante/…`); `@claude <project>
      <goal>` and `@claude <project>/<branch> <goal>` pick the project (and branch). Typing `@`
      suggests the agents, then the Projects. An unknown `a/b` word stays in the goal.
- [ ] **+ context** attaches a repo file and a memory note; the agent's first message lists
      them under "Context references".
- [ ] With two tasks waiting on you, **⌘J** opens the next one (Claude's on its Terminal tab)
      and says where you are ("1 of 2"); Esc goes back to Home with the keyboard on that card.

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
      "hibernate suggested" notice appear in the panel (one muted line); once pressure is
      normal the suggestion goes, and after 10 minutes only **History** keeps the notices.

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

## J. Performance

What changed in the terminal path and the views (2026-11-08), measured in tests; the by-hand
checks below are the part only a real window can show.

| What | Before | After | Where it is measured |
|---|---|---|---|
| Channel messages for a 1 MiB burst (daemon writes 8 KiB at a time) | 128, each base64 in JSON (eval + fetch round trip each), decoded with `atob` | 16 raw `ArrayBuffer`s (batches up to 64 KiB) | `mirante-bridge` `a_burst_reaches_the_frontend_in_few_large_messages` |
| Keystroke echo through the bridge (p50, fake daemon, release) | 7 µs | 8–18 µs (no coalescing wait for small reads) | `small_output_is_not_held_back_for_coalescing` |
| Output handed to a webview that acks nothing | unbounded (every read sent on) | exactly 256 KiB, then the daemon is held back and skips to a repaint; typing still arrives | `without_acks_reading_stops_and_typed_keys_still_get_through`, daemon `a_flood_toward_a_stalled_client_…` |
| `session.resize` (a full agent redraw) per link hover | 2 (hint pushed the terminal one line shorter, then back) | 0 (hint floats over it) | `TerminalTab` "hovering a link sends no session.resize" |
| Terminal tab switch away and back | new xterm, new attach, an `attached` event in the log, repaint without history | nothing (the terminal stays mounted and attached) | `Detail` "keeps one terminal per task across tab switches" |
| Coming back to a task's terminal after leaving the detail | screen only | screen plus up to 1000 rows of scrollback | daemon `attach_replays_the_scrollback_before_the_screen` |
| Card renders for a step event on one of 5 tasks (Home / Grid) | 5 / 5 | 1 / 1 | `renders.test.tsx` |
| Card renders for a `task.get` that changed nothing | 5 | 0 | `renders.test.tsx` |
| Detail renders for an event on another task | 1 | 0 | `renders.test.tsx` |

By hand, with a real Claude session:

- [ ] `cat` a large file (or `yes | head -200000`) in the terminal: the window stays
      responsive, typing during the flood is echoed, and the screen ends right (a repaint may
      skip some of the scroll).
- [ ] Hover links in the terminal: the hint shows at the bottom left and Claude does not
      redraw.
- [ ] A permission card arriving while on the Terminal tab floats at the top right; the
      terminal keeps its size.
- [ ] Switch Terminal → Steps → Terminal: the same screen and scrollback, instantly.
- [ ] Leave the detail and come back: the terminal shows its scrollback (scroll up).
- [ ] Scrolling a long scrollback is smooth (the WebGL renderer; in a `tauri dev` build the
      Web Inspector shows a `canvas` inside `.xterm-screen`).

## Log

| Date | Section | Result | Notes |
|---|---|---|---|
