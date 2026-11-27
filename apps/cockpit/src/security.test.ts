// The desktop shell's attack surface, pinned: a CSP with no remote origins or eval (frames: only a
// dev server on localhost or 127.0.0.1), and
// capabilities limited to notifications, opening github.com and localhost pages, the native picker and event listening (Finder drops). Widening either
// must be a deliberate edit of this test.
import { describe, expect, it } from "vitest";
import capabilities from "../src-tauri/capabilities/default.json";
import conf from "../src-tauri/tauri.conf.json";
import shell from "../src-tauri/src/lib.rs?raw";
import scanner from "../src-tauri/src/scan.rs?raw";
import editorRs from "../src-tauri/src/editor.rs?raw";
import grantsRs from "../src-tauri/src/grants.rs?raw";
import picker from "./main.tsx?raw";
import css from "./styles.css?raw";
import page from "../index.html?raw";
import bridgeRpc from "../../../crates/mirante-bridge/src/rpc.rs?raw";
import browserTab from "./components/BrowserTab.tsx?raw";
import { M } from "./lib/protocol";

describe("Tauri security baseline", () => {
  const csp = conf.app.security.csp;
  const directive = (name: string) =>
    csp
      .split(";")
      .map((d) => d.trim())
      .find((d) => d.startsWith(`${name} `)) ?? "";

  it("loads code only from the app and talks only to the IPC bridge", () => {
    expect(directive("default-src")).toBe("default-src 'self'");
    expect(directive("script-src")).toBe("script-src 'self'");
    expect(directive("connect-src")).toBe("connect-src ipc: http://ipc.localhost");
    // Outside frame-src (below), no remote origin and no wildcard anywhere.
    const rest = csp
      .split(";")
      .map((d) => d.trim())
      .filter((d) => !d.startsWith("frame-src "))
      .join("; ");
    expect(rest).not.toMatch(/unsafe-eval|https?:\/\/(?!ipc\.localhost)|\*/);
    expect(directive("object-src")).toBe("object-src 'none'");
    // Fonts are the app's own files (bundled, never a font CDN).
    expect(directive("font-src")).toBe("font-src 'self'");
    expect(directive("style-src")).toBe("style-src 'self' 'unsafe-inline'");
  });

  it("fonts are the app's own files: no font CDN, no remote url() or @import anywhere", () => {
    for (const [name, text] of [["styles.css", css], ["index.html", page]] as const) {
      expect(text, name).not.toMatch(/@import/);
      expect(text, name).not.toMatch(/fonts\.googleapis|fonts\.gstatic|use\.typekit|fonts\.bunny|rsms\.me|jsdelivr|unpkg|cdnjs/);
      expect(text, name).not.toMatch(/<link[^>]+href=["']?(https?:)?\/\//);
      for (const [, url] of text.matchAll(/url\(\s*["']?([^"')]+)/g)) expect(url, `${name}: ${url}`).not.toMatch(/^(https?:|\/\/)/);
    }
    // Each @font-face loads a woff2 bundled next to the stylesheet (styles.test.ts checks they exist).
    const faces = [...css.matchAll(/@font-face \{([^}]*)\}/g)].map((m) => m[1]);
    expect(faces.length).toBeGreaterThan(0);
    for (const f of faces) expect(f).toMatch(/src: url\("\.\/assets\/fonts\/[\w-]+\.woff2"\) format\("woff2"\);/);
    // Every family named anywhere is a bundled one or a system fallback: Nunito Sans
    // (the UI and the splash) and JetBrains Mono; Inter is gone.
    const bundled = faces.map((f) => /font-family: "?([^";]+)"?;/.exec(f)?.[1]);
    expect(bundled).toEqual(["Nunito Sans", "JetBrains Mono", "JetBrains Mono"]);
    for (const [name, text] of [["styles.css", css], ["index.html", page]] as const) expect(text, name).not.toMatch(/\bInter\b/);
    expect(page).toContain('font-family: "Nunito Sans", -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;');
  });

  it("frames show only a dev server on this machine (the Browser tab); nothing else frames", () => {
    // frame-src also governs every navigation inside the frame, so a link to the internet
    // never loads there. [::1] is not in it: CSP host sources cannot name an IPv6 literal.
    expect(directive("frame-src")).toBe("frame-src http://localhost:* http://127.0.0.1:*");
    expect(csp.match(/frame-src/g)).toHaveLength(1);
    expect(csp).not.toMatch(/child-src|worker-src|navigate-to/);
    expect(conf.app.security.devCsp).toContain("; frame-src http://localhost:* http://127.0.0.1:*;");
  });

  it("no capability is granted to a remote origin, so a framed dev server gets no IPC", () => {
    // Tauri 2 answers an invoke from a page that is not the app (tauri://localhost, or the
    // devUrl in dev) as Origin::Remote; only a capability with `remote.urls` allows one. A
    // capability added from Rust would bypass this file: there is none.
    const files = import.meta.glob("../src-tauri/capabilities/*", { query: "?raw", import: "default", eager: true }) as Record<string, string>;
    expect(Object.keys(files)).toEqual(["../src-tauri/capabilities/default.json"]);
    for (const [name, text] of Object.entries(files)) {
      expect(JSON.parse(text), name).not.toHaveProperty("remote");
      expect(text, name).not.toMatch(/"remote"/);
    }
    expect(shell).not.toMatch(/add_capability|CapabilityBuilder|\.remote\(/);
    // The Browser tab's one frame: the dev server's own origin, scripts and forms; no top
    // navigation, popups or modals; no referrer; no feature (camera, mic…) delegated.
    expect(browserTab.match(/<iframe/g)).toHaveLength(1);
    expect(browserTab).toContain('sandbox="allow-scripts allow-forms allow-same-origin"');
    expect(browserTab).toContain('referrerPolicy="no-referrer"');
    expect(browserTab).toContain('allow=""');
    expect(browserTab).not.toMatch(/allow-top-navigation|allow-popups|allow-modals|allow-downloads|allow-storage-access|allow-pointer-lock/);
    // Only BrowserTab frames anything.
    const sources = import.meta.glob(["./**/*.tsx", "!./**/*.test.tsx"], { query: "?raw", import: "default", eager: true }) as Record<string, string>;
    expect(Object.keys(sources).filter((n) => /<(iframe|webview|embed|object)[\s>]/.test(sources[n]))).toEqual(["./components/BrowserTab.tsx"]);
  });

  it("the dev policy only adds what Vite's dev server needs", () => {
    const dev = conf.app.security.devCsp;
    expect(dev.replace("script-src 'self' 'unsafe-inline'", "script-src 'self'").replace(" ws://127.0.0.1:1420", "")).toBe(csp);
  });

  it("no global Tauri object, no remote content", () => {
    expect(conf.app.withGlobalTauri).toBe(false);
    expect(conf.build.devUrl).toBe("http://127.0.0.1:1420");
  });

  it("the main window is built in Rust (src-tauri/src/lib.rs), where new windows are denied", () => {
    // `create: false`: Tauri does not build it from config, so `window.open` and
    // target=_blank never get a webview of their own; links go through the opener plugin.
    expect(conf.app.windows.map((w) => [w.label, w.create])).toEqual([["main", false]]);
    expect(shell).toContain(".on_new_window(|_, _| NewWindowResponse::Deny)");
  });

  it("grants only notifications, opening github.com and localhost URLs, the native picker and event listening", () => {
    // Notifications: exactly the three commands the plugin's \`window.Notification\` shim
    // calls (is_permission_granted, request_permission, notify), and register_listener so a
    // click on a task's notification opens it (\`onAction\`); not the plugin's default set
    // (channels, pending/active lists, cancel, action types…).
    const ids = capabilities.permissions.map((p) => (typeof p === "string" ? p : p.identifier));
    expect(ids).toEqual([
      "notification:allow-is-permission-granted",
      "notification:allow-request-permission",
      "notification:allow-notify",
      "notification:allow-register-listener",
      "opener:allow-open-url",
      // The native "Choose…" folder picker (Add project): `open` only, no save/ask/message.
      "dialog:allow-open",
      // Finder drops (attachments): Tauri's drag-drop events reach the webview as events, so
      // it may listen (and stop). The shell emits no events of its own (checked below).
      "core:event:allow-listen",
      "core:event:allow-unlisten",
    ]);
    const opener = capabilities.permissions.find((p) => typeof p !== "string");
    // github.com pages, and a local dev server on an explicit port (1 to 5 digits, then the
    // path: a "/" right after the port, so nothing such as "localhost:1@evil.example" or
    // "localhost:1.evil.example" can name another host). Terminal links say "copy it" for
    // the rest (see `openableLink`).
    const local = (host: string) => ["[0-9]", "[0-9][0-9]", "[0-9][0-9][0-9]", "[0-9][0-9][0-9][0-9]", "[0-9][0-9][0-9][0-9][0-9]"].map((d) => ({ url: `http://${host}:${d}/*` }));
    expect(opener).toEqual({
      identifier: "opener:allow-open-url",
      allow: [{ url: "https://github.com/*" }, ...local("localhost"), ...local("127.0.0.1")],
    });
    expect(capabilities.windows).toEqual(["main"]);
    // The webview's own picker is asked for one folder (Add project) only. Files to attach
    // are picked through the shell's `pick_attachments` (see the attachment.import test).
    const opens = [...picker.matchAll(/open\(\{([^}]*)\}\)/g)].map((m) => m[1].replace(/, title:.*$/, "").trim());
    expect(opens).toEqual(["directory: true, multiple: false"]);
    expect(picker).toContain('pickFiles: () => pickAttachments(t("att.pickTitle"))');
    // The only event listened to is the webview's drag-drop (paths), and nothing is emitted.
    expect(picker).toContain("getCurrentWebview()\n      .onDragDropEvent(");
    expect(picker).not.toMatch(/\blisten\(/);
    expect(shell).not.toMatch(/\.emit(_to)?\(/);
    // No filesystem plugin is registered (the dialog crate depends on it, the app does not use it).
    expect(shell).not.toContain("tauri_plugin_fs");
  });

  it("the shell's own commands are pinned", () => {
    const list = /generate_handler!\[([^\]]*)\]/.exec(shell)?.[1] ?? "";
    expect(list.split(",").map((c) => c.trim())).toEqual([
      "rpc",
      "subscribe",
      "attach_open",
      "attach_write",
      // Flow control: the terminal acks what it drew (an id and a byte count, nothing else).
      "attach_ack",
      "attach_close",
      // The person's own shell in the side pane: the same stream as attach_open (writes,
      // acks and close above take its id), for a shell id the daemon gave (shell.open).
      // shell.attach is never in the bridge's allowlist.
      "shell_attach_open",
      "daemon_start",
      // After an update: what the daemon binary on disk says it is (`--version`, no shell,
      // a time limit), and restarting the daemon (daemon.shutdown, then the same start as
      // daemon_start; the webview still cannot call daemon.shutdown through the bridge).
      "daemon_binary_build",
      "daemon_restart",
      "home_dir",
      "path_kind",
      // Add project given a folder: the git checkouts in it (read-only, two levels deep,
      // capped; see scan.rs).
      "scan_repos",
      // Open in editor: which editors are installed (fixed app locations, nothing from the
      // webview), and opening a task's file (see editor.rs and the test below).
      "editors_available",
      "open_in_editor",
      // "Open in Preview" for a task's PDF: the daemon resolves the path and must call it a
      // PDF; /usr/bin/open -a Preview, cleared environment (see the test below).
      "open_path_external",
      // Attach files: the shell opens the native picker itself and grants what is chosen
      // (only a title comes from the webview; see grants.rs).
      "pick_attachments",
    ]);
    // Neither takes anything from the webview: the binary is the one the app resolves itself.
    expect(shell).toContain("async fn daemon_binary_build() -> Option<DaemonBinary>");
    expect(shell).toContain("async fn daemon_restart(app: AppHandle) -> Result<(), String>");
  });

  it("attachment.import takes only a path the person just dropped or picked (granted in Rust)", () => {
    // Every rpc call goes through the grant check first; an import needs a live grant.
    expect(shell).toMatch(/async fn rpc\(app: AppHandle, method: String, params: Value\)[^{]*\{\s*let c = app\.state::<Cockpit>\(\);\s*grants::check\(&c\.grants, &method, &params, Instant::now\(\)\)\?;/);
    expect(grantsRs).toContain("if method != method::ATTACHMENT_IMPORT {");
    expect(grantsRs).toContain("if grants.take(path, now) {");
    // Grants come only from the native drop event and the shell's own picker: nothing the
    // webview can call adds one (no command takes paths to grant).
    const granted = [...shell.matchAll(/\.grants\.grant\(([^;]*)\);/g)].map((m) => m[1]);
    expect(granted).toEqual(["paths.iter().cloned(), Instant::now()", "paths.iter().cloned(), Instant::now()"]);
    expect(shell).toMatch(/WindowEvent::DragDrop\(DragDropEvent::Drop \{ paths, \.\. \}\) => \{\s*window\.state::<Cockpit>\(\)\.grants\.grant\(/);
    expect(shell).toMatch(/async fn pick_attachments\(app: AppHandle, title: String\) -> Result<Vec<String>, String> \{[\s\S]*?\.blocking_pick_files\(\)[\s\S]*?\.grants\.grant\(paths/);
    // One use each, for ten minutes at most.
    expect(grantsRs).toContain("pub const TTL: Duration = Duration::from_secs(10 * 60);");
    expect(grantsRs).toMatch(/pub fn take\([\s\S]*?g\.remove\(i\);/);
    // The import itself is still in the bridge's allowlist (the daemon refuses its own home,
    // credential folders and key files whatever the shell grants).
    expect(bridgeRpc).toContain('"attachment.import"');
  });

  it("open_in_editor: a closed editor list, a daemon-checked path, no shell, no $EDITOR", () => {
    expect(shell).toMatch(/async fn open_in_editor\(\s*app: AppHandle,\s*task: String,\s*path: String,\s*line: Option<u32>,\s*editor: String,?\s*\) -> Result<\(\), String>/);
    expect(shell).toContain("fn editors_available(app: AppHandle) -> Vec<editor::Editor>");
    // The absolute path comes from the daemon (task.file with stat, through the bridge's
    // allowlist), never from the webview.
    expect(shell).toMatch(/editor::plan\(&editor, &task, &path, line, [\s\S]*?"stat": true[\s\S]*?bridge_rpc::call\(&socket, method::TASK_FILE, params\)/);
    // URLs go through the opener crate's Rust API; everything else is /usr/bin/open with a
    // cleared environment. No other program, no shell.
    expect(shell).toContain("tauri_plugin_opener::open_url(");
    expect(shell).toContain('Command::new("/usr/bin/open")');
    expect(shell).toContain(".env_clear()");
    const commands = [...(shell + editorRs).matchAll(/Command::new\(([^)]*)\)/g)].map((m) => m[1]);
    expect(commands).toEqual(['"/usr/bin/open"', '"/usr/bin/open"']);
    // open_path_external: task and relative path from the webview; the absolute path from
    // the daemon's task.file (stat), refused unless it is a PDF, checked again, opened in Preview.
    expect(shell).toContain("async fn open_path_external(app: AppHandle, task: String, path: String) -> Result<(), String>");
    expect(shell).toMatch(/async fn open_path_external[\s\S]*?"stat": true[\s\S]*?bridge_rpc::call\(&socket, method::TASK_FILE, params\)[\s\S]*?pdf_abs\(&file\)\?[\s\S]*?recheck_file[\s\S]*?\.args\(\["-a", "Preview", "--", &abs\]\)\s*\.env_clear\(\)/);
    expect(shell).toContain('if file.get("kind").and_then(Value::as_str) != Some("pdf") {');
    const code = editorRs.slice(0, editorRs.indexOf("#[cfg(test)]"));
    expect(code).not.toMatch(/std::process|env::var|"sh"|"-c"|bash/);
    // The editors, spelled out: nothing else parses.
    expect([...code.matchAll(/^\s+"(\w+)" => Editor::/gm)].map((m) => m[1])).toEqual(["vscode", "cursor", "zed", "sublime", "xcode", "default"]);
    // The default editor is the text editor (`open -t`), never "whatever opens this file type".
    expect(code).toContain('Editor::Default => open(&["-t"])');
  });

  it("scan_repos only reads: no shell or process, symlinks never followed, input checked, bounded", () => {
    expect(shell).toContain("async fn scan_repos(path: String) -> Result<scan::Scan, String>");
    expect(shell).toContain("scan::scan(&path, scan::LIMITS)");
    // The code itself (its tests make folders): no process, nothing written or removed.
    const code = scanner.slice(0, scanner.indexOf("#[cfg(test)]"));
    expect(code).not.toMatch(/std::process|Command::new|fs::remove|fs::write|create_dir|OpenOptions|read_link|canonicalize/);
    // The walk goes by `file_type` (which does not follow a link) and `symlink_metadata`.
    expect(scanner).toContain("entry.file_type()");
    expect(scanner).toContain("symlink_metadata(&git)");
    // The webview's path: absolute, no "..", an existing folder.
    expect(scanner).toMatch(/fn folder\(path: &str\)[\s\S]*is_absolute[\s\S]*ParentDir[\s\S]*is_dir/);
    expect(scanner).toContain("pub const LIMITS: Limits = Limits { depth: 2, max_dirs: 500, max_repos: 200, time: Duration::from_secs(3) };");
  });

  it("no component renders raw HTML: no dangerouslySetInnerHTML anywhere in src", () => {
    const sources = import.meta.glob(["./**/*.tsx", "./**/*.ts", "!./**/*.test.ts", "!./**/*.test.tsx"], { query: "?raw", import: "default", eager: true }) as Record<string, string>;
    expect(Object.keys(sources).length).toBeGreaterThan(20);
    for (const [name, text] of Object.entries(sources)) {
      expect(text, name).not.toContain("dangerouslySetInnerHTML");
      expect(text, name).not.toMatch(/\.innerHTML\s*=|insertAdjacentHTML|outerHTML\s*=/);
    }
  });

  it("reaches the daemon only through the bridge's allowlist, pinned here too", () => {
    // `mirante_bridge::rpc::ALLOWED`, as written in the source (constant names).
    const block = /pub const ALLOWED: &\[&str\] = &\[([\s\S]*?)\];/.exec(bridgeRpc)?.[1] ?? "";
    const allowed = [...block.matchAll(/method::([A-Z_]+)/g)].map((m) => m[1]);
    expect(allowed).toEqual([
      "TASK_CREATE",
      "TASK_LIST",
      "TASK_GET",
      "TASK_ARCHIVE",
      "TASK_EVENTS",
      "TASK_DIFF",
      "SESSION_START",
      "SESSION_RESUME",
      "SESSION_TRUST",
      "SESSION_STOP",
      "SESSION_INTERRUPT",
      "SESSION_RESIZE",
      "MESSAGE_SEND",
      "MESSAGE_LIST_QUEUED",
      "MESSAGE_EDIT",
      "MESSAGE_REMOVE",
      "MESSAGE_REORDER",
      "PERMISSION_LIST",
      "PERMISSION_ANSWER",
      "DAEMON_STATUS",
      "DAEMON_SETTINGS",
      "SESSION_HIBERNATE",
      "SESSION_WAKE",
      "DNA_STATUS",
      "DNA_ENABLE",
      "DNA_DISABLE",
      "DNA_GRAPH",
      "DNA_SEARCH",
      "DNA_GET",
      "DNA_CHECK",
      "DNA_TASK_CONTEXT",
      "DNA_PROPOSALS",
      "DNA_REVIEW",
      "DNA_NOTE_ACTION",
      "SKILL_TENANTS",
      "SKILL_LEARN",
      "SKILL_LIST",
      "SKILL_GET",
      "SKILL_TASK_SKILLS",
      "SKILL_CONFIRM_ORIGIN",
      "SKILL_LINT",
      "SKILL_MINE",
      "SKILL_REREVIEW",
      "SKILL_REPO_ORIGIN",
      "SKILL_PROPOSALS",
      "SKILL_PENDING",
      "SKILL_REVIEW",
      "SKILL_ACTION",
      "REPO_FILES",
      "TASK_FILES",
      "TASK_FILE",
      "TASK_SUBAGENTS",
      "GROUP_CREATE",
      "GROUP_GET",
      "GROUP_ARCHIVE",
      "GROUP_SYNTHESIZE",
      "TASK_THREAD",
      "TASK_SUMMARIZE",
      "TASK_ORCHESTRATE",
      "TASK_SPAWN_PROPOSALS",
      "TASK_SPAWN_REVIEW",
      "RULES_LAYERS",
      "ATTACHMENT_PUT",
      "ATTACHMENT_IMPORT",
      "ATTACHMENT_REMOVE",
      "SHELL_OPEN",
      "SHELL_RESIZE",
      "SHELL_CLOSE",
      "SHELL_LIST",
    ]);
    // Never SHELL_ATTACH: it takes over the connection, through the shell's own command.
    expect(block).not.toContain("SHELL_ATTACH");
    expect(shell).toMatch(/async fn shell_attach_open\(\s*app: AppHandle,\s*shell: String,\s*rows: u16,\s*cols: u16,\s*on_msg: Channel<InvokeResponseBody>,?\s*\) -> Result<Opened, BridgeError>/);
    expect(shell).toContain("Target::Shell(shell)");
    // Never the agent's orchestrator methods: proposing and reading children go through its
    // `mirante-orchestrator` MCP server; the cockpit only decides.
    expect(block).not.toContain("TASK_SPAWN_PROPOSE,");
    expect(block).not.toContain("TASK_CHILDREN");
    expect(block).not.toContain("TASK_CHILD_RESULT");
    // Never DNA_PROPOSE: agents propose through MCP; people write notes in their editor.
    expect(block).not.toContain("DNA_PROPOSE");
    // Never SKILL_PROPOSE (agents propose through their MCP server) nor SKILL_MAY_REVIEW
    // (the CLI's pre-prompt check).
    expect(block).not.toContain("SKILL_PROPOSE");
    expect(block).not.toContain("SKILL_MAY_REVIEW");
    // Never SKILL_ADD: its `dir` is any absolute path the caller names (the CLI's); the
    // cockpit re-reviews with SKILL_REREVIEW, whose folder the daemon names.
    expect(block).not.toContain("SKILL_ADD");
    // Every method the cockpit calls is one of them ("message.list_queued" is MESSAGE_LIST_QUEUED).
    // The namespace is the constant's first word; the rest keeps its underscores.
    const wire = new Set(allowed.map((c) => c.toLowerCase().replace(/^([a-z]+)_/, "$1.")));
    for (const m of Object.values(M)) expect(wire, m).toContain(m);
  });
});
