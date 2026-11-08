import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

type LinkHandler = {
  activate(e: { metaKey?: boolean; ctrlKey?: boolean }, uri: string): void;
  hover?(e: object, uri: string): void;
  leave?(e: object, uri: string): void;
  allowNonHttpProtocols?: boolean;
};

const h = vi.hoisted(() => ({
  written: [] as Uint8Array[],
  onData: null as ((d: string) => void) | null,
  onBinary: null as ((d: string) => void) | null,
  onResize: null as ((s: { rows: number; cols: number }) => void) | null,
  term: null as { rows: number; cols: number } | null,
  disposed: false,
  options: null as Record<string, unknown> | null,
  attachOpen: vi.fn(),
  attachWrite: vi.fn(async (_id: number, _bytes: Uint8Array) => {}),
  attachClose: vi.fn(async (_id: number) => {}),
  attachAck: vi.fn(async (_id: number, _bytes: number) => {}),
  /** Write callbacks xterm has not called yet (it calls them once it parsed the chunk). */
  parsed: [] as (() => void)[],
}));

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    rows = 30;
    cols = 100;
    constructor(options: Record<string, unknown>) {
      h.options = options;
      h.term = this;
    }
    loadAddon() {}
    open() {}
    write(b: Uint8Array, cb?: () => void) {
      if (h.disposed) throw new Error("write after dispose");
      h.written.push(b);
      if (cb) h.parsed.push(cb);
    }
    onData(fn: (d: string) => void) {
      h.onData = fn;
      return { dispose() {} };
    }
    onBinary(fn: (d: string) => void) {
      h.onBinary = fn;
      return { dispose() {} };
    }
    onResize(fn: (s: { rows: number; cols: number }) => void) {
      h.onResize = fn;
      return { dispose() {} };
    }
    dispose() {
      h.disposed = true;
    }
  },
}));
vi.mock("@xterm/addon-fit", () => ({ FitAddon: class { fit() {} } }));
vi.mock("../lib/daemon", async (actual) => ({
  ...(await actual<typeof import("../lib/daemon")>()),
  attachOpen: h.attachOpen,
  attachWrite: h.attachWrite,
  attachClose: h.attachClose,
  attachAck: h.attachAck,
}));

import { EnvContext } from "../env";
import { DaemonError } from "../lib/daemon";
import type { TaskView } from "../lib/protocol";
import { makeEnv } from "../test/env";
import { task } from "../test/fixtures";
import { TerminalTab } from "./TerminalTab";

globalThis.ResizeObserver = class {
  observe() {}
  disconnect() {}
} as unknown as typeof ResizeObserver;

const text = (b: Uint8Array) => new TextDecoder().decode(b);
const sent = () => h.attachWrite.mock.calls.map(([id, b]) => [id, text(b)]);

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function mount(t: TaskView) {
  const { env, calls } = makeEnv();
  const r = render(
    <EnvContext.Provider value={env}>
      <TerminalTab t={t} />
    </EnvContext.Provider>,
  );
  const rerender = (next: TaskView) =>
    r.rerender(
      <EnvContext.Provider value={env}>
        <TerminalTab t={next} />
      </EnvContext.Provider>,
    );
  return { ...r, env, calls, rerender };
}

