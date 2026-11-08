import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./components/TerminalTab", () => ({ TerminalTab: () => null }));

import { App, NOTIFY_BATCH_MS, STATUS_POLL_MS, visibleTasks } from "./App";
import { THEME_KEY } from "./lib/theme";
import { ev, snapshot, task } from "./test/fixtures";
import { makeEnv } from "./test/env";

const mod = /Mac/.test(navigator.platform) ? { metaKey: true } : { ctrlKey: true };

const STATUS = { pid: 1, boot_id: "b1", version: "x", live_sessions: 0 };

function setup(extra: Record<string, unknown> = {}, tasks = [task("a", { repo_path: "/repos/shop" }), task("z", { repo_path: "/repos/other" })]) {
  const h = makeEnv({
    "daemon.status": STATUS,
    ...extra,
    "message.list_queued": [],
    "task.get": (p: Record<string, unknown>) => h.env.cockpit.getState().tasks[p.task as string],
  });
  render(<App env={h.env} />);
  act(() => h.load(tasks));
  return h;
}

const needs = (seq: number, id: string, boot = "b1") => ev(seq, id, "status", { to: "needs_you" }, boot);
const flushNotes = () => act(() => vi.advanceTimersByTime(NOTIFY_BATCH_MS));
const titles = (h: ReturnType<typeof makeEnv>) => vi.mocked(h.env.notify).mock.calls.map(([n]) => n.title);

/** Opens a task's detail through the command bar, synchronously (fake-timer safe). */
function openViaPalette(text: string) {
  fireEvent.keyDown(window, { key: "k", ...mod });
  fireEvent.change(screen.getByRole("combobox"), { target: { value: text } });
  fireEvent.keyDown(screen.getByRole("combobox"), { key: "Enter" });
}

function setPlatform(platform: string) {
  Object.defineProperty(navigator, "platform", { value: platform, configurable: true });
}

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  // Back to jsdom's own (prototype) navigator.platform.
  delete (navigator as { platform?: string }).platform;
  delete document.documentElement.dataset.theme;
});

