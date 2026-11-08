import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./components/TerminalTab", () => ({ TerminalTab: () => null }));

import { App, visibleTasks } from "./App";
import { THEME_KEY } from "./lib/theme";
import { ev, task } from "./test/fixtures";
import { makeEnv } from "./test/env";

const mod = /Mac/.test(navigator.platform) ? { metaKey: true } : { ctrlKey: true };

function setup() {
  const h = makeEnv({
    "daemon.status": { pid: 1, boot_id: "b1", version: "x", live_sessions: 0 },
    "message.list_queued": [],
    "task.get": (p: Record<string, unknown>) => h.env.cockpit.getState().tasks[p.task as string],
  });
  render(<App env={h.env} />);
  act(() => h.load([task("a", { repo_path: "/repos/shop" }), task("z", { repo_path: "/repos/other" })]));
  return h;
}

afterEach(() => {
  vi.restoreAllMocks();
  delete document.documentElement.dataset.theme;
});

describe("App", () => {
  it("notifies when a task off screen starts needing you, not when it is on screen and focused", () => {
    const h = setup();
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    // Home shows the first project (other): z is on screen, a is not.
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(1, "z", "status", { to: "needs_you" }) }));
    expect(h.env.notify).not.toHaveBeenCalled();
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(2, "a", "status", { to: "needs_you" }) }));
    expect(h.env.notify).toHaveBeenCalledWith(expect.objectContaining({ id: "a" }));
  });

  it("the first snapshot never notifies", () => {
    const h = makeEnv();
    render(<App env={h.env} />);
    act(() => h.load([task("n", { status: "needs_you" })]));
    expect(h.env.notify).not.toHaveBeenCalled();
  });

  it("notifies for the visible task when the window is not focused", () => {
    const h = setup();
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(1, "z", "status", { to: "needs_you" }) }));
    expect(h.env.notify).toHaveBeenCalledTimes(1);
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

  it("polls daemon.status while connected", () => {
    const h = setup();
    expect(h.calls.filter(([m]) => m === "daemon.status").length).toBeGreaterThanOrEqual(1);
  });

  it("visibleTasks follows the mode", () => {
    const h = makeEnv();
    h.load([task("a"), task("b", { repo_path: "/x" })]);
    const s = h.env.cockpit.getState();
    expect([...visibleTasks(s, "home", "/x", null)]).toEqual(["b"]);
    expect([...visibleTasks(s, "detail", null, "a")]).toEqual(["a"]);
  });
});
