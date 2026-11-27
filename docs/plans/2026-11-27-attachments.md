# Attachments: paste, drop and pick images and files

> **For agentic workers:** use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkboxes.

**Goal:** a person can paste, drop or pick images (screenshots) and files (.har, logs, PDFs, text) into the message composer, the goal input and the terminal. The agent receives them as files it can read.

**Why:** dogfooding (2026-11-27). "precisa permitir colar image, .har etc." T3 Code and cmux both do it. We studied their behaviour only (clean room: T3 Code is MIT, cmux is GPL-3.0). No code or design was copied.

**What they do, and what we take:**
- Both save every attachment to disk and give the agent a path.
  - T3 Code also sends images as native API image blocks.
  - cmux pastes the escaped path into the PTY, so the CLI's own image detection makes `[Image #1]`.
- Neither redacts .har files, which carry cookies and auth headers. We redact by default. That sets us apart and matches our security posture.

## Design

### Storage (daemon)
- Attachments live in `~/.mirante/sessions/<task>/attachments/`.
  - Written through `SafeDir` (`create`/`ensure_dir`, `write_new`) with mode 0600.
  - Names are sanitised: basename only, `[A-Za-z0-9._-]`, at most 80 characters, prefixed with `<n>-` so names never collide.
- Limits:
  - An image (png/jpeg/gif/webp, detected by magic bytes, not by extension) may be 10 MB.
  - Any other file may be 25 MB.
  - A task may hold at most 20 attachments per message and 200 MB in total.
- The session dir is removed with the task, as today. Check that archive/retire cleanup covers it.

### New RPCs (Person-only; add to `access.rs`, the bridge `ALLOWED` and `apps/cockpit/src/security.test.ts`)
- `attachment.put { task, name, b64 }`: bytes from the clipboard (image paste, or a long text paste).
  - Returns `{ id, path, name, kind: "image"|"file", bytes, redacted: u32 }`.
- `attachment.import { task, path }`: a file dropped from Finder or picked in the dialog. The daemon copies it.
  - It opens the source with `O_NOFOLLOW` on the final component, and it must be a regular file.
  - It reads at most limit+1 bytes.
  - It refuses anything inside `~/.mirante` and anything that isn't a regular file.
  - It returns the same shape as `attachment.put`.
- `attachment.remove { task, id }`: deletes one attachment that has not been sent yet.
- Agents never get these methods. A classification test enforces it.

### Redaction
Applies to `.har`, and to any JSON file that parses as HAR (it has `log.entries`).
- Request and response header values are replaced with `[redacted by Mirante]`. Names matched case-insensitively:
  - `Cookie`, `Set-Cookie`, `Authorization`, `Proxy-Authorization`, `X-Api-Key`, `X-Auth-Token`, `X-Csrf-Token`;
  - any name containing `token`, `secret`, `session` or `auth`.
- `cookies[].value` is replaced the same way.
- Query string values whose name matches the same patterns are replaced.
- Every remaining string passes through `mirante_core::redact::redact_counted`, which covers tokens in bodies and URLs.
- The redacted file is what is stored; the original never lands in `~/.mirante`. The count is returned as `redacted` and the chip shows "N secrets removed".
- Other text files (`.log`, `.txt`, `.json`, `.env`-like) also pass through `redact_counted`. Binary files and images are stored as they are.
- Building a test HAR needs care: build token-shaped literals at runtime (GitHub push protection).

### Delivery
- `SendMessage` and `StartSession` gain `attachments: Vec<String>` (ids, `#[serde(default)]`). The hub resolves each id to an attachment of that task and rejects unknown ids.
- The hub appends to the text one line per attachment, labelled as operator-supplied:
  - `[Attached image "<name>": <abs path>]`
  - `[Attached file "<name>" (<size>, N secrets removed): <abs path>]`
- `MAX_MESSAGE_BYTES` and `MAX_GOAL` apply to the person's own text only. The appended lines are added after the check.
  - An empty text with at least one attachment is allowed.
- Claude (PTY): the text with the path lines goes through the existing bracketed paste.
  - Claude Code detects image paths in pasted text. Even when it doesn't, its Read tool opens images and files by path.
  - Claude may need permission to read outside the worktree. Add the attachments dir to the session's allowed read directories (`--add-dir` or settings `additionalDirectories`), scoped to that one task's attachments dir only, never the whole `~/.mirante`.
- Codex (app-server):
  - The text item carries the path lines.
  - Each image also adds `{ "type": "localImage", "path": <abs> }` to `input` in both `turn/start` and `turn/steer`.
  - Check the item name against the app-server schema the driver already targets. If `localImage` is not accepted, fall back to path lines only.
  - Ensure the sandbox can read the dir; add it as a readable root if the policy needs that.

