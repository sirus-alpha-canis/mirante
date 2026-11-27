# Side pane surfaces: "+" opens Terminal, File, Browser, Device

> **For agentic workers:** use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkboxes; TDD (failing test → run → implement → run → commit).

**Goal:** a "+" button in a task's side pane opens tabs (and optionally one split) of four surfaces — Terminal, File, Browser, Device (iOS Simulator) — remembered per task.

**Why:** founder request (2026-11-27, dogfooding). T3 Code and cmux let you keep a shell, a file, a dev-server preview and a simulator next to the agent. We studied their behaviour only (clean room; no code or design copied). Founder decisions: the simulator is **interactive via serve-sim** (Apache-2.0); agents get **no** simulator access in v1, a screenshot-only tool later.

## Founder decisions on open questions (defaults taken 2026-11-27; change any of them)
1. PDF in v1: metadata + "Open in Preview" (no pdf.js yet).
2. The person's shell starts in the worktree; `cd` is not restricted (it's the person's own shell).
3. serve-sim is installed into `~/.mirante/tools` only on an explicit click, after showing the exact command; a pre-installed serve-sim is detected too.
4. A hidden device tab pauses its stream.
5. Browser allows `localhost`, `127.0.0.1` and `[::1]`.
6. Up to 4 shells per task.
7. Known limit kept on purpose: the `mirante` CLI run from a person's shell inside a worktree is classified as an agent (worktrees are agent-written). Documented in the tab's help text.

## What exists (read first)
- `apps/cockpit/src/components/SidePane.tsx` shows one `PaneTarget` with Swap and Close. `lib/sidepane.ts` stores `PaneState {open,width,target,swapped}` in `mirante.pane.<task>` (LRU 200) and validates every field it reads back.
- Files (Batch A of `2026-11-12-threads-fanout-side-panels.md`): `task.files` / `task.file` (Person-only, dirfd walk, secrets hidden), `FileView` (highlighter, Markdown, png/jpeg/gif/webp), `QuickOpen` (⌘P).
- PTY: `crates/mirante-daemon/src/driver/pty.rs` (`PtyProcess::spawn`, setsid, `kill_tree`, mirror/fanout). Stream path: `server.rs::attach` → bridge `attach.rs` (batching, acks, re-attach) → Tauri `attach_open/write/ack/close`.
- `Hub::identify` (`hub/dna.rs`) classifies a caller whose cwd is under the worktrees root as `Agent{task:None}`. Kept on purpose.

## Design

### Pane model (cockpit)
- `PaneState` gains `tabs: PaneTab[]` (≤ 8), `active: string`, `split: {dir:"row"|"col", second:string, ratio:number} | null`. `PaneTab = {id, t: PaneTarget}`.
- `PaneTarget` gains `{k:"shell", n}`, `{k:"browser", url}`, `{k:"device", udid}`.
- `loadPane` migrates the old single `target` into one tab; each tab passes `checkTarget` or is dropped.
- "+" menu: Terminal, File… (QuickOpen into a new tab), Browser, Device. Keyboard reachable; tabs close with ×; Swap and Close keep working. Vibely tokens only; all strings in i18n (en, pt).

### 1. Terminal (a person's shell, separate from the agent's PTY)
- Daemon `hub/shells.rs`: a per-task registry of `PtyProcess`, separate from `live` (so it never enters `task_of_tree` / `task_of_tty`).
- Methods (all Person): `shell.open {task, repo?, rows, cols}` → `{shell}`; `shell.attach` (stream, reuses `server::attach` framing); `shell.resize`, `shell.close`, `shell.list`.
- Spawn: `$SHELL` only if listed in `/etc/shells`, else `/bin/zsh`, with `-l`; cwd = task worktree (workspace task: chosen repo); env strips `AGENT_ENV`, `TASK_ENV`, every `MIRANTE_*`, hook/MCP tokens; `TERM=xterm-256color`. Max 4 per task.
- Lifecycle: `kill_tree` on `archive_one`, daemon shutdown and `shell.close`. Not resumed after a daemon restart ("Shell ended" + Restart). memguard reports `kind: shell`, never hibernates it.
- Security: Person-only in `ACCESS`; `shell.attach` reached only through Tauri commands (or `attach_open` with a `kind`), pinned in `security.test.ts`. `TerminalTab` reused with a `source` prop. Nothing ever auto-types into it.

