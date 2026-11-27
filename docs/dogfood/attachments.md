# Attachments dogfood checklist

Run against the real daemon, the cockpit and real agents (Claude Code and Codex), on a
throwaway repo first. Tick each line on the day you try it; write what went wrong next to it.
Plan and as-built notes: `docs/plans/2026-11-27-attachments.md`.

Setup: as in `cockpit.md` (`scripts/install.sh --app`). The CLI, the daemon, `mirante-hook`
and `mirante-mcp` must be the same build. For experiments, `export MIRANTE_HOME=/tmp/mt`
(short: the socket path has a length limit).

Have ready: a screenshot on the clipboard (⇧⌘⌃4), a `.har` exported from a browser's
network tab after logging in to a test site (it carries cookies and an `Authorization`
header), a small `.log` and a PDF.

## A. Composer

- [ ] **Paste a screenshot** (⌘V) in a live Claude task's message composer: a chip with a
      thumbnail, the name (`pasted-image.png`) and the size appears; a spinner while it
      uploads. Send with no text: the agent says it sees the image (or reads it by path).
      The chip clears after the send.
- [ ] **Drop the `.har`** from Finder onto the composer: the composer is outlined while you
      hover; the chip says **"N secrets removed"** (N > 0). Open the stored file (its path is
      the chip's tooltip): cookie and auth header values read `[redacted by Mirante]`; the
      original was never copied into `~/.mirante`.
- [ ] **Pick** the `.log` and the PDF with the paperclip (several at once): two chips.
- [ ] **Remove a chip** with ×: it goes, and the agent never hears of that file when you send.
- [ ] A text paste over 32 KB becomes a `pasted-text.txt` chip instead of filling the box.

## B. Goal

- [ ] On Home, paste a screenshot into the goal field and dispatch a **goal with an
      attachment** (text is still needed: the branch is named after it). The new task's
      first prompt carries the `[Attached image …]` line and the agent uses the image.
- [ ] A fan-out goal (two agents): each child gets its own copy.

## C. Terminal

- [ ] In a **Claude session's Terminal tab, ⌘V an image**: the stored path is typed at the
      prompt as one quoted word, and Claude shows it as `[Image #1]`. Nothing runs until you
      press Enter.
- [ ] Drop a file onto the terminal: the terminal (not the composer around it) is outlined,
      and the stored path is typed in.
- [ ] A plain text paste in the terminal still pastes text, as before.

## D. Codex

- [ ] In a **Codex** task, send a message with a screenshot attached: Codex describes the
      image (it got a `localImage` input, not only the path line).

## E. Daemon restart

- [ ] Attach a file in a composer, do not send. Restart the daemon
      (`mirante daemon restart`). The chips clear and the note says
      **"The daemon restarted: attach the files again"**; the text stays in the box.
      Attach again and send: it goes through.

## For the founder

Everything above needs a real interactive session; nothing here was run by an agent.
