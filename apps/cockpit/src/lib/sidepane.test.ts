import { describe, expect, it } from "vitest";
import { clampWidth, DEFAULT_PANE, loadPane, PANE_INDEX, PANE_LRU, paneKey, savePane } from "./sidepane";

const store = () => {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m };
};
const alive = () => true;

describe("side pane memory", () => {
  it("saves open/width/target per task, LRU 200", () => {
    const s = store();
    savePane(s, "t1", { open: true, width: 45, target: { k: "file", path: "src/a.rs", line: 3 }, swapped: false });
    savePane(s, "t2", { ...DEFAULT_PANE, target: { k: "diff" } });
    expect(loadPane(s, "t1", alive)).toEqual({ open: true, width: 45, target: { k: "file", path: "src/a.rs", line: 3 }, swapped: false });
    expect(loadPane(s, "t2", alive).target).toEqual({ k: "diff" });
    expect(loadPane(s, "t3", alive)).toEqual(DEFAULT_PANE);
    for (let i = 0; i < PANE_LRU + 5; i++) savePane(s, `x${i}`, { ...DEFAULT_PANE, open: true, target: { k: "steps" } });
    const index = JSON.parse(s.getItem(PANE_INDEX)!) as string[];
    expect(index).toHaveLength(PANE_LRU);
    expect(index.at(-1)).toBe(`x${PANE_LRU + 4}`);
    // The oldest are forgotten.
    expect(loadPane(s, "t1", alive)).toEqual(DEFAULT_PANE);
    expect(loadPane(s, `x${PANE_LRU + 4}`, alive).open).toBe(true);
  });

  it("width clamps to 25..70%", () => {
    expect(clampWidth(10)).toBe(25);
    expect(clampWidth(90)).toBe(70);
    expect(clampWidth(Number.NaN)).toBe(DEFAULT_PANE.width);
    const s = store();
    s.setItem(paneKey("t"), JSON.stringify({ open: true, width: 99, target: { k: "diff" } }));
    expect(loadPane(s, "t", alive).width).toBe(70);
  });

  it("a target of an archived/missing task falls back to closed", () => {
    const s = store();
    savePane(s, "t1", { open: true, width: 40, target: { k: "terminal", task: "gone" }, swapped: false });
    expect(loadPane(s, "t1", (id) => id !== "gone")).toEqual({ ...DEFAULT_PANE, width: 40 });
    // The same task's terminal is never a pane target (it stays the main view).
    savePane(s, "t1", { open: true, width: 40, target: { k: "terminal", task: "t1" }, swapped: false });
    expect(loadPane(s, "t1", alive).target).toBeNull();
  });

  it("garbage in storage is the default", () => {
    const s = store();
    for (const bad of ["{", "null", "[]", '{"open":true,"target":{"k":"file"}}', '{"open":true,"target":{"k":"evil","x":1}}']) {
      s.setItem(paneKey("t"), bad);
      const p = loadPane(s, "t", alive);
      expect(p.target, bad).toBeNull();
      expect(p.open, bad).toBe(false);
    }
  });

  it("a Diff target may name another task (a sibling's, beside this one); gone or bad, it is dropped", () => {
    const s = store();
    savePane(s, "t1", { open: true, width: 40, target: { k: "diff", task: "t2" }, swapped: false });
    expect(loadPane(s, "t1", alive).target).toEqual({ k: "diff", task: "t2" });
    expect(loadPane(s, "t1", (id) => id !== "t2").target).toBeNull();
    savePane(s, "t1", { open: true, width: 40, target: { k: "diff", task: "t1" }, swapped: false });
    expect(loadPane(s, "t1", alive).target).toEqual({ k: "diff" });
    s.setItem(paneKey("t"), JSON.stringify({ open: true, target: { k: "diff", task: 5 } }));
    expect(loadPane(s, "t", alive).target).toEqual({ k: "diff" });
  });
});
