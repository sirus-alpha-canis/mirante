# Side pane surfaces dogfood checklist (Batch A: tabs, "+", Terminal, File; Batch B: Browser)

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

## D. Browser (Batch B; also spike S2, which needs a real WKWebView)

Start a throwaway dev server: an empty folder with an `index.html` holding a link to
`https://example.com` and `<script>document.body.dataset.t = typeof window.__TAURI_INTERNALS__</script>`,
then `python3 -m http.server 8765` in it. Try the release build, then `pnpm tauri dev`.

- [ ] **S2 loads.** **+ → Browser** → `localhost:8765`: the page shows in the tab (also
      `127.0.0.1:8765`). If WebKit blocks an `http://` frame inside `tauri://localhost` as
      mixed content, S2 fails: the fallback is a child webview.
- [ ] **S2 no Tauri object inside.** Web Inspector (dev build), the frame's console context:
      `typeof window.__TAURI_INTERNALS__` and `typeof window.isTauri` are `"undefined"`.
- [ ] **S2 invoke refused from that origin.** In the frame's console,
      `window.webkit.messageHandlers.ipc.postMessage(JSON.stringify({cmd:"home_dir",callback:0,error:1,payload:{}}))`
      gets no answer, and `fetch("ipc://localhost/home_dir", {method:"POST", body:"{}"})` (and
      `http://ipc.localhost/home_dir`) fails or is refused.
- [ ] **S2 internet blocked in the frame.** Click the `https://example.com` link in the framed
      page: it does not load there (console: a `frame-src` refusal) and no window opens.
- [ ] Refused addresses say why and load nothing: `https://localhost:8765`,
      `example.com:8765`, `localhost` (no port), `user:pw@localhost:8765`, `file:///etc/hosts`,
      `[::1]:8765` (CSP cannot name IPv6), and in a dev build `127.0.0.1:1420` (Mirante itself).
- [ ] A server sending `X-Frame-Options: DENY` (or `frame-ancestors 'none'`): "This server
      can't be shown here." with **Open in browser**. Note what shows when nothing listens on
      the port (blank or that message: not seen yet).
- [ ] **Back** returns to the previous address typed (links clicked inside the page are not
      in it); **Reload** reloads; **Open in browser** opens the page in your browser.
- [ ] Quit and reopen: the Browser tab comes back on its address; the tab is named
      `localhost:8765`.
- [ ] The framed page cannot open a popup, an alert, or navigate Mirante itself
      (`window.open`, `alert`, `top.location = …` from its console all do nothing).
