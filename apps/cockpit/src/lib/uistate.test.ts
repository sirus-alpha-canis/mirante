import { describe, expect, it } from "vitest";
import { DEFAULT_UI, loadUi, saveUi, UI_KEY } from "./uistate";

const mem = () => {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m };
};

describe("ui state", () => {
  it("round-trips the view, project, task, tab and scroll", () => {
    const s = mem();
    saveUi(s, { mode: "detail", project: "/r/shop", taskId: "t1", tab: "Diff", scroll: 120 });
    expect(loadUi(s)).toEqual({ mode: "detail", project: "/r/shop", taskId: "t1", tab: "Diff", scroll: 120 });
  });

  it("remembers the Rules view", () => {
    const s = mem();
    saveUi(s, { mode: "rules", project: "/r/shop", taskId: null, tab: null, scroll: 0 });
    expect(loadUi(s).mode).toBe("rules");
  });

  it("anything missing, bad or refused falls back to Home", () => {
    const s = mem();
    expect(loadUi(s)).toEqual(DEFAULT_UI);
    s.setItem(UI_KEY, "{not json");
    expect(loadUi(s)).toEqual(DEFAULT_UI);
    s.setItem(UI_KEY, JSON.stringify({ mode: "nope", tab: "Bogus", scroll: -5, project: 3 }));
    expect(loadUi(s)).toEqual(DEFAULT_UI);
    s.setItem(UI_KEY, JSON.stringify({ mode: "detail" }));
    expect(loadUi(s).mode).toBe("home");
    const throws = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); } };
    expect(loadUi(throws)).toEqual(DEFAULT_UI);
    expect(() => saveUi(throws, DEFAULT_UI)).not.toThrow();
  });
});
