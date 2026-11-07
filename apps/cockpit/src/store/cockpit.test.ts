import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { StreamMsg } from "../lib/protocol";
import { ev, snapshot, task } from "../test/fixtures";
import { createCockpit, REFRESH_MS } from "./cockpit";

function fakeTransport(answers: Record<string, unknown>) {
  let push: (m: StreamMsg) => void = () => {};
  const calls: [string, object | undefined][] = [];
  return {
    calls,
    push: (m: StreamMsg) => push(m),
    rpc: vi.fn(async (method: string, params?: object) => {
      calls.push([method, params]);
      const a = answers[method];
      return (typeof a === "function" ? a(params) : a) as never;
    }),
    subscribe: vi.fn(async (fn: (m: StreamMsg) => void) => {
      push = fn;
    }),
  };
}

describe("createCockpit", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("loads history and permissions after a full snapshot", async () => {
    const t = fakeTransport({
      "permission.list": [],
      "task.events": [ev(1, "t1", "turn_started"), ev(2, "t1", "turn_ended")],
    });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 2) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    expect(t.calls).toContainEqual(["task.events", { task: "t1", limit: 200 }]);
    expect(t.calls.map((x) => x[0])).toContain("permission.list");
    expect(c.getState().feed.t1).toHaveLength(2);
  });

  it("refreshes a task once for a burst of its events", async () => {
    const t = fakeTransport({ "task.events": [], "permission.list": [], "task.get": task("t1", { status: "working", queued: 2 }) });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 0) });
    for (let i = 1; i <= 5; i++) t.push({ type: "event", event: ev(i, "t1", "step", { type: "tool" }) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    expect(t.calls.filter((x) => x[0] === "task.get")).toEqual([["task.get", { task: "t1" }]]);
    expect(c.getState().tasks.t1.queued).toBe(2);
  });

  it("after a resumed snapshot reloads the task list instead of history", async () => {
    const t = fakeTransport({ "permission.list": [], "task.list": [task("t1"), task("t2")] });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([], 4, { resumed: true }) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    expect(Object.keys(c.getState().tasks)).toEqual(["t1", "t2"]);
    expect(t.calls.map((x) => x[0])).not.toContain("task.events");
  });

  it("tells watchers about each change with prev and next", async () => {
    const t = fakeTransport({ "task.events": [], "permission.list": [], "task.get": task("t1", { status: "needs_you" }) });
    const c = createCockpit(t);
    const seen: string[] = [];
    c.onChange((p, n) => seen.push(`${p.tasks.t1?.status ?? "-"}>${n.tasks.t1?.status ?? "-"}`));
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 0) });
    t.push({ type: "event", event: ev(1, "t1", "status", { to: "needs_you" }) });
    expect(seen).toEqual(["->idle", "idle>needs_you"]);
  });
});
