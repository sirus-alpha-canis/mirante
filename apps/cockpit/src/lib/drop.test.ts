import { afterEach, describe, expect, it, vi } from "vitest";
import { addDropTarget, type DropTarget, focusedTarget, handleDragDrop } from "./drop";

const cleanups: (() => void)[] = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});

function target(parent: HTMLElement = document.body) {
  const el = document.createElement("div");
  const input = document.createElement("textarea");
  el.appendChild(input);
  parent.appendChild(el);
  const t: DropTarget & { dropped: string[][]; hovers: boolean[] } = {
    dropped: [],
    hovers: [],
    el: () => el,
    drop: (p) => t.dropped.push(p),
    hover: (on) => t.hovers.push(on),
  };
  cleanups.push(addDropTarget(t));
  return { t, el, input };
}

const at = { x: 20, y: 20 };

describe("drop routing", () => {
  it("the target under the cursor takes the drop (physical pixels divided by the scale)", () => {
    const a = target();
    const b = target();
    Object.defineProperty(window, "devicePixelRatio", { value: 2, configurable: true });
    const point = vi.fn((_x: number, _y: number) => b.input as Element);
    document.elementFromPoint = point;
    handleDragDrop({ type: "enter", paths: ["/x/a.png"], position: at });
    expect(b.t.hovers).toEqual([true]);
    handleDragDrop({ type: "drop", paths: ["/x/a.png"], position: at });
    expect(point).toHaveBeenCalledWith(10, 10);
    expect(b.t.dropped).toEqual([["/x/a.png"]]);
    expect(b.t.hovers).toEqual([true, false]);
    expect(a.t.dropped).toEqual([]);
  });

  it("the innermost target wins (a terminal inside a panel that is a target too)", () => {
    const outer = target();
    const inner = target(outer.el);
    document.elementFromPoint = () => inner.input;
    handleDragDrop({ type: "drop", paths: ["/x/a"], position: at });
    expect(inner.t.dropped).toHaveLength(1);
    expect(outer.t.dropped).toHaveLength(0);
  });

  it("off every target: the focused one, else the last focused, else the only one", () => {
    document.elementFromPoint = () => document.body;
    const a = target();
    handleDragDrop({ type: "drop", paths: ["/1"], position: at });
    expect(a.t.dropped).toEqual([["/1"]]);
    const b = target();
    handleDragDrop({ type: "drop", paths: ["/2"], position: at });
    expect(a.t.dropped).toHaveLength(1);
    expect(b.t.dropped).toHaveLength(0);
    focusedTarget(b.t);
    handleDragDrop({ type: "drop", paths: ["/3"], position: at });
    expect(b.t.dropped).toEqual([["/3"]]);
    a.input.focus();
    handleDragDrop({ type: "drop", paths: ["/4"], position: at });
    expect(a.t.dropped).toEqual([["/1"], ["/4"]]);
  });

  it("leaving the window clears the highlight; nothing is dropped", () => {
    const a = target();
    document.elementFromPoint = () => a.input;
    handleDragDrop({ type: "over", position: at });
    handleDragDrop({ type: "leave" });
    expect(a.t.hovers).toEqual([true, false]);
    expect(a.t.dropped).toEqual([]);
  });
});