describe("TerminalTab", () => {
  beforeEach(() => {
    h.written = [];
    h.onData = null;
    h.onBinary = null;
    h.onResize = null;
    h.term = null;
    h.disposed = false;
    h.options = null;
    h.attachOpen.mockReset();
    h.attachWrite.mockReset();
    h.attachWrite.mockImplementation(async () => {});
    h.attachClose.mockClear();
    h.attachAck.mockClear();
    h.parsed = [];
  });

  it("attaches at the terminal's size, writes output, sends keys, detaches on unmount", async () => {
    h.attachOpen.mockImplementation(async (_t: string, _r: number, _c: number, onData: (b: Uint8Array) => void) => {
      onData(new TextEncoder().encode("hi"));
      return { id: 4, input: true };
    });
    const r = mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    expect(h.attachOpen.mock.calls[0].slice(0, 3)).toEqual(["a", 30, 100]);
    expect(text(h.written[0])).toBe("hi");
    h.onData!("ls\r");
    expect(sent()).toEqual([[4, "ls\r"]]);
    r.unmount();
    expect(h.attachClose).toHaveBeenCalledWith(4);
    expect(h.disposed).toBe(true);
  });

  it("the terminal is a labelled region", () => {
    h.attachOpen.mockReturnValue(new Promise(() => {}));
    mount(task("a"));
    expect(screen.getByRole("region", { name: "Terminal of feat/a" })).toBeInTheDocument();
  });

  it("a read-only stream says so and drops keys", async () => {
    h.attachOpen.mockResolvedValue({ id: 5, input: false });
    mount(task("c", { agent: "codex" }));
    expect(await screen.findByRole("status")).toHaveTextContent("read-only stream");
    h.onData?.("x");
    expect(h.attachWrite).not.toHaveBeenCalled();
  });

  it("no live session, no attach", () => {
    mount(task("a", { live: false }));
    expect(screen.getByText(/No live session/)).toBeInTheDocument();
    expect(h.attachOpen).not.toHaveBeenCalled();
  });

  it("keys reach the daemon in order: one write in flight, the rest coalesced behind it", async () => {
    h.attachOpen.mockResolvedValue({ id: 4, input: true });
    const first = deferred<void>();
    h.attachWrite.mockImplementationOnce(() => first.promise);
    mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    h.onData!("a");
    h.onData!("b");
    h.onData!("c");
    expect(sent()).toEqual([[4, "a"]]);
    await act(async () => first.resolve());
    await waitFor(() => expect(sent()).toEqual([[4, "a"], [4, "bc"]]));
  });

  it("binary input (mouse reports) is forwarded byte for byte", async () => {
    h.attachOpen.mockResolvedValue({ id: 4, input: true });
    mount(task("a"));
    await waitFor(() => expect(h.onBinary).not.toBeNull());
    h.onBinary!("\x1b[M\xff");
    expect([...h.attachWrite.mock.calls[0][1]]).toEqual([0x1b, 0x5b, 0x4d, 0xff]);
  });

  it("a write that times out says so and drops what was typed behind it", async () => {
    h.attachOpen.mockResolvedValue({ id: 4, input: true });
    const first = deferred<void>();
    h.attachWrite.mockImplementationOnce(() => first.promise);
    mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    h.onData!("a");
    h.onData!("b");
    await act(async () => first.reject(new DaemonError({ kind: "timeout", message: "5s" })));
    expect(await screen.findByRole("status")).toHaveTextContent("input not delivered (the daemon timed out)");
    expect(sent()).toEqual([[4, "a"]]);
    h.onData!("c");
    await waitFor(() => expect(sent()).toEqual([[4, "a"], [4, "c"]]));
  });

  it("unmounted before the attach resolves: detaches, writes nothing, takes no keys", async () => {
    const open = deferred<{ id: number; input: boolean }>();
    let output: ((b: Uint8Array) => void) | null = null;
    h.attachOpen.mockImplementation((_t: string, _r: number, _c: number, onData: (b: Uint8Array) => void) => {
      output = onData;
      return open.promise;
    });
    const r = mount(task("a"));
    r.unmount();
    await act(async () => open.resolve({ id: 9, input: true }));
    expect(h.attachClose).toHaveBeenCalledWith(9);
    expect(h.onData).toBeNull();
    expect(() => output!(new TextEncoder().encode("late"))).not.toThrow();
    expect(h.written).toEqual([]);
  });

  it("switching task: the old stream's end is not reported on the new one, and the note resets", async () => {
    const ends: Record<string, () => void> = {};
    h.attachOpen.mockImplementation(async (t: string, _r: number, _c: number, _d: unknown, onEnd: () => void) => {
      ends[t] = onEnd;
      return { id: t === "a" ? 1 : 2, input: true };
    });
    const r = mount(task("a"));
    await waitFor(() => expect(ends.a).toBeDefined());
    act(() => ends.a());
    expect(await screen.findByRole("status")).toHaveTextContent("stream ended");
    r.rerender(task("b"));
    await waitFor(() => expect(ends.b).toBeDefined());
    expect(screen.queryByRole("status")).toBeNull();
    expect(h.attachClose).toHaveBeenCalledWith(1);
    act(() => ends.a());
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("resizes are debounced into one session.resize, and one is sent if the size changed before attach", async () => {
    const open = deferred<{ id: number; input: boolean }>();
    h.attachOpen.mockReturnValue(open.promise);
    const r = mount(task("a"));
    // Before the attach answers: the size changes; nothing can be sent yet.
    h.term!.rows = 40;
    h.term!.cols = 120;
    h.onResize!({ rows: 40, cols: 120 });
    await act(async () => open.resolve({ id: 3, input: true }));
    await waitFor(() => expect(r.calls).toEqual([["session.resize", { task: "a", rows: 40, cols: 120 }]]));
    for (const [rows, cols] of [
      [41, 121],
      [42, 122],
      [43, 123],
    ]) {
      h.term!.rows = rows;
      h.term!.cols = cols;
      h.onResize!({ rows, cols });
    }
    await new Promise((res) => setTimeout(res, 200));
    expect(r.calls).toEqual([
      ["session.resize", { task: "a", rows: 40, cols: 120 }],
      ["session.resize", { task: "a", rows: 43, cols: 123 }],
    ]);
  });

  it("acks output once xterm has parsed it, in batches, so the bridge keeps reading", async () => {
    let output!: (b: Uint8Array) => void;
    h.attachOpen.mockImplementation(async (_t: string, _r: number, _c: number, onData: (b: Uint8Array) => void) => {
      output = onData;
      onData(new Uint8Array(100)); // the repaint, before the attach answers
      return { id: 4, input: true };
    });
    mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    for (let i = 0; i < 3; i++) output(new Uint8Array(40 * 1024));
    expect(h.attachAck).not.toHaveBeenCalled(); // nothing parsed yet
    act(() => h.parsed.splice(0).forEach((cb) => cb()));
    // 100 + 3 × 40 KiB parsed: one ack past the batch size, the rest after a moment.
    expect(h.attachAck.mock.calls.reduce((n, [, b]) => n + b, 0)).toBeGreaterThanOrEqual(64 * 1024);
    await waitFor(() => expect(h.attachAck.mock.calls.reduce((n, [, b]) => n + b, 0)).toBe(100 + 3 * 40 * 1024));
    expect(h.attachAck.mock.calls.every(([id]) => id === 4)).toBe(true);
    expect(h.attachAck.mock.calls.length).toBeLessThanOrEqual(3);
  });

  it("terminal links leave only through the opener: http(s), on a modifier click", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const { env } = mount(task("a"));
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

  it("hovering a link shows where it really goes, whatever its text says", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    const link = h.options!.linkHandler as LinkHandler;
    act(() => link.hover!({}, "https://evil.example/login"));
    expect(screen.getByText(/https:\/\/evil\.example\/login/)).toBeInTheDocument();
    act(() => link.leave!({}, "https://evil.example/login"));
    expect(screen.queryByText(/evil\.example/)).toBeNull();
  });
});
