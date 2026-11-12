import { describe, expect, it } from "vitest";
import { ev, snapshot, task } from "../test/fixtures";
import { initialState, reduce, type State } from "../store/state";
import { elapsed, inboxOf, inboxSections, needingOrder, nextNeeding, PROJECT_COLORS, projectColor, sameSections, waitingLongerThan } from "./inbox";

function state(tasks: ReturnType<typeof task>[], events: ReturnType<typeof ev>[] = []): State {
  let s = reduce(initialState, { type: "connected", snapshot: snapshot(tasks) });
  for (const e of events) s = reduce(s, { type: "event", event: e });
  return s;
}

describe("projectColor", () => {
  it("is stable per path and within the palette", () => {
    expect(projectColor("/repos/shop")).toBe(projectColor("/repos/shop"));
    for (const p of ["/a", "/b", "/repos/mirante", ""]) expect(projectColor(p)).toBeGreaterThanOrEqual(0);
    for (const p of ["/a", "/b", "/repos/mirante", ""]) expect(projectColor(p)).toBeLessThan(PROJECT_COLORS);
    const colors = new Set(["/r/a", "/r/b", "/r/c", "/r/d", "/r/e", "/r/f"].map(projectColor));
    expect(colors.size).toBeGreaterThan(1);
  });
});

describe("inboxSections", () => {
  it("sorts every project's tasks into needs-you (longest wait first), review, working and idle, grouped by project", () => {
    const s = state(
      [
        task("w1", { status: "working", repo_path: "/r/shop", updated_at: 5 }),
        task("n-new", { status: "needs_you", repo_path: "/r/shop", updated_at: 50 }),
        task("n-old", { status: "needs_you", repo_path: "/r/api", updated_at: 10 }),
        task("rev", { status: "idle", repo_path: "/r/api" }),
        task("idle", { status: "idle", repo_path: "/r/api" }),
        task("gone", { status: "idle", lifecycle: "archived" }),
      ],
      [ev(1, "rev", "turn_started"), ev(2, "rev", "turn_ended")],
    );
    const sec = inboxSections(s);
    expect(sec.needs_you.map((g) => [g.name, g.ids])).toEqual([
      ["api", ["n-old"]],
      ["shop", ["n-new"]],
    ]);
    expect(sec.review.map((g) => g.ids)).toEqual([["rev"]]);
    expect(sec.working.map((g) => g.ids)).toEqual([["w1"]]);
    expect(sec.idle.map((g) => g.ids)).toEqual([["idle"]]);
    expect(sec.needs_you[0].color).toBe(projectColor("/r/api"));
    expect(sameSections(sec, inboxSections(s))).toBe(true);
    expect(needingOrder(s)).toEqual(["n-old", "n-new"]);
  });

  it("a pending request's time is the wait; the oldest request counts", () => {
    const s = state(
      [task("a", { status: "needs_you", updated_at: 1 }), task("b", { status: "needs_you", updated_at: 999_999_999 })],
      [{ ...ev(1, "b", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" }), ts_ms: 5 * 60_000 }],
    );
    // b's request came at its event's time (5 min), after a's last change (1 ms).
    expect(needingOrder(s)).toEqual(["a", "b"]);
    const asked = s.perms.p1.created_at;
    expect(waitingLongerThan(s, asked + 11 * 60_000, 10 * 60_000)).toBe(2);
    expect(waitingLongerThan(s, asked + 9 * 60_000, 10 * 60_000)).toBe(1);
  });
});

describe("inboxOf", () => {
  it("is worked out once per (tasks, perms, feed): every reader shares it", () => {
    const s = state([task("a", { status: "needs_you", updated_at: 1 }), task("w", { status: "working" })]);
    const a = inboxOf(s);
    expect(inboxOf(s)).toBe(a);
    // Another state object with the same slices (a connection change): the same answer.
    expect(inboxOf({ ...s, connection: { state: "connecting" } } as State)).toBe(a);
    expect(inboxSections(s)).toBe(a.sections);
    expect(needingOrder(s)).toBe(a.needing);
    expect(a.columns).toEqual({ a: "needs_you", w: "working" });
    expect(a.waits.get("a")).toBe(1);
    // A new feed (an event) is a new answer.
    const s2 = reduce(s, { type: "event", event: ev(9, "w", "turn_started") });
    expect(inboxOf(s2)).not.toBe(a);
  });
});

describe("nextNeeding", () => {
  it("goes to the item after the current one, wrapping, and says where it is", () => {
    expect(nextNeeding(["a", "b", "c"], null)).toEqual({ id: "a", at: 0 });
    expect(nextNeeding(["a", "b", "c"], "a")).toEqual({ id: "b", at: 1 });
    expect(nextNeeding(["a", "b", "c"], "c")).toEqual({ id: "a", at: 3 });
    expect(nextNeeding(["a", "b"], "zzz")).toEqual({ id: "a", at: 0 });
    expect(nextNeeding([], "a")).toEqual({ id: null, at: 0 });
  });
});

describe("elapsed", () => {
  it("reads short", () => {
    expect(elapsed(10_000)).toBe("now");
    expect(elapsed(3 * 60_000)).toBe("3 min");
    expect(elapsed(65 * 60_000)).toBe("1 h 5 min");
    expect(elapsed(120 * 60_000)).toBe("2 h");
    expect(elapsed(50 * 3_600_000)).toBe("2 d");
  });
});

describe("fan-out groups in the inbox", () => {
  const g = { id: "g1", kind: "compare" as const, title: "fix login", goal: "fix login", branch: "mirante/fix-login", created_at: 1 };
  const member = (id: string, over: Parameters<typeof task>[1] = {}) => task(id, { group_id: "g1", group_role: "child", group: g, ...over });

  it("a child needing you is listed under Needs you; the others fold under one group row", () => {
    const s = state([
      member("c1", { status: "needs_you", updated_at: 30 }),
      member("c2", { status: "working", updated_at: 20 }),
      member("c3", { status: "working", updated_at: 25 }),
      task("solo", { status: "working", updated_at: 10 }),
    ]);
    const sec = inboxSections(s);
    expect(sec.needs_you.flatMap((x) => x.ids)).toEqual(["c1"]);
    // One row for the group, at its newest member's change; never its tasks one by one.
    expect(sec.working.flatMap((x) => x.ids)).toEqual(["group:g1", "solo"]);
  });

  it("the group row sits in the most urgent section its other members are in", () => {
    const s = state(
      [member("c1", { status: "idle" }), member("c2", { status: "working" })],
      [ev(1, "c1", "turn_started"), ev(2, "c1", "turn_ended")],
    );
    const sec = inboxSections(s);
    expect(sec.review.flatMap((x) => x.ids)).toEqual(["group:g1"]);
    expect(sec.working.flatMap((x) => x.ids)).toEqual([]);
  });

  it("focus order keeps siblings adjacent, the group with the oldest wait first", () => {
    const s = state([
      member("c1", { status: "needs_you", updated_at: 50 }),
      task("solo", { status: "needs_you", updated_at: 20 }),
      member("c2", { status: "needs_you", updated_at: 10 }),
    ]);
    expect(needingOrder(s)).toEqual(["c2", "c1", "solo"]);
  });
});