describe("App", () => {
  describe("notifications", () => {
    beforeEach(() => vi.useFakeTimers());

    it("notifies when a task off screen starts needing you, not when it is on screen and focused", () => {
      const h = setup();
      vi.spyOn(document, "hasFocus").mockReturnValue(true);
      // Home shows the first project (other): z is on screen, a is not.
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(1, "z") }));
      flushNotes();
      expect(h.env.notify).not.toHaveBeenCalled();
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(2, "a") }));
      flushNotes();
      expect(h.env.notify).toHaveBeenCalledWith({ title: "feat/a needs you", body: "goal of a" });
    });

    it("the first snapshot never notifies", () => {
      const h = makeEnv();
      render(<App env={h.env} />);
      act(() => h.load([task("n", { status: "needs_you" })]));
      flushNotes();
      expect(h.env.notify).not.toHaveBeenCalled();
    });

    it("notifies for the visible task when the window is not focused", () => {
      const h = setup();
      vi.spyOn(document, "hasFocus").mockReturnValue(false);
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(1, "z") }));
      flushNotes();
      expect(h.env.notify).toHaveBeenCalledTimes(1);
    });

    it("in the detail, only the task on screen is quiet", () => {
      const h = setup();
      vi.spyOn(document, "hasFocus").mockReturnValue(true);
      openViaPalette("goal of a");
      expect(screen.getByRole("button", { name: "Detail" })).toHaveAttribute("aria-pressed", "true");
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(1, "a") }));
      flushNotes();
      expect(h.env.notify).not.toHaveBeenCalled();
      // z is in Home's project, but Home is not on screen now.
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(2, "z") }));
      flushNotes();
      expect(titles(h)).toEqual(["feat/z needs you"]);
    });

    it("in the grid, tiles on screen are quiet and the rest notify", () => {
      const h = setup({}, [task("a", { repo_path: "/repos/shop" }), task("off", { repo_path: "/repos/shop", live: false, boot: null })]);
      vi.spyOn(document, "hasFocus").mockReturnValue(true);
      fireEvent.keyDown(window, { key: "g", ...mod });
      expect(screen.getByRole("main", { name: "Agent grid" })).toBeInTheDocument();
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(1, "a") }));
      flushNotes();
      expect(h.env.notify).not.toHaveBeenCalled();
      act(() => h.env.cockpit.dispatch({ type: "event", event: ev(2, "off", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" }) }));
      flushNotes();
      expect(titles(h)).toEqual(["feat/off needs you"]);
    });

    it("a resumed stream replaying a burst gives one summary, not one per task", () => {
      const tasks = ["a", "b", "c", "d"].map((id) => task(id, { repo_path: "/repos/shop" }));
      const h = setup({}, tasks);
      vi.spyOn(document, "hasFocus").mockReturnValue(false);
      act(() => h.env.cockpit.dispatch({ type: "disconnected", error: "gone", retryMs: 1000 }));
      act(() => h.env.cockpit.dispatch({ type: "connected", snapshot: snapshot([], 0, { resumed: true }) }));
      act(() => {
        for (const [i, id] of ["a", "b", "c"].entries()) h.env.cockpit.dispatch({ type: "event", event: needs(i + 1, id) });
      });
      expect(h.env.notify).not.toHaveBeenCalled();
      flushNotes();
      expect(h.env.notify).toHaveBeenCalledTimes(1);
      expect(h.env.notify).toHaveBeenCalledWith({ title: "3 agents need you", body: "feat/a, feat/b, feat/c" });
    });

    it("a restarted daemon (new boot_id) notifies for what now needs you, summarised past two", () => {
      const h = setup();
      vi.spyOn(document, "hasFocus").mockReturnValue(false);
      act(() => h.env.cockpit.dispatch({ type: "disconnected", error: "gone", retryMs: 1000 }));
      const back = ["a", "b", "c"].map((id) => task(id, { status: "needs_you" }));
      act(() => h.env.cockpit.dispatch({ type: "connected", snapshot: snapshot(back, 0, { boot_id: "b2" }) }));
      flushNotes();
      expect(titles(h)).toEqual(["3 agents need you"]);
    });

    it("two at once still get one notification each", () => {
      const h = setup();
      vi.spyOn(document, "hasFocus").mockReturnValue(false);
      act(() => {
        h.env.cockpit.dispatch({ type: "event", event: needs(1, "a") });
        h.env.cockpit.dispatch({ type: "event", event: needs(2, "z") });
      });
      flushNotes();
      expect(titles(h)).toEqual(["feat/a needs you", "feat/z needs you"]);
    });
  });

  it("⌘G toggles the grid, ⌘K opens the command bar, Escape leaves the detail", async () => {
    setup();
    fireEvent.keyDown(window, { key: "g", ...mod });
    expect(screen.getByRole("button", { name: /Grid/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "g", ...mod });
    expect(screen.getByRole("button", { name: /Home/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "k", ...mod });
    const input = screen.getByRole("combobox");
    await userEvent.type(input, "goal of z{Enter}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByRole("button", { name: "Detail" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.getByRole("button", { name: /Home/ })).toHaveAttribute("aria-pressed", "true");
  });

  it("Escape typed into the composer (or the terminal) stays there", async () => {
    setup();
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox"), "goal of z{Enter}");
    const box = screen.getByRole("textbox", { name: "Message to the agent" });
    fireEvent.keyDown(box, { key: "Escape" });
    expect(screen.getByRole("button", { name: "Detail" })).toHaveAttribute("aria-pressed", "true");
  });

  it("the grid toggles back to the view it came from", async () => {
    setup();
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox"), "goal of z{Enter}");
    fireEvent.keyDown(window, { key: "g", ...mod });
    expect(screen.getByRole("main", { name: "Agent grid" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "g", ...mod });
    expect(screen.getByRole("button", { name: "Detail" })).toHaveAttribute("aria-pressed", "true");
  });

  it("⇧⌘J cycles the theme and remembers it", () => {
    const h = setup();
    fireEvent.keyDown(window, { key: "J", shiftKey: true, ...mod });
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(h.env.storage.getItem(THEME_KEY)).toBe("light");
    fireEvent.keyDown(window, { key: "J", shiftKey: true, ...mod });
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("a daemon that is down gets a Start daemon button", async () => {
    const h = makeEnv();
    render(<App env={h.env} />);
    act(() => h.env.cockpit.dispatch({ type: "disconnected", error: "no socket", retryMs: 2000 }));
    expect(screen.getByRole("status", { name: "Daemon connection" })).toHaveTextContent("Daemon not reachable (no socket). Retrying in 2s.");
    await userEvent.click(screen.getByRole("button", { name: "Start daemon" }));
    expect(h.env.startDaemon).toHaveBeenCalled();
  });

  it("polls daemon.status while connected, one request at a time, and forgets it when the daemon goes away", async () => {
    vi.useFakeTimers();
    const h = setup();
    const polls = () => h.calls.filter(([m]) => m === "daemon.status").length;
    await act(async () => {});
    expect(polls()).toBe(1);
    expect(screen.getByText(/Memory guard not available/)).toBeInTheDocument();
    await act(async () => vi.advanceTimersByTimeAsync(STATUS_POLL_MS));
    expect(polls()).toBe(2);
    act(() => h.env.cockpit.dispatch({ type: "disconnected", error: "gone", retryMs: 1000 }));
    // No stale numbers while the daemon is away.
    expect(screen.getByText("Memory status unknown.")).toBeInTheDocument();
    await act(async () => vi.advanceTimersByTimeAsync(STATUS_POLL_MS * 3));
    expect(polls()).toBe(2);
  });

  it("a slow daemon.status is never stacked", async () => {
    vi.useFakeTimers();
    const h = setup({ "daemon.status": () => new Promise(() => {}) });
    await act(async () => vi.advanceTimersByTimeAsync(STATUS_POLL_MS * 4));
    expect(h.calls.filter(([m]) => m === "daemon.status")).toHaveLength(1);
  });

  it("⌘K offers Answer in terminal for a Claude task waiting while inline answers are off", async () => {
    const h = setup({}, [task("a", { status: "needs_you" }), task("x", { status: "needs_you", agent: "codex" }), task("w")]);
    await act(async () => {});
    fireEvent.keyDown(window, { key: "k", ...mod });
    const labels = screen.getAllByRole("option").map((o) => o.textContent);
    expect(labels).toContain("Answer in terminal: goal of a (@claude feat/a)");
    expect(labels).toContain("Open goal of x (@codex feat/x)");
    expect(labels).toContain("Open goal of w (@claude feat/w)");
    await userEvent.type(screen.getByRole("combobox"), "Answer in terminal{Enter}");
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(h.env.cockpit.getState().tasks.a).toBeDefined();
  });

  it("with inline answers on, the same task is just opened", async () => {
    setup({ "daemon.status": { ...STATUS, claude_inline_answers: true } }, [task("a", { status: "needs_you" })]);
    await act(async () => {});
    fireEvent.keyDown(window, { key: "k", ...mod });
    const labels = screen.getAllByRole("option").map((o) => o.textContent);
    expect(labels).toContain("Open goal of a (@claude feat/a)");
    expect(labels.some((l) => l?.startsWith("Answer in terminal"))).toBe(false);
  });

  it("off macOS, Ctrl+K inside the terminal belongs to the terminal", () => {
    setPlatform("Win32");
    setup();
    const term = document.createElement("div");
    term.className = "xterm";
    const input = document.createElement("textarea");
    term.appendChild(input);
    document.body.appendChild(term);
    const e = fireEvent.keyDown(input, { key: "k", code: "KeyK", ctrlKey: true });
    expect(e).toBe(true); // not prevented
    expect(screen.queryByRole("dialog")).toBeNull();
    term.remove();
  });

  it("on macOS, ⌘K inside the terminal still opens the command bar (the terminal has no ⌘ keys)", () => {
    setPlatform("MacIntel");
    setup();
    const term = document.createElement("div");
    term.className = "xterm";
    const input = document.createElement("textarea");
    term.appendChild(input);
    document.body.appendChild(term);
    fireEvent.keyDown(input, { key: "k", code: "KeyK", metaKey: true });
    expect(screen.getByRole("dialog", { name: "Command bar" })).toBeInTheDocument();
    // And Ctrl+K there is the terminal's.
    term.remove();
  });

  it("while the command bar is open, other shortcuts wait; auto-repeat never fires one", () => {
    setup();
    fireEvent.keyDown(window, { key: "g", ...mod, repeat: true });
    expect(screen.getByRole("button", { name: /Home/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "k", ...mod });
    fireEvent.keyDown(window, { key: "g", ...mod });
    expect(screen.getByRole("button", { name: /Home/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "k", ...mod });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("visibleTasks follows the mode", () => {
    const h = makeEnv();
    h.load([task("a"), task("b", { repo_path: "/x" })]);
    const s = h.env.cockpit.getState();
    expect([...visibleTasks(s, "home", "/x", null)]).toEqual(["b"]);
    expect([...visibleTasks(s, "detail", null, "a")]).toEqual(["a"]);
  });
});