### Cockpit
- An `AttachmentTray` component and a `useAttachments(taskId)` hook are shared by `MessageComposer`, `DispatchComposer` (goal) and `GroupPanel`. For a goal before the task exists, keep the bytes and paths client-side, then upload after `task.create` and before `session.start`.
- Inputs:
  - **Paste in the textarea:** `clipboardData.files` gives images and files; send them with `attachment.put`. A text paste over 32 KB becomes a "pasted-text.txt" attachment instead of going inline.
  - **Drop:** Tauri `getCurrentWebview().onDragDropEvent` gives absolute paths. Route the drop to the composer that is visible or focused, and highlight the drop zone while hovering. Send the paths with `attachment.import`.
  - **Pick:** a paperclip button opens `@tauri-apps/plugin-dialog` `open({ multiple: true })` and sends the paths with `attachment.import`.
- The tray shows a chip per file:
  - a thumbnail for images (`data:` URL; CSP already allows `img-src data:`);
  - the name, size and "N secrets removed";
  - a × to remove (`attachment.remove`);
  - a spinner while uploading, and an error toast on rejection.
- Send is enabled when there is text or at least one attachment. After a successful send the tray clears.
- TerminalTab (cmux-style):
  - On a paste event with image or file data, upload it, then write the bracketed-paste-safe absolute path into the PTY with `attach_write`.
  - A plain text paste keeps today's path.
  - Dropping onto the terminal does the same.
- i18n: add en and pt keys for every new string.

### Out of scope (v1)
- Native API image or document blocks for Claude. Our Claude driver is a PTY, so it would need the stream driver.
- PDF text extraction.
- Image compression.
- An option to skip HAR redaction.

## Tasks
- [x] 1. Daemon: attachment store + redaction module + RPCs + access/bridge/security-test classification; unit tests (limits, sanitising, O_NOFOLLOW/regular-file checks, refusal of ~/.mirante paths, HAR redaction with runtime-built tokens).
  - **As built:** `crates/mirante-daemon/src/attachments/` (store: sanitise, magic-byte kinds, limits, strict base64, `read_source`, `store`/`unlink` through `SafeDir`; `redact.rs`: HAR + text) and `hub/attachments.rs` (in-memory registry by task, the three RPCs, `carry_attachments`).
    - Wire shapes: `attachment.put {task, name, b64}` and `attachment.import {task, path}` → `{id, path, name, kind: "image"|"file", bytes, redacted}`; `attachment.remove {task, id}` → `{}`.
    - `id` is a fresh UUID (v7), not the stored name. The registry is in memory: after a daemon restart an id is unknown (refused at send; the client uploads again) and the files stay until the task is archived (archive already removes the whole session dir, checked by a test).
    - Import refuses a relative path, a symlink (`O_NOFOLLOW`), a non-regular file (FIFO included: `O_NONBLOCK`), a file with other hard links (it could alias a file in `~/.mirante`), and any file whose opened descriptor's path (`F_GETPATH`; `/proc/self/fd` on Linux) or given path is inside the home, canonical or not.
    - Text = UTF-8 without NUL and not `%PDF-`/zip; it goes through `redact_counted`. HAR also masks `postData.params` and credential parameters inside `request.url`. Stored paths are canonical (`/private/tmp/...` on macOS).
    - Deviation: `attachment.put` travels in one JSON line, and the socket's `MAX_LINE` is 16 MB, so a pasted blob is at most about 12 MB decoded (images, at 10 MB, always fit). Bigger files go through `attachment.import`. `MAX_LINE` was left alone.
- [x] 2. Delivery: `attachments` on SendMessage/StartSession, path lines, Claude read access to the dir, Codex `localImage` items; driver tests (fake + codex input-shape tests).
  - **As built:** `SendMessage.attachments` / `StartSession.attachments` (`Vec<String>` of ids; `#[serde(default)]`, left out of the JSON when empty, so older callers are unchanged).
    - The hub resolves every id against that task's registry before anything is sent: an unknown id, another task's, one listed twice or more than 20 refuse the whole message. Ids are marked sent (no longer removable) only once the message was steered or queued, or the session started.
    - The person's own text is size-checked alone (`check_with_attachments`: empty allowed). Then the lines are appended after a blank line. A file line reads `[Attached file "<name>" (<size>): <path>]`, and gets `, N secret(s) removed` only when N > 0. Only a person attaches (`send_by` refuses any other sender).
    - Claude: the settings' `permissions.allow` gets `Read(/<canonical attachments dir>/**)` (the folder is made at every launch, so a file attached mid-session is covered). That is the same mechanism as a workspace's read-only repos. It grants read only, for that one task's folder; it is not `--add-dir` or `additionalDirectories`, which would make the folder a working directory.
    - Codex: `localImage` was checked against the installed app-server binary's `UserInput` variants (`text`, `image`, `localImage{path}`, `localAudio`, `skill`, `mention`), so no fallback was needed. The driver gets the task's attachments folder (`set_attachments_dir`). Each message's `[Attached image "…": <path>]` lines whose file is directly in that folder (a regular file now) add `{"type":"localImage","path":…}` after the text item, in both `turn/start` and `turn/steer` (`steer_params`). A look-alike line naming any other path adds nothing. The `workspace-write` sandbox already reads anywhere, and app-server itself reads the image, so no readable root was added.
