import { describe, expect, it } from "vitest";
import { task } from "../test/fixtures";
import { groupKey, groupOfKey, loadOpen, membersOf, rollupOf, rollupText, saveOpen } from "./groups";

describe("group rollup", () => {
  it("counts by the cockpit's own columns, leaving archived ones out", () => {
    const r = rollupOf(["a", "b", "c", "d", "x"], { a: "needs_you", b: "working", c: "working", d: "archived" });
    expect(r).toEqual({ total: 3, needs_you: 1, working: 2, review: 0, idle: 0 });
    expect(rollupText(r)).toBe("3 children: 1 needs you, 2 working");
    expect(rollupText(rollupOf(["a"], { a: "idle" }))).toBe("1 child: 1 idle");
    expect(rollupText(rollupOf([], {}))).toBe("0 children");
  });

  it("members are the group's tasks not archived, in creation order", () => {
    const tasks = {
      b: task("b", { group_id: "g", created_at: 2 }),
      a: task("a", { group_id: "g", created_at: 1 }),
      z: task("z", { group_id: "g", lifecycle: "archived" }),
      o: task("o"),
    };
    expect(membersOf(tasks, "g").map((t) => t.id)).toEqual(["a", "b"]);
    expect(groupOfKey(groupKey("g1"))).toBe("g1");
    expect(groupOfKey("t1")).toBeNull();
  });

  it("a group row starts folded and remembers being unfolded", () => {
    const m = new Map<string, string>();
    const store = { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
    expect(loadOpen(store, "g")).toBe(false);
    saveOpen(store, "g", true);
    expect(loadOpen(store, "g")).toBe(true);
    const broken = { getItem: () => { throw new Error("no"); }, setItem: () => { throw new Error("no"); } };
    expect(loadOpen(broken, "g")).toBe(false);
    expect(() => saveOpen(broken, "g", true)).not.toThrow();
  });
});
