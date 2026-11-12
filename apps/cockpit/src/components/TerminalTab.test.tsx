import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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
  /** What happened to the terminal, in order (addons by name). */
  log: [] as string[],
  terms: 0,
  fits: 0,
  focused: 0,
  unicode: { activeVersion: "6" },
  webglThrows: false,
  contextLoss: null as (() => void) | null,
  webglDisposed: false,
  ro: null as (() => void) | null,
  /** The link provider the terminal registered, and the text of its buffer's lines. */
  provider: null as null | { provideLinks(y: number, cb: (links: { text: string; range: unknown; activate(e: object, text: string): void }[] | undefined) => void): void },
  lines: [] as string[],
}));

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    rows = 30;
    cols = 100;
    unicode = h.unicode;
    constructor(options: Record<string, unknown>) {
      h.options = options;
      h.term = this;
      h.terms++;
    }
    loadAddon(a: { name?: string }) {
      h.log.push(`addon:${a.name ?? "?"}`);
    }
    open() {
      h.log.push("open");
    }
    focus() {
      h.focused++;
    }
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
    reset() {
      h.log.push("reset");
    }
    registerLinkProvider(p: typeof h.provider) {
      h.provider = p;
      return { dispose() {} };
    }
    buffer = { active: { getLine: (y: number) => ({ translateToString: () => h.lines[y] ?? "" }) } };
    dispose() {
      h.disposed = true;
    }
  },
}));
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    name = "fit";
    fit() {
      h.fits++;
    }
  },
}));
vi.mock("@xterm/addon-unicode11", () => ({ Unicode11Addon: class { name = "unicode11"; } }));
vi.mock("@xterm/addon-webgl", () => ({
  WebglAddon: class {
    name = "webgl";
    constructor() {
      if (h.webglThrows) throw new Error("no WebGL2");
    }
    onContextLoss(fn: () => void) {
      h.contextLoss = fn;
      return { dispose() {} };
    }
    dispose() {
      h.webglDisposed = true;
    }
  },
}));
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
import { snapshot, task } from "../test/fixtures";
import { fontsPending, TerminalTab } from "./TerminalTab";
import { type PaneApi, PaneContext } from "./paneContext";
import css from "../styles.css?raw";

/** The declarations of `selector`'s one-line rule in styles.css. */
const rule = (selector: string) =>
  css
    .split("\n")
    .find((l: string) => l.startsWith(`${selector} {`))
    ?.slice(selector.length + 2) ?? "";

