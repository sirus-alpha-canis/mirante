import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { Command } from "./CommandBar";
import { CommandBar } from "./CommandBar";

function setup() {
  const home = vi.fn();
  const grid = vi.fn();
  const onDispatch = vi.fn();
  const onClose = vi.fn();
  const commands = [
    { id: "home", label: "Go to Home", hint: "⌘1", run: home },
    { id: "grid", label: "Toggle agent grid", run: grid },
  ];
  render(<CommandBar commands={commands} onDispatch={onDispatch} onClose={onClose} />);
  return { home, grid, onDispatch, onClose };
}

describe("CommandBar", () => {
  it("is a labelled modal dialog with a focused combobox over a listbox", () => {
    setup();
    expect(screen.getByRole("dialog", { name: "Command bar" })).toHaveAttribute("aria-modal", "true");
    const box = screen.getByRole("combobox", { name: "Command" });
    expect(box).toHaveFocus();
    expect(box).toHaveAttribute("aria-activedescendant", "cmd-0");
    expect(screen.getAllByRole("option").map((o) => o.getAttribute("aria-selected"))).toEqual(["true", "false"]);
  });

  it("filters, moves with the arrows and runs on Enter", async () => {
    const h = setup();
    await userEvent.keyboard("{ArrowDown}{Enter}");
    expect(h.onClose).toHaveBeenCalled();
    expect(h.grid).toHaveBeenCalled();
    await userEvent.type(screen.getByRole("combobox"), "nothing like this");
    expect(screen.queryAllByRole("option")).toHaveLength(0);
    expect(screen.getByText("No matching commands")).toBeInTheDocument();
  });

  it("@ text becomes a dispatch; Escape closes", async () => {
    const h = setup();
    await userEvent.type(screen.getByRole("combobox"), "@claude shop/x fix it");
    expect(screen.getAllByRole("option")[0]).toHaveTextContent("Write in the dispatch box: @claude shop/x fix it");
    await userEvent.keyboard("{Enter}");
    expect(h.onDispatch).toHaveBeenCalledWith("@claude shop/x fix it");
    await userEvent.keyboard("{Escape}");
    expect(h.onClose).toHaveBeenCalledTimes(2);
  });

  it("option ids are positional (task ids and paths are not valid id tokens)", () => {
    const commands = [{ id: "p-/repos/my shop", label: "Project my shop", run: () => {} }];
    render(<CommandBar commands={commands} onDispatch={() => {}} onClose={() => {}} />);
    expect(screen.getByRole("option")).toHaveAttribute("id", "cmd-0");
  });

  it("the active option stays in range when the commands shrink", async () => {
    const run = vi.fn();
    const three: Command[] = ["a", "b", "c"].map((x) => ({ id: x, label: `item ${x}`, run }));
    const { rerender } = render(<CommandBar commands={three} onDispatch={() => {}} onClose={() => {}} />);
    await userEvent.keyboard("{ArrowDown}{ArrowDown}");
    rerender(<CommandBar commands={three.slice(0, 1)} onDispatch={() => {}} onClose={() => {}} />);
    const box = screen.getByRole("combobox");
    expect(box).toHaveAttribute("aria-activedescendant", "cmd-0");
    expect(screen.getByRole("option")).toHaveAttribute("aria-selected", "true");
    await userEvent.keyboard("{Enter}");
    expect(run).toHaveBeenCalledTimes(1);
  });
});
