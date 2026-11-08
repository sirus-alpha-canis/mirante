import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

type LinkHandler = { activate(e: { metaKey?: boolean; ctrlKey?: boolean }, uri: string): void; allowNonHttpProtocols?: boolean };

const h = vi.hoisted(() => ({
  written: [] as Uint8Array[],
  onData: null as ((d: string) => void) | null,
  disposed: false,
  options: null as Record<string, unknown> | null,
  attachOpen: vi.fn(),
  attachWrite: vi.fn(async () => {}),
  attachClose: vi.fn(async () => {}),
}));

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    rows = 30;
    cols = 100;
    constructor(options: Record<string, unknown>) {
      h.options = options;
    }
    loadAddon() {}
    open() {}
    write(b: Uint8Array) {
      h.written.push(b);
    }
    onData(fn: (d: string) => void) {
      h.onData = fn;
    }
    dispose() {
      h.disposed = true;
    }
  },
}));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class { fit() {} } }));
vi.mock("../lib/daemon", () => ({ attachOpen: h.attachOpen, attachWrite: h.attachWrite, attachClose: h.attachClose }));

import { EnvContext } from "../env";
import { makeEnv } from "../test/env";
import { task } from "../test/fixtures";
import { TerminalTab } from "./TerminalTab";

globalThis.ResizeObserver = class {
  observe() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

describe("TerminalTab", () => {
  beforeEach(() => {
    h.written = [];
    h.onData = null;
    h.disposed = false;
    h.options = null;
    h.attachOpen.mockReset();
    h.attachWrite.mockClear();
    h.attachClose.mockClear();
  });

  it("attaches at the terminal's size, writes output, sends keys, detaches on unmount", async () => {
    h.attachOpen.mockImplementation(async (_t: string, _r: number, _c: number, onData: (b: Uint8Array) => void) => {
      onData(new TextEncoder().encode("hi"));
      return { id: 4, input: true };
    });
    const { env } = makeEnv();
    const r = render(
      <EnvContext.Provider value={env}>
        <TerminalTab t={task("a")} />
      </EnvContext.Provider>,
    );
    await waitFor(() => expect(h.onData).not.toBeNull());
    expect(h.attachOpen.mock.calls[0].slice(0, 3)).toEqual(["a", 30, 100]);
    expect(new TextDecoder().decode(h.written[0])).toBe("hi");
    h.onData!("ls\r");
    expect(h.attachWrite).toHaveBeenCalledWith(4, "ls\r");
    r.unmount();
    expect(h.attachClose).toHaveBeenCalledWith(4);
    expect(h.disposed).toBe(true);
  });

  it("a read-only stream says so and drops keys", async () => {
    h.attachOpen.mockResolvedValue({ id: 5, input: false });
    const { env } = makeEnv();
    render(
      <EnvContext.Provider value={env}>
        <TerminalTab t={task("c", { agent: "codex" })} />
      </EnvContext.Provider>,
    );
    expect(await screen.findByRole("status")).toHaveTextContent("read-only stream");
    h.onData!("x");
    expect(h.attachWrite).not.toHaveBeenCalled();
  });

  it("no live session, no attach", () => {
    const { env } = makeEnv();
    render(
      <EnvContext.Provider value={env}>
        <TerminalTab t={task("a", { live: false })} />
      </EnvContext.Provider>,
    );
    expect(screen.getByText(/No live session/)).toBeInTheDocument();
    expect(h.attachOpen).not.toHaveBeenCalled();
  });

  it("terminal links leave only through the opener: http(s), on a modifier click", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const { env } = makeEnv();
    render(
      <EnvContext.Provider value={env}>
        <TerminalTab t={task("a")} />
      </EnvContext.Provider>,
    );
    await waitFor(() => expect(h.onData).not.toBeNull());
    const link = h.options!.linkHandler as LinkHandler;
    expect(link.allowNonHttpProtocols).toBe(false);
    link.activate({}, "https://example.com/x");
    expect(env.openUrl).not.toHaveBeenCalled();
    link.activate({ metaKey: true }, "javascript:alert(1)");
    link.activate({ metaKey: true }, "file:///etc/passwd");
    expect(env.openUrl).not.toHaveBeenCalled();
    link.activate({ metaKey: true }, "https://example.com/x");
    expect(env.openUrl).toHaveBeenCalledWith("https://example.com/x");
  });
});