globalThis.ResizeObserver = class {
  constructor(fn: () => void) {
    h.ro = fn;
  }
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

function mount(t: TaskView, active?: boolean, pane: PaneApi | null = null, answers: Record<string, unknown> = {}) {
  const { env, calls } = makeEnv(answers);
  const r = render(
    <EnvContext.Provider value={env}>
      <PaneContext.Provider value={pane}>
        <TerminalTab t={t} active={active} />
      </PaneContext.Provider>
    </EnvContext.Provider>,
  );
  const rerender = (next: TaskView, nextActive?: boolean) =>
    r.rerender(
      <EnvContext.Provider value={env}>
        <PaneContext.Provider value={pane}>
          <TerminalTab t={next} active={nextActive} />
        </PaneContext.Provider>
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
    h.provider = null;
    h.lines = [];
    h.attachOpen.mockReset();
    h.attachWrite.mockReset();
    h.attachWrite.mockImplementation(async () => {});
    h.attachClose.mockClear();
    h.attachAck.mockClear();
    h.parsed = [];
    h.log = [];
    h.terms = 0;
    h.fits = 0;
    h.focused = 0;
    h.unicode = { activeVersion: "6" };
    h.webglThrows = false;
    h.contextLoss = null;
    h.webglDisposed = false;
    h.ro = null;
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

  it("terminal links leave only through the opener: github.com and localhost ports, on a modifier click", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const { env } = mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    const link = h.options!.linkHandler as LinkHandler;
    // On, so OSC 8 file:// links reach `activate` (they open in the side pane, never the opener).
    expect(link.allowNonHttpProtocols).toBe(true);
    link.activate({}, "https://github.com/o/r/pull/1");
    expect(env.openUrl).not.toHaveBeenCalled();
    link.activate({ metaKey: true }, "javascript:alert(1)");
    link.activate({ metaKey: true }, "file:///etc/passwd");
    expect(env.openUrl).not.toHaveBeenCalled();
    link.activate({ metaKey: true }, "https://github.com/o/r/pull/1");
    link.activate({ ctrlKey: true }, "http://localhost:5173/app");
    link.activate({ metaKey: true }, "http://127.0.0.1:8080/");
    expect(vi.mocked(env.openUrl).mock.calls).toEqual([["https://github.com/o/r/pull/1"], ["http://localhost:5173/app"], ["http://127.0.0.1:8080/"]]);
  });

  describe("file paths", () => {
    const WT = "/Users/me/.mirante/worktrees/shop/feat-a";
    const paneApi = () => ({ taskId: "a", openFile: vi.fn(), openTarget: vi.fn() });
    const stat = { "task.file": { path: "x", abs: "/x", size: 1, kind: "text" } };
    const linksOf = (y: number) => {
      let got: { text: string; activate(e: object, text: string): void }[] | undefined;
      h.provider!.provideLinks(y, (l) => (got = l));
      return got ?? [];
    };

    it("⌘-click on a detected path opens the pane, plain click does nothing", async () => {
      h.attachOpen.mockResolvedValue({ id: 6, input: true });
      h.lines = ["error[E0308]: mismatched types --> src/main.rs:12:5", "see https://github.com/o/r/blob/main/a.ts"];
      const pane = paneApi();
      const r = mount(task("a", { worktree_path: WT }), true, pane, stat);
      await waitFor(() => expect(h.provider).not.toBeNull());
      const [l] = linksOf(1);
      expect(l.text).toBe("src/main.rs:12:5");
      expect(linksOf(2)).toEqual([]);
      l.activate({}, l.text);
      expect(r.calls).toEqual([]);
      await act(async () => l.activate({ metaKey: true }, l.text));
      expect(r.calls).toContainEqual(["task.file", { task: "a", path: "src/main.rs", stat: true }]);
      expect(pane.openFile).toHaveBeenCalledWith("src/main.rs", 12);
    });

    it("a path outside the worktree, or one the daemon refuses, is a note, not an error", async () => {
      h.attachOpen.mockResolvedValue({ id: 6, input: true });
      h.lines = ["read /etc/hosts and gone.rs"];
      const pane = paneApi();
      mount(task("a", { worktree_path: WT }), true, pane, { "task.file": { kind: "rpc", code: 1, message: "gone" } });
      await waitFor(() => expect(h.provider).not.toBeNull());
      const [abs, missing] = linksOf(1);
      await act(async () => abs.activate({ metaKey: true }, abs.text));
      expect(screen.getByRole("status")).toHaveTextContent("/etc/hosts is not a file in this task's worktree");
      await act(async () => missing.activate({ metaKey: true }, missing.text));
      expect(screen.getByRole("status")).toHaveTextContent("gone.rs is not a file in this task's worktree");
      expect(pane.openFile).not.toHaveBeenCalled();
    });

    it("OSC 8 file:// with empty or localhost host maps into the pane; other hosts say copy", async () => {
      h.attachOpen.mockResolvedValue({ id: 6, input: true });
      const pane = paneApi();
      const { env } = mount(task("a", { worktree_path: WT }), true, pane, stat);
      await waitFor(() => expect(h.onData).not.toBeNull());
      const link = h.options!.linkHandler as LinkHandler;
      await act(async () => link.activate({ metaKey: true }, `file://${WT}/src/a%20b.ts#L3`));
      expect(pane.openFile).toHaveBeenCalledWith("src/a b.ts", 3);
      await act(async () => link.activate({ ctrlKey: true }, `file://localhost${WT}/b.ts`));
      expect(pane.openFile).toHaveBeenLastCalledWith("b.ts", undefined);
      await act(async () => link.activate({ metaKey: true }, "file://evil.example/etc/passwd"));
      expect(screen.getByRole("status")).toHaveTextContent("points to another machine");
      expect(env.openUrl).not.toHaveBeenCalled();
      expect(pane.openFile).toHaveBeenCalledTimes(2);
    });
  });

  it("a link the app may not open says to copy it (not a silent no-op)", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const { env } = mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    const link = h.options!.linkHandler as LinkHandler;
    for (const u of ["https://example.com/x", "http://localhost/", "http://localhost:3000@evil.example/", "https://localhost:3000/", "http://github.com/x"]) {
      act(() => link.activate({ metaKey: true }, u));
    }
    expect(env.openUrl).not.toHaveBeenCalled();
    expect(screen.getByRole("status")).toHaveTextContent("Mirante opens only github.com and localhost links — copy it instead");
  });

  it("a link the opener refuses says it could not be opened", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const { env } = mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    vi.mocked(env.openUrl).mockRejectedValueOnce(new Error("forbidden"));
    const link = h.options!.linkHandler as LinkHandler;
    await act(async () => link.activate({ metaKey: true }, "https://github.com/o/r"));
    expect(screen.getByRole("status")).toHaveTextContent("could not open the link");
  });

  it("a lost GPU context is tried again when the window is shown", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    h.contextLoss!();
    document.dispatchEvent(new Event("visibilitychange"));
    expect(h.log.filter((l) => l === "addon:webgl")).toHaveLength(2);
    // Not again while it holds.
    document.dispatchEvent(new Event("visibilitychange"));
    expect(h.log.filter((l) => l === "addon:webgl")).toHaveLength(2);
  });

  it("renders on the GPU when it can (loaded after open), and falls back to the DOM renderer", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const r = mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    expect(h.log.indexOf("addon:webgl")).toBeGreaterThan(h.log.indexOf("open"));
    // A lost context drops the GPU renderer; xterm goes back to the DOM one.
    h.contextLoss!();
    expect(h.webglDisposed).toBe(true);
    r.unmount();
    h.webglThrows = true;
    h.onData = null;
    mount(task("b"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    expect(h.log.filter((l) => l === "addon:webgl")).toHaveLength(1);
  });

  it("uses JetBrains Mono (else the system monospace), the graphite theme and Unicode 11 widths, and takes focus when it opens", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    expect(h.options!.fontFamily).toBe('"JetBrains Mono", "SF Mono", Menlo, monospace');
    expect((h.options as { theme?: { background?: string } }).theme?.background).toBe("#141519");
    expect(h.options!.allowProposedApi).toBe(true);
    expect(h.log).toContain("addon:unicode11");
    expect(h.unicode.activeVersion).toBe("11");
    expect(h.focused).toBe(1);
  });

  it("stays attached while hidden: a tab switch is no new terminal and no new attach", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const r = mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    r.rerender(task("a"), false);
    r.rerender(task("a", { status: "working" }), false);
    r.rerender(task("a"), true);
    expect([h.terms, h.attachOpen.mock.calls.length, h.attachClose.mock.calls.length, h.disposed]).toEqual([1, 1, 0, false]);
  });

  it("is not attached before it is first shown (a hidden view has no size to give the PTY)", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const r = mount(task("a"), false);
    await new Promise((res) => setTimeout(res, 20));
    expect([h.terms, h.attachOpen.mock.calls.length]).toEqual([0, 0]);
    r.rerender(task("a"), true);
    await waitFor(() => expect(h.attachOpen).toHaveBeenCalledTimes(1));
    expect(h.focused).toBe(1);
  });

  it("opened while hidden it does not take focus", async () => {
    h.attachOpen.mockImplementation(() => new Promise((res) => setTimeout(() => res({ id: 6, input: true }), 10)));
    const r = mount(task("a"));
    r.rerender(task("a"), false);
    await waitFor(() => expect(h.onData).not.toBeNull());
    expect(h.focused).toBe(0);
  });

  it("waits for the fonts before the first fit and the attach at its size", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    let loaded!: () => void;
    const ready = new Promise<void>((res) => (loaded = res));
    Object.defineProperty(document, "fonts", { configurable: true, value: { status: "loading", ready } });
    const width = vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(800);
    const height = vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(400);
    try {
      mount(task("a"));
      await new Promise((res) => setTimeout(res, 20));
      expect(h.fits).toBe(0);
      expect(h.attachOpen).not.toHaveBeenCalled();
      loaded();
      await waitFor(() => expect(h.attachOpen).toHaveBeenCalledTimes(1));
      expect(h.fits).toBe(1);
    } finally {
      delete (document as { fonts?: unknown }).fonts;
      width.mockRestore();
      height.mockRestore();
    }
  });

  it("loads the bundled JetBrains Mono (regular and bold) before the first fit, even when nothing else is loading", async () => {
    // The canvas never asks for the web font, so the set can say "loaded" before it is.
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    let fetched!: () => void;
    const face = new Promise<FontFace[]>((res) => (fetched = () => res([])));
    const asked: string[] = [];
    const fonts = { status: "loaded", ready: Promise.resolve(), check: () => false, load: (f: string) => (asked.push(f), face) };
    Object.defineProperty(document, "fonts", { configurable: true, value: fonts });
    const width = vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(800);
    const height = vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(400);
    try {
      mount(task("a"));
      await new Promise((res) => setTimeout(res, 20));
      expect(asked).toEqual(['13px "JetBrains Mono"', 'bold 13px "JetBrains Mono"']);
      expect(h.fits).toBe(0);
      expect(h.attachOpen).not.toHaveBeenCalled();
      fetched();
      await waitFor(() => expect(h.attachOpen).toHaveBeenCalledTimes(1));
      expect(h.fits).toBe(1);
    } finally {
      delete (document as { fonts?: unknown }).fonts;
      width.mockRestore();
      height.mockRestore();
    }
  });

  it("does not wait when the terminal's faces are already loaded", () => {
    const doc = { fonts: { status: "loaded", ready: Promise.resolve(), check: () => true, load: vi.fn() } } as unknown as Document;
    expect(fontsPending(doc)).toBeNull();
    expect(doc.fonts.load).not.toHaveBeenCalled();
    expect(fontsPending({} as Document)).toBeNull();
  });

  it("fits at most once per frame, and never while hidden", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const width = vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(800);
    const height = vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(400);
    try {
      mount(task("a"));
      await waitFor(() => expect(h.onData).not.toBeNull());
      const before = h.fits;
      for (let i = 0; i < 5; i++) h.ro!();
      await new Promise((res) => setTimeout(res, 40));
      expect(h.fits - before).toBe(1);
      width.mockReturnValue(0);
      h.ro!();
      await new Promise((res) => setTimeout(res, 40));
      expect(h.fits - before).toBe(1);
    } finally {
      width.mockRestore();
      height.mockRestore();
    }
  });

  it("hovering a link sends no session.resize: the hint floats over the terminal", async () => {
    h.attachOpen.mockResolvedValue({ id: 6, input: true });
    const r = mount(task("a"));
    await waitFor(() => expect(h.onData).not.toBeNull());
    const link = h.options!.linkHandler as LinkHandler;
    act(() => link.hover!({}, "https://example.com/x"));
    expect(screen.getByText(/example\.com/).closest(".terminal-overlay")).not.toBeNull();
    act(() => link.leave!({}, "https://example.com/x"));
    await new Promise((res) => setTimeout(res, 150));
    expect(r.calls.filter(([m]) => m === "session.resize")).toEqual([]);
  });

  it("its size depends on its pane only: the terminal fills it, notes and hints float over it", () => {
    expect(rule(".terminal")).toMatch(/position: absolute; inset: 0/);
    expect(rule(".terminal-overlay")).toMatch(/position: absolute/);
    expect(rule(".terminal-overlay")).toMatch(/pointer-events: none/);
    expect(rule(".term-overlay")).toMatch(/position: absolute/);
    expect(rule(".detail-head")).toMatch(/flex-wrap: nowrap/);
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
  describe("re-attach", () => {
    afterEach(() => vi.useRealTimers());
    const advance = (ms: number) => act(async () => void (await vi.advanceTimersByTimeAsync(ms)));

    it("a stream that ends while the task is live re-attaches after a short wait, on a reset terminal", async () => {
      vi.useFakeTimers();
      const ends: (() => void)[] = [];
      h.attachOpen.mockImplementation(async (_t: string, _r: number, _c: number, _d: unknown, onEnd: () => void) => {
        ends.push(onEnd);
        return { id: ends.length, input: true };
      });
      mount(task("a"));
      await advance(0);
      expect(h.attachOpen).toHaveBeenCalledTimes(1);
      act(() => ends[0]());
      expect(screen.getByRole("status")).toHaveTextContent("stream ended");
      await advance(400);
      expect(h.attachOpen).toHaveBeenCalledTimes(1);
      await advance(200);
      expect(h.attachOpen).toHaveBeenCalledTimes(2);
      // The daemon replays the screen on attach: the old one is cleared first, same terminal.
      expect(h.log.filter((l) => l === "reset")).toHaveLength(1);
      expect(h.terms).toBe(1);
      expect(screen.queryByRole("status")).toBeNull();
      // Keys go to the new stream.
      h.onData!("x");
      expect(sent()).toEqual([[2, "x"]]);
    });

    it("a failed re-attach backs off (0.5, 1, 2, 5 s), then offers Reconnect", async () => {
      vi.useFakeTimers();
      let end!: () => void;
      h.attachOpen.mockImplementationOnce(async (_t: string, _r: number, _c: number, _d: unknown, onEnd: () => void) => {
        end = onEnd;
        return { id: 1, input: true };
      });
      h.attachOpen.mockRejectedValue(new DaemonError({ kind: "disconnected", message: "gone" }));
      mount(task("a"));
      await advance(0);
      act(() => end());
      for (const [ms, calls] of [
        [500, 2],
        [1000, 3],
        [2000, 4],
        [5000, 5],
      ] as const) {
        await advance(ms - 1);
        expect(h.attachOpen).toHaveBeenCalledTimes(calls - 1);
        await advance(1);
        expect(h.attachOpen).toHaveBeenCalledTimes(calls);
      }
      await advance(60_000);
      expect(h.attachOpen).toHaveBeenCalledTimes(5);
      h.attachOpen.mockResolvedValue({ id: 9, input: true });
      act(() => screen.getByRole("button", { name: "Reconnect" }).click());
      await advance(0);
      expect(h.attachOpen).toHaveBeenCalledTimes(6);
      expect(screen.queryByRole("button", { name: "Reconnect" })).toBeNull();
    });

    it("a new daemon boot re-attaches at once on a reset terminal", async () => {
      h.attachOpen.mockImplementation(async () => ({ id: h.attachOpen.mock.calls.length, input: true }));
      const r = mount(task("a"));
      act(() => r.env.cockpit.dispatch({ type: "connected", snapshot: snapshot([task("a")], 0, { boot_id: "b1" }) }));
      await waitFor(() => expect(h.attachOpen).toHaveBeenCalledTimes(1));
      act(() => r.env.cockpit.dispatch({ type: "connected", snapshot: snapshot([task("a")], 0, { boot_id: "b2" }) }));
      await waitFor(() => expect(h.attachOpen).toHaveBeenCalledTimes(2));
      expect(h.attachClose).toHaveBeenCalledWith(1);
      expect(h.log).toContain("reset");
      expect(h.terms).toBe(1);
    });

    it("an ended stream is not re-attached once the task is no longer live", async () => {
      vi.useFakeTimers();
      let end!: () => void;
      h.attachOpen.mockImplementation(async (_t: string, _r: number, _c: number, _d: unknown, onEnd: () => void) => {
        end = onEnd;
        return { id: 1, input: true };
      });
      const r = mount(task("a"));
      await advance(0);
      act(() => end());
      r.rerender(task("a", { live: false }));
      await advance(10_000);
      expect(h.attachOpen).toHaveBeenCalledTimes(1);
    });
  });
});