### 2. File
- Reuse `FileView` + `QuickOpen`; one tab per file; opening an already-open file focuses its tab.
- PDF: `worktree_file::Kind::Pdf` by magic `%PDF-`, cap 20 MB; v1 shows size + page count + "Open in Preview" through a pinned Tauri command that opens only the daemon-resolved worktree path.
- SVG shown as text, never rendered.

### 3. Browser (localhost preview)
- `<iframe>`, not a Tauri child webview (needs `unstable`, native overlay z-order/focus/resize problems). Child webview is the fallback if spike S2 fails.
- CSP: add `frame-src http://localhost:* http://127.0.0.1:* http://[::1]:*` to `csp` and `devCsp`. Nothing else changes; `frame-src` also governs in-frame navigation, so no internet origin loads.
- iframe: `sandbox="allow-scripts allow-forms allow-same-origin"` (the dev server's own origin), no `allow-top-navigation`/`allow-popups`/`allow-modals`; `referrerpolicy="no-referrer"`; `allow=""`.
- Address bar: `URL`-parsed; `http:` only; host localhost / 127.0.0.1 / [::1]; explicit port 1–65535; no userinfo. Reload, Back, "Open in browser" (existing opener allowlist).
- Framing refused (X-Frame-Options / frame-ancestors): "This server can't be shown here" + "Open in browser".
- Optional: suggest ports seen as `http://localhost:NNNN` in shell/agent output.

### 4. Device (iOS Simulator via serve-sim)
- serve-sim (verified from its GitHub README 2026-11-27): Apache-2.0, Apple Silicon only; needs Node 20+, Xcode CLT, a booted simulator (it doesn't boot). Bundled Swift helper serves `/helper/<udid>/stream` (MJPEG) and `/helper/<udid>/control` (WebSocket input). Flags: `--no-preview`, `-p`, `-d <udid>`, `--auth-token`, `--detach`, `--allow-exec` (off by default). Loopback may be accepted without a token — spike S1 checks.
- Daemon `hub/devices.rs` runs `node <~/.mirante/tools/serve-sim@PINNED>/…/cli.js --no-preview -p <random free port> -d <udid> --auth-token <32 random bytes>`. Never `--detach`, never `--allow-exec`. Env cleared to HOME + minimal PATH (node's dir); `TMPDIR=~/.mirante/sim/tmp` (SafeDir, 0700). No shell. After start, check the port answers on 127.0.0.1 and not on the LAN address.
- simctl via the hardened runner: `xcrun simctl list devices available --json`, `list runtimes --json`, `boot`, `shutdown`; no shell, timeout, capped stdout, udid matched against a UUID pattern and the list.
- Methods (all Person): `device.setup` → `{arm64, xcode, ios_runtime, node{found,version}, serve_sim{installed,version}}`; `device.list`; `device.boot`; `device.shutdown` (cockpit confirms); `device.tool_install` (explicit click; shows `npm install --ignore-scripts --no-audit --no-fund serve-sim@<pinned>` with `--prefix ~/.mirante/tools`; then checks version + pinned integrity hash); `device.stream_start` / `device.stream_stop` (bridge-internal).
- Frames: the bridge proxies. Tauri `device_stream_open(udid)` calls `device.stream_start` from Rust; `{port, token}` stays in the bridge and never reaches JS (`device.stream_start` is absent from `ALLOWED`, asserted by a test). The bridge parses MJPEG (bearer header, multipart, frame ≤ 4 MB), keeps only the latest frame, sends JPEG bytes over a Tauri `Channel` with the `attach.rs` ack model, ≤ 30 fps, paused while the tab is hidden. JS draws with `createImageBitmap(new Blob([bytes]))` on a `<canvas>`; `img-src` unchanged.
- Input: `device_input(id, ev)` with a closed enum validated in the bridge: `touch{phase,x,y}` (0..1), `key{code}`, `text` (≤ 1 KB), `button{home|lock|volume_up|volume_down}`, `rotate` → control WebSocket (`tungstenite`; check `deny.toml`).
- Pane: setup checklist (Xcode / iOS runtime / Node missing, Install serve-sim), device picker grouped by runtime, Refresh, Boot, Shutdown, live canvas.
- Lifecycle: one serve-sim per device, shared and ref-counted by open streams, stopped 60 s after the last viewer; killed on daemon shutdown; a task's streams released on archive; the device is never shut down without a click. memguard reports `kind: tool` with "Stop device stream". `device.setup` notes a booted device costs ~1–2 GB.
- Residual risk: another local process could reach the helper port; with `--auth-token` it needs the token. Agents can already run `xcrun simctl`, so no new privilege.

## Batches
### Batch 0 — spikes (`docs/spikes/FINDINGS-surfaces.md`)
- [ ] S1: serve-sim `--auth-token` covers loopback + helper port; exact control-WebSocket schema for the pinned version; `--ignore-scripts` still leaves a runnable helper.
- [ ] S2: WKWebView iframe at `http://localhost` from `tauri://localhost`; `window.__TAURI_INTERNALS__` undefined inside; `invoke` from that origin refused; in-frame link to `https://example.com` blocked by `frame-src`.
- [ ] S3: MJPEG bandwidth/CPU at 30 fps through a Tauri Channel.

### Batch A — "+" menu, tabs, Terminal, File
- [ ] 1. `lib/sidepane.ts` tabs/split/migration (tests: migrates single target, drops bad tabs, caps at 8, split ratio clamps).
- [ ] 2. `SidePane.tsx` + new `PaneTabs.tsx`: tab strip, "+" menu, split (tests: + opens each surface, tabs persist per task, Escape closes menu).
- [ ] 3. Daemon `hub/shells.rs`, `mirante-ipc` types, `ACCESS` (tests: `shell_env_has_no_agent_markers`, `shell_is_not_a_live_session`, `archive_kills_shells`, `fifth_shell_refused`, `$SHELL not in /etc/shells falls back`, `shells_are_a_persons_only`).
- [ ] 4. `shell.attach` server stream; bridge `attach.rs` `Target::{Task,Shell}`; Tauri commands pinned in `security.test.ts`.
- [ ] 5. `TerminalTab` `source` prop + "Shell ended / Restart".
- [ ] 6. File tabs + PDF `Kind` (`pdf_detected_by_magic_not_extension`); pinned `open_path_external` with a daemon-resolved path.
- [ ] 7. i18n `pane.add.*`, `shell.*` (en, pt).

### Batch B — Browser
- [ ] 1. CSP `frame-src`; `security.test.ts` asserts the exact `frame-src` and that no capability has `remote`.
- [ ] 2. `BrowserTab.tsx` + `lib/localurl.ts` (tests: refuses https, other hosts, userinfo, missing port, file:; exact sandbox flags).
- [ ] 3. Framing-refused fallback; optional port suggestions.

### Batch C — Device (serve-sim)
- [ ] 1. `hub/devices.rs` + simctl JSON parsing (fixtures in `hub/testdata`, fake `xcrun` via env override; tests: `udid_must_be_listed`, `setup_reports_missing_node_xcode_runtime`, `non_arm64_reports_unsupported`).
- [ ] 2. serve-sim supervisor (tests: `never_passes_allow_exec_or_detach`, `random_port_and_token_per_start`, `refcount_stops_after_grace`, `daemon_shutdown_kills_tree`, `env_is_cleared`).
- [ ] 3. `device.tool_install` (tests: `pinned_version_and_ignore_scripts`, `refused_without_person`).
- [ ] 4. Bridge `device.rs`: MJPEG parser with caps (`oversize_frame_drops_stream`, `latest_frame_wins`), input enum (`text_over_1k_refused`, `coords_outside_0_1_refused`); Tauri `device_stream_open/close/input` pinned; `device.stream_start` absent from `ALLOWED`.
- [ ] 5. `DeviceTab.tsx`: checklist, picker, canvas, pointer + keyboard forwarding with focus capture.
- [ ] 6. memguard `kind: tool` reporting.
- [ ] 7. `THIRD_PARTY_NOTICES.md`: serve-sim (Apache-2.0, run as a separate program, not bundled).

### Batch D — later, not v1
- Agent `device_screenshot` MCP tool: own task only, read-only, no input, each screenshot shown to the person in Steps, rate-limited.

## Out of scope (v1)
Android; agent input to the simulator; file editing; browser outside localhost; devtools; shells surviving a daemon restart; `simctl create`; Intel Macs.

Sources: https://github.com/EvanBacon/serve-sim · its package README.