- [x] 3. Cockpit: useAttachments + AttachmentTray in the three composers; paste/drop/pick; i18n; vitest.
  - **As built:** `src/lib/attachments.ts` (wire shapes, base64, `shellQuote`, sizes, `data:` thumbnails for png/jpeg/gif/webp only, never SVG; `isStaleAttachment`), `src/lib/drop.ts` (the drop router) and `src/components/attachments.tsx` (`useAttachments`, `uploadHeld`, `useDropZone`, `AttachmentTray`, `AttachButton`).
    - `MessageComposer` and a group child's steer box upload at once (`attachment.put` for a paste, `attachment.import` for a drop or the picker); `DispatchComposer` holds the files (no task yet), then after `task.create` uploads them one by one to the new task and passes the ids to `session.start` (each fan-out child gets its own copy). A file the daemon refuses there is named ("Not attached to <branch>: …"); the session still starts with the rest. A goal still needs text (the branch is named after it).
    - Held goal files live in the composer's state: leaving Home drops them (the goal text is kept). Fine for v1.
    - Send is enabled with text or at least one ready attachment, never while one is uploading; the sent chips clear after a successful send (chips added meanwhile stay). `attachments` is only in the params when there are some (older call shapes unchanged).
    - A pasted blob over 12 MB is refused in the cockpit with "drop it or pick it" (the request-line cap, see Task 1). More than 20 per message: the extra ones are refused with a note.
    - Daemon restart: when the boot id changes, uploaded chips are cleared and the note says "The daemon restarted: attach the files again"; a send refused with "unknown attachment" does the same (the text stays in the box).
    - Drops: `getCurrentWebview().onDragDropEvent` (main.tsx, `Env.onDragDrop`) feeds one router (App). It picks the innermost registered target under the cursor (physical pixels ÷ `devicePixelRatio`, `elementFromPoint`), else the one holding the focus, else the last focused, else the only one mounted; it outlines that target while hovering (`.drop-on`).
    - Deviation (capabilities): listening to the drag-drop event needs `core:event:allow-listen` and `core:event:allow-unlisten`. They were added and pinned in `security.test.ts`, which also checks that the only listen is the webview's drag-drop and that the shell emits no events of its own. The picker for files is a second `open({ directory: false, multiple: true })`; the test pins both calls. CSP unchanged.
- [x] 4. TerminalTab paste/drop of images/files → path into PTY; vitest.
  - **As built:** `TerminalTab` listens for `paste` on the terminal host in the capture phase, ahead of xterm. A paste with files is taken (`preventDefault` + `stopImmediatePropagation`); each file goes up with `attachment.put` in order, then the stored paths are typed in through the existing input queue (`send`, so `MAX_PENDING_INPUT` applies). A plain text paste is still xterm's.
    - `pathPaste` (`lib/attachments.ts`): each path `shellQuote`d after its C0/C1 control characters are stripped (a name cannot close the paste or press Enter), space-separated, wrapped in `ESC[200~ … ESC[201~` only when `term.modes.bracketedPasteMode` is on.
    - A file over 12 MB shows the existing `att.tooBig` note and is skipped; a refused one shows `att.failed`; the others still go. A read-only stream takes the paste (nothing is typed as text) and uploads nothing.
    - Drop: the terminal wrap is a drop target (`useDropZone`, outlined with the existing `.terminal-wrap.drop-on`), so the router's innermost-target rule sends a drop on the terminal there rather than to the composer around it. Each path goes through `attachment.import`, then the stored path is typed in the same way.
    - These attachments are never "sent" in a message, so they stay in the daemon's registry until the task is archived (the files go with the session dir).
- [x] 5. Docs: README feature line, CHANGELOG Unreleased, docs/dogfood/attachments.md checklist for the founder.
  - **As built:** README "Use → The app" has an **Attachments** line; CHANGELOG Unreleased has an **Added** entry; `docs/dogfood/attachments.md` lists the founder's checks (composer paste, `.har` drop and "N secrets removed", picker, remove, goal with an attachment, terminal ⌘V in a Claude session and a drop, Codex image, daemon-restart re-attach note). None of it was run by an agent.
