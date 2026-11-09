import { describe, expect, it } from "vitest";
import { ev, snapshot, task } from "../test/fixtures";
import { columnOf, gridTasks, newlyNeedingYou, projects, tasksIn } from "./select";
import { initialState, reduce } from "./state";

const withTasks = (...ts: ReturnType<typeof task>[]) => reduce(initialState, { type: "connected", snapshot: snapshot(ts) });

describe("columns", () => {
  it("needs-you wins over everything; trust prompt and pending cards count", () => {
    expect(columnOf(task("a", { status: "needs_you" }), [])).toBe("needs_you");
    expect(columnOf(task("a", { status: "working", needs_you: 1 }), [])).toBe("needs_you");
    expect(columnOf(task("a", { boot: "trust_prompt" }), [])).toBe("needs_you");
  });

  it("working, review after a finished turn, idle before any", () => {
    expect(columnOf(task("a", { status: "working" }), [])).toBe("working");
    expect(columnOf(task("a", { boot: "booting" }), [])).toBe("working");
    const ended = [{ seq: 1, ts: 1, kind: "turn" as const, phase: "ended" as const, text: "" }];
    expect(columnOf(task("a", { status: "idle" }), ended)).toBe("review");
    expect(columnOf(task("a", { status: "ended", live: false }), ended)).toBe("review");
    expect(columnOf(task("a"), [])).toBe("idle");
    expect(columnOf(task("a", { lifecycle: "archived", status: "needs_you" }), [])).toBe("archived");
  });

  it("tasksIn filters by repo, newest first", () => {
    const s = withTasks(task("a", { status: "working", updated_at: 1 }), task("b", { status: "working", updated_at: 2 }), task("c", { status: "working", repo_path: "/repos/other" }));
    expect(tasksIn(s, "working", "/repos/shop").map((t) => t.id)).toEqual(["b", "a"]);
  });
});

describe("projects", () => {
  it("merges the registry with task repos and counts needs-you", () => {
    const s = withTasks(task("a", { status: "needs_you" }), task("b"));
    expect(projects(s, ["/repos/mirante", "/repos/shop"])).toEqual([
      { path: "/repos/mirante", name: "mirante", tasks: 0, live: 0, needsYou: 0, hidden: false, group: null },
      { path: "/repos/shop", name: "shop", tasks: 2, live: 2, needsYou: 1, hidden: false, group: null },
    ]);
  });

  it("marks hidden projects and the folder each was picked from", () => {
    const s = withTasks(task("a", { live: false }), task("b", { repo_path: "/g/api" }));
    expect(projects(s, ["/g/web"], { hidden: ["/repos/shop"], groups: { "/g/api": "/g", "/g/web": "/g" } })).toEqual([
      { path: "/g/api", name: "api", tasks: 1, live: 1, needsYou: 0, hidden: false, group: "/g" },
      { path: "/repos/shop", name: "shop", tasks: 1, live: 0, needsYou: 0, hidden: true, group: null },
      { path: "/g/web", name: "web", tasks: 0, live: 0, needsYou: 0, hidden: false, group: "/g" },
    ]);
  });
});

describe("notifications input", () => {
  it("reports only tasks that newly need the user", () => {
    const prev = withTasks(task("a"), task("b", { status: "needs_you" }));
    let next = reduce(prev, { type: "event", event: ev(1, "a", "status", { to: "needs_you" }) });
    expect(newlyNeedingYou(prev, next).map((t) => t.id)).toEqual(["a"]);
    next = reduce(next, { type: "event", event: ev(2, "b", "step", { type: "tool" }) });
    expect(newlyNeedingYou(prev, next).map((t) => t.id)).toEqual(["a"]);
  });
});

describe("grid", () => {
  it("orders needs-you, working, review and caps at six", () => {
    const many = Array.from({ length: 8 }, (_, i) => task(`w${i}`, { status: "working", updated_at: i }));
    const s = withTasks(task("n", { status: "needs_you" }), ...many);
    const g = gridTasks(s);
    expect(g).toHaveLength(6);
    expect(g[0].id).toBe("n");
    expect(g[1].id).toBe("w7");
  });
});

describe("shallow", () => {
  it("compares arrays and objects one level deep", async () => {
    const { shallow } = await import("../env");
    const a = { id: "a" };
    expect(shallow([a], [a])).toBe(true);
    expect(shallow([a], [{ id: "a" }])).toBe(false);
    expect(shallow({ x: a }, { x: a })).toBe(true);
    expect(shallow({ x: a }, { x: a, y: 1 })).toBe(false);
    expect(shallow([], {})).toBe(false);
    expect(shallow(undefined, undefined)).toBe(true);
  });
});
