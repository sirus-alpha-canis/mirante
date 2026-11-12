import { act, fireEvent, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { onMac } from "../lib/shortcuts";
import { makeEnv, renderWith } from "../test/env";
import { QuickOpen } from "./QuickOpen";

const mod = () => (onMac() ? { metaKey: true } : { ctrlKey: true });

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

function mount(files: (p: Record<string, unknown>) => string[]) {
  const h = makeEnv({ "task.files": files });
  const onOpen = vi.fn();
  const onOpenEditor = vi.fn();
  const onClose = vi.fn();
  const r = renderWith(h.env, <QuickOpen taskId="t1" onOpen={onOpen} onOpenEditor={onOpenEditor} onClose={onClose} />);
  return { ...h, ...r, onOpen, onOpenEditor, onClose };
}

const settle = () => act(async () => void (await vi.advanceTimersByTimeAsync(300)));

describe("QuickOpen", () => {
  it("debounces task.files and lists ranked paths, secrets flagged", async () => {
    const h = mount((p) => (String(p.query) === "log" ? ["src/login.ts", "config/.env.login"] : ["README.md"]));
    await settle();
    expect(h.calls).toEqual([["task.files", { task: "t1", query: "", limit: 50 }]]);
    const input = screen.getByRole("combobox", { name: "File name" });
    fireEvent.change(input, { target: { value: "l" } });
    fireEvent.change(input, { target: { value: "lo" } });
    fireEvent.change(input, { target: { value: "log" } });
    await settle();
    expect(h.calls.map(([, p]) => p.query)).toEqual(["", "log"]);
    const options = screen.getAllByRole("option");
    expect(options.map((o) => o.textContent)).toEqual(["src/login.ts", "config/.env.login⚠ may hold secrets"]);
    expect(options[0]).toHaveAttribute("aria-selected", "true");
  });

  it("↵ opens in the pane, ⌘↵ opens in editor", async () => {
    const h = mount(() => ["a.ts", "b.ts"]);
    await settle();
    const input = screen.getByRole("combobox", { name: "File name" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(h.onOpen).toHaveBeenCalledWith("b.ts");
    expect(h.onClose).toHaveBeenCalled();
    fireEvent.keyDown(input, { key: "Enter", ...mod() });
    expect(h.onOpenEditor).toHaveBeenCalledWith("b.ts");
  });

  it("a click opens in the pane; a failed listing says why", async () => {
    const h = mount(() => {
      throw new Error("x");
    });
    await settle();
    expect(screen.getByRole("alert")).toHaveTextContent("Could not list the files");
    expect(h.onOpen).not.toHaveBeenCalled();
  });

  it("Escape closes and focus returns", async () => {
    vi.useRealTimers();
    const before = document.createElement("button");
    document.body.appendChild(before);
    before.focus();
    const h = mount(() => ["a.ts"]);
    const input = screen.getByRole("combobox", { name: "File name" });
    expect(input).toHaveFocus();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(h.onClose).toHaveBeenCalled();
    h.unmount();
    expect(before).toHaveFocus();
    before.remove();
  });
});
