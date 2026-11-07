// The desktop shell's attack surface, pinned: a CSP with no remote origins or eval, and
// capabilities limited to notifications plus opening github.com pages. Widening either
// must be a deliberate edit of this test.
import { describe, expect, it } from "vitest";
import capabilities from "../src-tauri/capabilities/default.json";
import conf from "../src-tauri/tauri.conf.json";

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

  it("grants only notifications and opening github.com URLs", () => {
    const ids = capabilities.permissions.map((p) => (typeof p === "string" ? p : p.identifier));
    expect(ids).toEqual(["notification:default", "opener:allow-open-url"]);
    const opener = capabilities.permissions.find((p) => typeof p !== "string");
    expect(opener).toEqual({ identifier: "opener:allow-open-url", allow: [{ url: "https://github.com/*" }] });
    expect(capabilities.windows).toEqual(["main"]);
  });
});
