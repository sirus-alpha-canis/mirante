# Side pane surfaces dogfood checklist (Batch A: tabs, "+", Terminal, File)

Run against the real daemon and the cockpit, on a throwaway repo first. Tick each line on
the day you try it; write what went wrong next to it. Plan and as-built notes:
`docs/plans/2026-11-27-side-pane-surfaces.md`.

Setup: as in `cockpit.md` (`scripts/install.sh --app`). The CLI, the daemon and the app must
be the same build (the daemon must know `shell.*`). For experiments,
`export MIRANTE_HOME=/tmp/mt`. Have a task with a worktree and a PDF committed in it.

## A. Tabs

- [ ] Open the side pane (⌘\) on a task that had a pane before this build: what it showed
      comes back as one tab.
- [ ] Review layout, then a path in Steps: the Diff tab is reused for the file (no new tab);
      **+ → File…** adds a tab instead. The same file again focuses its tab.
- [ ] ←/→ move between tabs; × closes one and its neighbour shows.
- [ ] **Split** with two tabs: both show; drag the line between them (and ←/→ on it).
      **Unsplit** goes back.
- [ ] Switch to another task and back, and quit and reopen the app: tabs, the shown tab and
      the split are as you left them.

## B. Terminal (your own shell)

- [ ] **+ → Terminal**: a shell opens, `pwd` is the task's worktree, your prompt and aliases
      load (login shell). The agent's Terminal tab is untouched.
- [ ] `env | grep -i mirante` prints nothing; `echo $MIRANTE_TASK` is empty.
- [ ] Run a long command, switch tabs and back: it kept running, the screen is whole.
- [ ] Resize the pane: `tput cols` follows.
- [ ] Four shells: **+ → Terminal** says "all 4 open" and is disabled.
- [ ] × on a shell's tab: the shell and anything it started end (check `ps`).
- [ ] `exit` in the shell: the tab says **Shell ended** with **Restart**; Restart gives a new
      shell in the worktree.
- [ ] Restart the daemon (Settings or `mirante daemon stop` then start): each shell tab says
      Shell ended; nothing came back by itself.
- [ ] Archive the task with a shell open: the shell ends before the worktree goes.
- [ ] Known limit: `mirante task list` from that shell is refused as an agent (worktree).
      From a shell `cd`-ed out of the worktree it works.
- [ ] Nothing was ever typed into the shell by Mirante (no auto-run, no pasted paths).

## C. File

- [ ] **+ → File…** on the PDF: "PDF, N pages · size"; **Open in Preview** opens that file in
      Preview. Nothing else opens this way.
- [ ] An `.svg` shows as text, never as a picture.
- [ ] An image, a Markdown file and a large file behave as before.
