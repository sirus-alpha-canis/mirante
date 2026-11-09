import { act, fireEvent, render, screen, within } from "@testing-library/react";
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
  fireEvent.change(screen.getByRole("combobox", { name: "Command" }), { target: { value: text } });
  fireEvent.keyDown(screen.getByRole("combobox", { name: "Command" }), { key: "Enter" });
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
      const h = setup({}, [task("a", { repo_path: "/repos/shop", status: "working" }), task("z", { repo_path: "/repos/other", status: "working" })]);
      vi.spyOn(document, "hasFocus").mockReturnValue(true);
      // Home is an inbox of every project: z's row is on screen there.
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(1, "z") }));
      flushNotes();
      expect(h.env.notify).not.toHaveBeenCalled();
      // In z's detail, a is off screen.
      openViaPalette("goal of z");
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(2, "a") }));
      flushNotes();
      expect(h.env.notify).toHaveBeenCalledWith({ title: "feat/a needs you", body: "goal of a", taskId: "a" });
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

    it("on Home, a task folded away (idle) or a hidden window is not on screen", () => {
      const h = setup({}, [task("w", { status: "working" }), task("i", { status: "idle" })]);
      vi.spyOn(document, "hasFocus").mockReturnValue(true);
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(1, "w") }));
      flushNotes();
      expect(h.env.notify).not.toHaveBeenCalled();
      // "Parados" is folded: its task is not on screen.
      act(() => h.env.cockpit.dispatch({ type: "event", event: needs(2, "i") }));
      flushNotes();
      expect(titles(h)).toEqual(["feat/i needs you"]);
      // A focused but hidden document (another space, minimized) is not looking either.
      Object.defineProperty(document, "hidden", { value: true, configurable: true });
      try {
        act(() => h.env.cockpit.dispatch({ type: "event", event: ev(3, "w", "status", { to: "working" }) }));
        act(() => h.env.cockpit.dispatch({ type: "event", event: needs(4, "w") }));
        flushNotes();
        expect(titles(h)).toEqual(["feat/i needs you", "feat/w needs you"]);
      } finally {
        delete (document as { hidden?: boolean }).hidden;
      }
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
    const input = screen.getByRole("combobox", { name: "Command" });
    await userEvent.type(input, "goal of z{Enter}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByRole("button", { name: "Detail" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.getByRole("button", { name: /Home/ })).toHaveAttribute("aria-pressed", "true");
  });

  it("Escape typed into the composer (or the terminal) stays there", async () => {
    setup();
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox", { name: "Command" }), "goal of z{Enter}");
    const box = screen.getByRole("textbox", { name: "Message to the agent" });
    fireEvent.keyDown(box, { key: "Escape" });
    expect(screen.getByRole("button", { name: "Detail" })).toHaveAttribute("aria-pressed", "true");
  });

  it("the grid toggles back to the view it came from", async () => {
    setup();
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox", { name: "Command" }), "goal of z{Enter}");
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
    expect(screen.getByText("System memory status unknown.")).toBeInTheDocument();
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
    await userEvent.type(screen.getByRole("combobox", { name: "Command" }), "Answer in terminal{Enter}");
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

  it("view chords wait while a field is being typed in; ⌘K still opens the command bar", () => {
    setup();
    const box = screen.getByRole("textbox", { name: "Goal" });
    for (const key of ["b", "g"]) {
      fireEvent.keyDown(box, { key, code: `Key${key.toUpperCase()}`, ...mod });
      expect(screen.getByRole("button", { name: /Home/ })).toHaveAttribute("aria-pressed", "true");
    }
    fireEvent.keyDown(box, { key: "k", code: "KeyK", ...mod });
    expect(screen.getByRole("dialog", { name: "Command bar" })).toBeInTheDocument();
  });

  it("⌘J opens the task that has waited longest, then the next (Claude on its Terminal tab); the button says i of n", async () => {
    const h = setup({}, [
      task("a", { status: "needs_you", updated_at: 5, agent: "codex" }),
      task("b", { status: "needs_you", updated_at: 1 }),
      task("w", { status: "working" }),
    ]);
    const next = screen.getByRole("button", { name: /^Next/ });
    expect(next).toHaveTextContent("– of 2");
    fireEvent.keyDown(window, { key: "j", code: "KeyJ", ...mod });
    expect(screen.getByRole("main", { name: "Task goal of b" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("button", { name: /^Next/ })).toHaveTextContent("1 of 2");
    await userEvent.click(screen.getByRole("button", { name: /^Next/ }));
    expect(screen.getByRole("main", { name: "Task goal of a" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Next/ })).toHaveTextContent("2 of 2");
    // Back to the first.
    fireEvent.keyDown(window, { key: "j", code: "KeyJ", ...mod });
    expect(screen.getByRole("main", { name: "Task goal of b" })).toBeInTheDocument();
    void h;
  });

  it("⌘J waits while a form field is being typed in; with nothing waiting, the button is off", () => {
    setup({}, [task("w", { status: "working" })]);
    expect(screen.getByRole("button", { name: /^Next/ })).toBeDisabled();
    const goal = screen.getByLabelText("Goal");
    goal.focus();
    fireEvent.keyDown(goal, { key: "j", code: "KeyJ", ...mod });
    expect(screen.getByRole("main", { name: "Inbox" })).toBeInTheDocument();
  });

  it("a remembered detail whose task is gone opens Home instead", () => {
    const h = makeEnv({ "daemon.status": STATUS, "message.list_queued": [] });
    h.env.storage.setItem("mirante.ui", JSON.stringify({ mode: "detail", taskId: "deleted", project: null, tab: "Diff", scroll: 0 }));
    render(<App env={h.env} />);
    act(() => h.load([task("a")]));
    expect(screen.getByRole("main", { name: "Inbox" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Detail" })).toBeDisabled();
  });

  it("Next's tooltip says its shortcut and that it waits while typing", () => {
    setup({}, [task("n", { status: "needs_you" })]);
    const title = screen.getByRole("button", { name: /^Next/ }).getAttribute("title") ?? "";
    expect(title).toMatch(/[⌘C].*J/);
    expect(title).toMatch(/not while typing/i);
  });

  it("reopens where the person was: the view, the task and its tab", async () => {
    const h = makeEnv({ "daemon.status": STATUS, "message.list_queued": [], "task.diff": { base: "main", merge_base: "x", files: [], untracked: [], patch: "", truncated: false, compare_url: null, pushed: false } });
    h.load([task("a"), task("b")]);
    const { unmount } = render(<App env={h.env} />);
    await userEvent.click(screen.getByRole("button", { name: "goal of b" }));
    await userEvent.click(screen.getByRole("tab", { name: "Diff" }));
    unmount();
    render(<App env={h.env} />);
    expect(screen.getByRole("main", { name: "Task goal of b" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Diff" })).toHaveAttribute("aria-selected", "true");
  });

  it("visibleTasks follows the mode", () => {
    const h = makeEnv();
    h.load([task("a"), task("b", { repo_path: "/x" })]);
    const s = h.env.cockpit.getState();
    // Home is an inbox of every project: the ones whose card or row is on screen.
    expect([...visibleTasks(s, "home", "/x", null, () => true)].sort()).toEqual(["a", "b"]);
    expect([...visibleTasks(s, "home", "/x", null, (id) => id === "b")]).toEqual(["b"]);
    expect([...visibleTasks(s, "detail", null, "a")]).toEqual(["a"]);
    expect([...visibleTasks(s, "brain", "/x", "a")]).toEqual([]);
    expect([...visibleTasks(s, "skills", "/x", "a")]).toEqual([]);
  });

  it("the Skills view opens from the nav and the command bar", async () => {
    setup({ "skill.tenants": [], "skill.list": [], "skill.proposals": [] });
    const nav = () => within(screen.getByRole("navigation", { name: "Views" })).getByRole("button", { name: /^Skills/ });
    await userEvent.click(nav());
    expect(await screen.findByRole("main", { name: "Skills" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /Home/ }));
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox", { name: "Command" }), "Go to Skills{Enter}");
    expect(nav()).toHaveAttribute("aria-pressed", "true");
    expect(await screen.findByRole("main", { name: "Skills" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox", { name: "Command" }), "Review skills{Enter}");
    expect(await screen.findByRole("region", { name: "Skill review queue" })).toBeInTheDocument();
  });

  it("⌘B toggles the brain and back; the command bar opens it and the review queue", async () => {
    setup({ "dna.status": { repo: "/repos/other", enabled: true, dir: true, notes: 0, stale: 0, unreviewed: 0, pending: 1, invalid: 0, head: null }, "dna.graph": { nodes: [], links: [] }, "dna.proposals": [] });
    fireEvent.keyDown(window, { key: "b", ...mod });
    expect(screen.getByRole("button", { name: /Brain/ })).toHaveAttribute("aria-pressed", "true");
    expect(await screen.findByRole("main", { name: "Brain" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "b", ...mod });
    expect(screen.getByRole("button", { name: /Home/ })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox", { name: "Command" }), "Review memory{Enter}");
    expect(await screen.findByRole("region", { name: "Memory review queue" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox", { name: "Command" }), "Go to Brain{Enter}");
    expect(screen.getByRole("button", { name: /Brain/ })).toHaveAttribute("aria-pressed", "true");
  });

  const DNA = {
    "dna.status": (p: Record<string, unknown>) => ({ repo: p.repo, enabled: true, dir: true, notes: 0, stale: 0, unreviewed: 0, pending: 1, invalid: 0, head: null }),
    "dna.graph": { nodes: [], links: [] },
    "dna.proposals": [],
  };

  it("Review memory from inside the brain switches its panel without starting it over", async () => {
    const h = setup(DNA);
    fireEvent.keyDown(window, { key: "b", ...mod });
    await userEvent.type(await screen.findByRole("searchbox", { name: "Search memory" }), "kept");
    const graphs = () => h.calls.filter((c) => c[0] === "dna.graph").length;
    const n = graphs();
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox", { name: "Command" }), "Review memory{Enter}");
    expect(await screen.findByRole("region", { name: "Memory review queue" })).toBeInTheDocument();
    expect(screen.getByRole("searchbox", { name: "Search memory" })).toHaveValue("kept");
    expect(graphs()).toBe(n);
  });

  it("the brain opened from a task's detail shows that task's repo", async () => {
    setup(DNA);
    // Home starts on the first project (other); the task a lives in /repos/shop.
    fireEvent.keyDown(window, { key: "k", ...mod });
    await userEvent.type(screen.getByRole("combobox", { name: "Command" }), "goal of a{Enter}");
    expect(screen.getByRole("button", { name: "Detail" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "b", ...mod });
    expect(await screen.findByRole("heading", { name: "Memory of shop" })).toBeInTheDocument();
  });
  describe("coming back to Home", () => {
    it("Home → Detail → Home keeps where the list was scrolled", async () => {
      vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
      setup({}, [task("a", { status: "working" }), task("b", { status: "working" })]);
      const list = () => screen.getByRole("main", { name: "Inbox" }).querySelector(".cards") as HTMLElement;
      list().scrollTop = 140;
      fireEvent.scroll(list());
      await act(async () => new Promise<void>((r) => requestAnimationFrame(() => r())));
      openViaPalette("goal of a");
      expect(screen.getByRole("main", { name: "Task goal of a" })).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "Escape" });
      expect(list().scrollTop).toBe(140);
    });

    it("the dispatch form keeps its attached context and edited branch across a visit elsewhere", async () => {
      setup({ "repo.files": ["src/login.ts"] });
      await userEvent.type(screen.getByLabelText("Goal"), "Fix the login");
      await userEvent.click(screen.getByRole("button", { name: "Edit branch" }));
      const name = screen.getByLabelText("Branch");
      await userEvent.clear(name);
      await userEvent.type(name, "feat/mine");
      await userEvent.click(screen.getByRole("button", { name: "+ context" }));
      const dialog = screen.getByRole("dialog", { name: "Add context" });
      await userEvent.click(await within(dialog).findByRole("button", { name: /^src\/login\.ts/ }));
      await userEvent.keyboard("{Escape}");
      await userEvent.click(screen.getByRole("button", { name: /^Grid/ }));
      await userEvent.click(screen.getByRole("button", { name: /^Home/ }));
      expect(screen.getByLabelText("Goal")).toHaveValue("Fix the login");
      expect(screen.getByLabelText("Branch")).toHaveValue("feat/mine");
      expect(within(screen.getByRole("list", { name: "Attached context" })).getByText("src/login.ts")).toBeInTheDocument();
    });

    it("Escape goes back to the view the detail was opened from, and Home focuses the card it came from", async () => {
      setup({}, [task("a", { status: "working" }), task("z", { repo_path: "/repos/other", status: "working" })]);
      await userEvent.click(screen.getByRole("button", { name: "goal of a" }));
      expect(screen.getByRole("main", { name: "Task goal of a" })).toBeInTheDocument();
      fireEvent.keyDown(window, { key: "Escape" });
      expect(screen.getByRole("main", { name: "Inbox" })).toBeInTheDocument();
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "goal of a" }));
      fireEvent.keyDown(window, { key: "g", ...mod });
      openViaPalette("goal of z");
      fireEvent.keyDown(window, { key: "Escape" });
      expect(screen.getByRole("main", { name: "Agent grid" })).toBeInTheDocument();
    });
  });

  it("a remembered project that is no longer listed falls back to the first one", () => {
    const h = makeEnv({ "daemon.status": STATUS, "message.list_queued": [] });
    h.env.storage.setItem("mirante.ui", JSON.stringify({ mode: "home", taskId: null, project: "/repos/removed", tab: null, scroll: 0 }));
    h.env.storage.setItem("mirante.repos", JSON.stringify(["/repos/shop"]));
    render(<App env={h.env} />);
    act(() => h.load([task("a")]));
    expect(screen.getByRole("button", { name: "shop" })).toHaveAttribute("aria-current", "page");
  });

  it("starts on Home with storage that throws on every call", () => {
    const h = makeEnv({ "daemon.status": STATUS, "message.list_queued": [] });
    const boom = () => {
      throw new Error("SecurityError");
    };
    h.env.storage = { getItem: boom, setItem: boom };
    render(<App env={h.env} />);
    act(() => h.load([task("a")]));
    expect(screen.getByRole("main", { name: "Inbox" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "J", shiftKey: true, ...mod });
    expect(document.documentElement.dataset.theme).toBe("light");
  });

  it("⌘L opens Skills, and the command bar shows its chord", async () => {
    setup({ "skill.tenants": [], "skill.list": [], "skill.proposals": [] });
    fireEvent.keyDown(window, { key: "l", code: "KeyL", ...mod });
    expect(await screen.findByRole("main", { name: "Skills" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "k", ...mod });
    const option = screen.getAllByRole("option").find((o) => o.textContent?.startsWith("Go to Skills"));
    expect(option?.querySelector("kbd")?.textContent).toMatch(/L$/);
  });

  it("clicking a task's notification opens it", () => {
    let click: ((taskId: string) => void) | null = null;
    const h2 = makeEnv({ "daemon.status": STATUS, "message.list_queued": [] });
    h2.env.onNoteClick = (fn) => {
      click = fn;
      return () => {
        click = null;
      };
    };
    render(<App env={h2.env} />);
    act(() => h2.load([task("a", { status: "needs_you", agent: "codex" })]));
    act(() => click!("a"));
    expect(screen.getByRole("main", { name: "Task goal of a" })).toBeInTheDocument();
  });

  describe("launch", () => {
    afterEach(() => {
      document.getElementById("splash")?.remove();
      delete (window as { matchMedia?: unknown }).matchMedia;
    });

    it("the launch splash in index.html goes once the app has mounted", () => {
      const splash = document.createElement("div");
      splash.id = "splash";
      document.body.append(splash);
      // jsdom has no matchMedia: the person asked for reduced motion.
      Object.defineProperty(window, "matchMedia", { configurable: true, value: (q: string) => ({ matches: q.includes("reduce") }) as MediaQueryList });
      render(<App env={makeEnv().env} />);
      // Reduced motion: removed at once (with motion it fades first, see splash.test.ts).
      expect(document.getElementById("splash")).toBeNull();
    });

    it("until the daemon first answers, the orbiting dots say it is connecting; then the view", () => {
      const h = makeEnv({ "daemon.status": STATUS });
      render(<App env={h.env} />);
      const waiting = screen.getByRole("status", { name: "Connecting to the Mirante daemon…" });
      expect(waiting).toHaveAttribute("aria-live", "polite");
      expect(waiting).toHaveTextContent("Connecting to the Mirante daemon…");
      expect(waiting.querySelector("svg .orb-sats")).not.toBeNull();
      // One message, not the banner too.
      expect(screen.queryByRole("status", { name: "Daemon connection" })).toBeNull();
      act(() => h.load([task("a", { repo_path: "/repos/shop" })]));
      expect(screen.queryByRole("status", { name: "Connecting to the Mirante daemon…" })).toBeNull();
      expect(screen.getByLabelText("Goal")).toBeInTheDocument();
    });

    it("in Portuguese too", async () => {
      const h = makeEnv();
      h.env.storage.setItem("mirante.lang", "pt");
      render(<App env={h.env} />);
      expect(screen.getByRole("status", { name: "Conectando ao daemon do Mirante…" })).toBeInTheDocument();
    });

    it("a reconnect after the first answer keeps the view and shows the banner", async () => {
      const h = setup();
      await act(async () => {});
      act(() => h.env.cockpit.dispatch({ type: "disconnected", error: "no socket", retryMs: 3000 }));
      expect(screen.queryByRole("status", { name: "Connecting to the Mirante daemon…" })).toBeNull();
      expect(screen.getByLabelText("Goal")).toBeInTheDocument();
    });
  });

  it("while the daemon is down, dispatch is off and the banner counts down", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"] });
    const h = setup();
    await act(async () => {});
    act(() => h.env.cockpit.dispatch({ type: "disconnected", error: "no socket", retryMs: 3000 }));
    const banner = screen.getByRole("status", { name: "Daemon connection" });
    expect(banner).toHaveTextContent("Retrying in 3s");
    act(() => vi.advanceTimersByTime(1000));
    expect(banner).toHaveTextContent("Retrying in 2s");
    fireEvent.change(screen.getByLabelText("Goal"), { target: { value: "do it" } });
    expect(screen.getByRole("button", { name: "Dispatch" })).toBeDisabled();
  });

  describe("the brand is Home", () => {
    it("the Orb-Dot mark and the wordmark are one Home button, pressed on Home; there is no separate Home tab", async () => {
      setup();
      const home = screen.getByRole("button", { name: "Home" });
      expect(home).toHaveAttribute("aria-pressed", "true");
      expect(home).toHaveTextContent("Mirante");
      // The launch splash's mark, still (no orbiting satellites in the bar).
      expect(home.querySelector("svg.orb.orb-static")).not.toBeNull();
      expect(within(screen.getByRole("navigation", { name: "Views" })).queryByRole("button", { name: /Home/ })).toBeNull();
      expect(screen.queryByRole("button", { name: /Home ⌘1|Home Ctrl\+1/ })).toBeNull();
      fireEvent.keyDown(window, { key: "g", ...mod });
      expect(home).toHaveAttribute("aria-pressed", "false");
      await userEvent.click(home);
      expect(home).toHaveAttribute("aria-pressed", "true");
      // ⌘1 still goes Home.
      fireEvent.keyDown(window, { key: "g", ...mod });
      fireEvent.keyDown(window, { key: "1", code: "Digit1", ...mod });
      expect(home).toHaveAttribute("aria-pressed", "true");
    });

    it("says Início in Portuguese", async () => {
      setup();
      await userEvent.selectOptions(screen.getByRole("combobox", { name: "Language" }), "pt");
      expect(screen.getByRole("button", { name: "Início" })).toHaveAttribute("aria-pressed", "true");
    });
  });

  describe("an outdated daemon", () => {
    const disk = { version: "0.1.0", build: "new.2" };

    it("a daemon older than the binary on disk gets a restart banner; Restart daemon restarts it", async () => {
      const h = setup({ "daemon.status": { ...STATUS, build: "old.1", auto_resume: true } });
      vi.mocked(h.env.daemonBinaryBuild).mockResolvedValue(disk);
      // The check runs once the daemon answers.
      act(() => h.env.cockpit.dispatch({ type: "disconnected", error: "x", retryMs: 1 }));
      act(() => h.load([task("a")]));
      const banner = await screen.findByRole("status", { name: "Mirante update" });
      expect(banner).toHaveTextContent("Mirante was updated — restart the daemon to use the new version (your sessions come back automatically).");
      await userEvent.click(within(banner).getByRole("button", { name: "Restart daemon" }));
      expect(h.env.restartDaemon).toHaveBeenCalledTimes(1);
    });

    it("a daemon that predates build ids is outdated too; with auto-resume off the banner says sessions end", async () => {
      const h = makeEnv({ "daemon.status": { ...STATUS, auto_resume: false }, "message.list_queued": [] });
      vi.mocked(h.env.daemonBinaryBuild).mockResolvedValue(disk);
      render(<App env={h.env} />);
      act(() => h.load([task("a")]));
      const banner = await screen.findByRole("status", { name: "Mirante update" });
      expect(banner).toHaveTextContent("live sessions end; resume them from their cards");
    });

    it("the same build, or a binary that cannot tell, shows no banner", async () => {
      const h = makeEnv({ "daemon.status": { ...STATUS, build: "new.2" }, "message.list_queued": [] });
      vi.mocked(h.env.daemonBinaryBuild).mockResolvedValue(disk);
      render(<App env={h.env} />);
      act(() => h.load([task("a")]));
      await act(async () => {});
      await act(async () => {});
      expect(h.env.daemonBinaryBuild).toHaveBeenCalled();
      expect(screen.queryByRole("status", { name: "Mirante update" })).toBeNull();
    });

    it("a failed restart says why, and the button comes back", async () => {
      const h = makeEnv({ "daemon.status": { ...STATUS, build: "old.1" }, "message.list_queued": [] });
      vi.mocked(h.env.daemonBinaryBuild).mockResolvedValue(disk);
      vi.mocked(h.env.restartDaemon).mockRejectedValue(new Error("the old daemon is still running"));
      render(<App env={h.env} />);
      act(() => h.load([task("a")]));
      const banner = await screen.findByRole("status", { name: "Mirante update" });
      await userEvent.click(within(banner).getByRole("button", { name: "Restart daemon" }));
      expect(await within(banner).findByText(/Could not restart: the old daemon is still running/)).toBeInTheDocument();
      expect(within(banner).getByRole("button", { name: "Restart daemon" })).toBeEnabled();
    });
  });

  describe("language", () => {
    it("switches the interface to Portuguese and back, remembered, with <html lang>", async () => {
      const h = setup({}, [task("n", { status: "needs_you" })]);
      expect(document.documentElement.lang).toBe("en");
      expect(screen.getByRole("heading", { name: /^Needs you/ })).toBeInTheDocument();
      await userEvent.selectOptions(screen.getByRole("combobox", { name: "Language" }), "pt");
      expect(document.documentElement.lang).toBe("pt");
      expect(h.env.storage.getItem("mirante.lang")).toBe("pt");
      expect(screen.getByRole("heading", { name: /^Precisa de você/ })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /^Próximo/ })).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Despachar" })).toBeInTheDocument();
      await userEvent.selectOptions(screen.getByRole("combobox", { name: "Idioma" }), "en");
      expect(screen.getByRole("heading", { name: /^Needs you/ })).toBeInTheDocument();
    });

    it("starts in the remembered language, else the system's", () => {
      const h = makeEnv({ "daemon.status": STATUS, "message.list_queued": [] });
      h.env.storage.setItem("mirante.lang", "pt");
      render(<App env={h.env} />);
      expect(document.documentElement.lang).toBe("pt");
      expect(screen.getByRole("button", { name: /^Comandos/ })).toBeInTheDocument();
    });
  });
});
