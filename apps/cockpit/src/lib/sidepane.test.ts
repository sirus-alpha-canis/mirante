import { describe, expect, it } from "vitest";
import { activeTarget, clampRatio, clampWidth, closeTab, DEFAULT_PANE, focusTab, freeShell, loadPane, openTab, PANE_INDEX, retarget, PANE_LRU, PANE_TABS, paneKey, type PaneState, type PaneTarget, savePane, toggleSplit } from "./sidepane";

const store = () => {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m };
};
const alive = () => true;
/** A pane showing one target (the shape saved before tabs, as the model now has it). */
const one = (open: boolean, width: number, target: PaneTarget): PaneState => ({ ...DEFAULT_PANE, open, width, tabs: [{ id: "t1", t: target }], active: "t1" });

describe("side pane memory", () => {
  it("saves open/width/target per task, LRU 200", () => {
    const s = store();
    savePane(s, "t1", one(true, 45, { k: "file", path: "src/a.rs", line: 3 }));
    savePane(s, "t2", one(false, 40, { k: "diff" }));
    expect(loadPane(s, "t1", alive)).toEqual(one(true, 45, { k: "file", path: "src/a.rs", line: 3 }));
    expect(activeTarget(loadPane(s, "t2", alive))).toEqual({ k: "diff" });
    expect(loadPane(s, "t3", alive)).toEqual(DEFAULT_PANE);
    for (let i = 0; i < PANE_LRU + 5; i++) savePane(s, `x${i}`, one(true, 40, { k: "steps" }));
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
    savePane(s, "t1", one(true, 40, { k: "terminal", task: "gone" }));
    expect(loadPane(s, "t1", (id) => id !== "gone")).toEqual({ ...DEFAULT_PANE, width: 40 });
    // The same task's terminal is never a pane target (it stays the main view).
    savePane(s, "t1", one(true, 40, { k: "terminal", task: "t1" }));
    expect(activeTarget(loadPane(s, "t1", alive))).toBeNull();
  });

  it("garbage in storage is the default", () => {
    const s = store();
    for (const bad of ["{", "null", "[]", '{"open":true,"target":{"k":"file"}}', '{"open":true,"target":{"k":"evil","x":1}}']) {
      s.setItem(paneKey("t"), bad);
      const p = loadPane(s, "t", alive);
      expect(activeTarget(p), bad).toBeNull();
      expect(p.open, bad).toBe(false);
    }
  });

  it("a Diff target may name another task (a sibling's, beside this one); gone or bad, it is dropped", () => {
    const s = store();
    savePane(s, "t1", one(true, 40, { k: "diff", task: "t2" }));
    expect(activeTarget(loadPane(s, "t1", alive))).toEqual({ k: "diff", task: "t2" });
    expect(activeTarget(loadPane(s, "t1", (id) => id !== "t2"))).toBeNull();
    savePane(s, "t1", one(true, 40, { k: "diff", task: "t1" }));
    expect(activeTarget(loadPane(s, "t1", alive))).toEqual({ k: "diff" });
    s.setItem(paneKey("t"), JSON.stringify({ open: true, target: { k: "diff", task: 5 } }));
    expect(activeTarget(loadPane(s, "t", alive))).toEqual({ k: "diff" });
  });

  it("migrates a pane saved before tabs (one target) into one tab", () => {
    const s = store();
    s.setItem(paneKey("t"), JSON.stringify({ open: true, width: 50, target: { k: "file", path: "a.ts", line: 2 }, swapped: true }));
    expect(loadPane(s, "t", alive)).toEqual({ open: true, width: 50, tabs: [{ id: "t1", t: { k: "file", path: "a.ts", line: 2 } }], active: "t1", split: null, swapped: true });
    s.setItem(paneKey("t"), JSON.stringify({ open: true, width: 50, target: null }));
    expect(loadPane(s, "t", alive)).toEqual({ ...DEFAULT_PANE, open: true, width: 50 });
  });

  it("drops bad tabs (bad target, bad or repeated id, the same target twice) and keeps the rest", () => {
    const s = store();
    const tabs = [
      { id: "a", t: { k: "file", path: "x.ts" } },
      { id: "b", t: { k: "evil" } },
      { id: "a", t: { k: "steps" } },
      { id: 7, t: { k: "steps" } },
      { id: "c", t: { k: "file", path: "x.ts", line: 9 } },
      { id: "d", t: { k: "shell", n: 2 } },
      { id: "e", t: { k: "shell", n: 9 } },
      { id: "f", t: { k: "shell", n: 1.5 } },
      { id: "g", t: { k: "terminal", task: "t" } },
      "junk",
    ];
    s.setItem(paneKey("t"), JSON.stringify({ open: true, width: 40, tabs, active: "b" }));
    const p = loadPane(s, "t", alive);
    expect(p.tabs).toEqual([{ id: "a", t: { k: "file", path: "x.ts" } }, { id: "d", t: { k: "shell", n: 2 } }]);
    // The active tab was dropped: the first one shows.
    expect(p.active).toBe("a");
    expect(p.open).toBe(true);
  });

  it("caps tabs at 8, when read back and when opened (the oldest that is not shown or a shell goes)", () => {
    const s = store();
    const tabs = Array.from({ length: 12 }, (_, i) => ({ id: `t${i + 1}`, t: { k: "file", path: `f${i}.ts` } }));
    s.setItem(paneKey("t"), JSON.stringify({ open: true, width: 40, tabs, active: "t12" }));
    const p = loadPane(s, "t", alive);
    expect(p.tabs).toHaveLength(PANE_TABS);
    expect(p.active).toBe("t1");
    let q: PaneState = { ...DEFAULT_PANE, open: true };
    q = openTab(q, { k: "shell", n: 1 }, true);
    for (let i = 0; i < 10; i++) q = openTab(q, { k: "file", path: `f${i}.ts` }, true);
    expect(q.tabs).toHaveLength(PANE_TABS);
    expect(q.tabs[0].t).toEqual({ k: "shell", n: 1 });
    expect(activeTarget(q)).toEqual({ k: "file", path: "f9.ts" });
    expect(q.tabs.some((t) => sameFile(t.t, "f0.ts"))).toBe(false);
  });

  it("a split names another tab; its ratio clamps to 20..80%; a bad one is dropped", () => {
    expect(clampRatio(0.05)).toBe(0.2);
    expect(clampRatio(0.95)).toBe(0.8);
    expect(clampRatio(Number.NaN)).toBe(0.5);
    const s = store();
    const tabs = [{ id: "a", t: { k: "steps" } }, { id: "b", t: { k: "diff" } }];
    s.setItem(paneKey("t"), JSON.stringify({ open: true, width: 40, tabs, active: "a", split: { dir: "col", second: "b", ratio: 3 } }));
    expect(loadPane(s, "t", alive).split).toEqual({ dir: "col", second: "b", ratio: 0.8 });
    for (const bad of [{ dir: "diag", second: "b", ratio: 0.5 }, { dir: "row", second: "a", ratio: 0.5 }, { dir: "row", second: "zz", ratio: 0.5 }]) {
      s.setItem(paneKey("t"), JSON.stringify({ open: true, width: 40, tabs, active: "a", split: bad }));
      expect(loadPane(s, "t", alive).split, JSON.stringify(bad)).toBeNull();
    }
  });

  it("opening: the same target focuses its tab; the shown tab is reused unless it is a shell; + adds one", () => {
    let p: PaneState = DEFAULT_PANE;
    p = openTab(p, { k: "file", path: "a.ts" });
    expect(p.open).toBe(true);
    p = openTab(p, { k: "diff" });
    expect(p.tabs.map((t) => t.t.k)).toEqual(["diff"]);
    p = openTab(p, { k: "file", path: "a.ts" }, true);
    p = openTab(p, { k: "diff" });
    expect(p.tabs).toHaveLength(2);
    expect(activeTarget(p)).toEqual({ k: "diff" });
    p = openTab(p, { k: "file", path: "a.ts", line: 4 });
    expect(activeTarget(p)).toEqual({ k: "file", path: "a.ts", line: 4 });
    p = openTab(p, { k: "shell", n: freeShell(p)! });
    p = openTab(p, { k: "steps" });
    expect(p.tabs.map((t) => t.t.k)).toEqual(["diff", "file", "shell", "steps"]);
    expect(freeShell(p)).toBe(2);
  });

  it("closing and focusing tabs, with a split", () => {
    let p: PaneState = DEFAULT_PANE;
    for (const k of ["steps", "diff", "notes"] as const) p = openTab(p, { k }, true);
    p = focusTab(p, "t1");
    p = toggleSplit(p);
    expect(p.split).toEqual({ dir: "row", second: "t2", ratio: 0.5 });
    // The second tab clicked: the two swap places.
    p = focusTab(p, "t2");
    expect([p.active, p.split?.second]).toEqual(["t2", "t1"]);
    p = closeTab(p, "t1");
    expect(p.split).toBeNull();
    expect(p.active).toBe("t2");
    p = closeTab(p, "t2");
    expect(p.active).toBe("t3");
    p = closeTab(p, "t3");
    expect([p.tabs, p.active]).toEqual([[], null]);
    expect(toggleSplit(p)).toBe(p);
  });

  it("a Browser tab keeps a checked localhost URL (or none yet); any other URL read back drops the tab", () => {
    const s = store();
    const tabs = [
      { id: "t1", t: { k: "browser", url: "http://localhost:3000/app" } },
      { id: "t2", t: { k: "browser", url: "" } },
      { id: "t3", t: { k: "browser", url: "https://example.com:443/" } },
      { id: "t4", t: { k: "browser", url: "file:///etc/passwd" } },
      { id: "t5", t: { k: "browser" } },
    ];
    s.setItem(paneKey("a"), JSON.stringify({ open: true, width: 40, tabs, active: "t1" }));
    expect(loadPane(s, "a", alive).tabs).toEqual([
      { id: "t1", t: { k: "browser", url: "http://localhost:3000/app" } },
      { id: "t2", t: { k: "browser", url: "" } },
    ]);
  });

  it("retarget: a tab shows something else in place (the Browser tab navigating)", () => {
    let p = openTab(DEFAULT_PANE, { k: "browser", url: "" }, true);
    p = openTab(p, { k: "shell", n: 1 }, true);
    const id = p.tabs[0].id;
    p = retarget(p, id, { k: "browser", url: "http://localhost:5173/" });
    expect(p.tabs[0]).toEqual({ id, t: { k: "browser", url: "http://localhost:5173/" } });
    expect(p.active).toBe(p.tabs[1].id);
    expect(retarget(p, "nope", { k: "steps" })).toBe(p);
  });
});

const sameFile = (t: PaneTarget, path: string) => t.k === "file" && t.path === path;
