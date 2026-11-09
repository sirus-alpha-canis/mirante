// The desktop shell's attack surface, pinned: a CSP with no remote origins or eval, and
// capabilities limited to notifications, opening github.com and localhost pages and the folder picker. Widening either
// must be a deliberate edit of this test.
import { describe, expect, it } from "vitest";
import capabilities from "../src-tauri/capabilities/default.json";
import conf from "../src-tauri/tauri.conf.json";
import shell from "../src-tauri/src/lib.rs?raw";
import picker from "./main.tsx?raw";
import bridgeRpc from "../../../crates/mirante-bridge/src/rpc.rs?raw";
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
    expect(csp).not.toMatch(/unsafe-eval|https?:\/\/(?!ipc\.localhost)|\*/);
    expect(directive("object-src")).toBe("object-src 'none'");
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

  it("grants only notifications, opening github.com and localhost URLs, and the folder picker", () => {
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
    // The picker is asked for one folder, never files or several.
    expect(picker).toContain("directory: true");
    expect(picker).toContain("multiple: false");
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
      "daemon_start",
      // After an update: what the daemon binary on disk says it is (`--version`, no shell,
      // a time limit), and restarting the daemon (daemon.shutdown, then the same start as
      // daemon_start; the webview still cannot call daemon.shutdown through the bridge).
      "daemon_binary_build",
      "daemon_restart",
      "home_dir",
      "path_kind",
    ]);
    // Neither takes anything from the webview: the binary is the one the app resolves itself.
    expect(shell).toContain("async fn daemon_binary_build() -> Option<DaemonBinary>");
    expect(shell).toContain("async fn daemon_restart(app: AppHandle) -> Result<(), String>");
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
      "RULES_LAYERS",
    ]);
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
