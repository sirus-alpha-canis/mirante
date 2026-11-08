import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { StreamMsg } from "../lib/protocol";
import { ev, snapshot, task } from "../test/fixtures";
import { createCockpit, HISTORY_CONCURRENCY, HISTORY_RETRIES, REFRESH_MS } from "./cockpit";

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

  it("connect subscribes once, however often it is called (StrictMode runs effects twice)", async () => {
    const t = fakeTransport({});
    const c = createCockpit(t);
    await Promise.all([c.connect(), c.connect()]);
    await c.connect();
    expect(t.subscribe).toHaveBeenCalledTimes(1);
  });

  it("a failed subscribe can be retried", async () => {
    const t = fakeTransport({});
    t.subscribe.mockRejectedValueOnce(new Error("no shell"));
    const c = createCockpit(t);
    await expect(c.connect()).rejects.toThrow("no shell");
    await c.connect();
    expect(t.subscribe).toHaveBeenCalledTimes(2);
  });

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

  const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const busy = { kind: "rpc", code: 7, message: "the daemon is at its connection limit; retry" };

  it("loads histories at most HISTORY_CONCURRENCY at a time (a daemon refusing more stays happy)", async () => {
    let inFlight = 0;
    let peak = 0;
    const t = fakeTransport({
      "permission.list": [],
      "task.events": async (p: { task: string }) => {
        if (inFlight >= HISTORY_CONCURRENCY) throw busy;
        inFlight++;
        peak = Math.max(peak, inFlight);
        await wait(20);
        inFlight--;
        return [ev(1, p.task, "turn_ended")];
      },
    });
    const c = createCockpit(t);
    await c.connect();
    const ids = Array.from({ length: 12 }, (_, i) => `t${i}`);
    t.push({ type: "connected", snapshot: snapshot(ids.map((id) => task(id)), 1) });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(peak).toBe(HISTORY_CONCURRENCY);
    expect(ids.filter((id) => c.getState().feed[id]?.length === 1)).toEqual(ids);
    expect(c.getState().historyErrors).toEqual({});
  });

  it("retries a busy history load with backoff, then surfaces the error for that task", async () => {
    const tries: Record<string, number> = {};
    const t = fakeTransport({
      "permission.list": [],
      "task.events": async (p: { task: string }) => {
        tries[p.task] = (tries[p.task] ?? 0) + 1;
        if (p.task === "t2" || tries[p.task] < 3) throw busy;
        return [ev(1, p.task, "turn_ended")];
      },
    });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1"), task("t2")], 1) });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(c.getState().feed.t1).toHaveLength(1);
    expect(tries.t2).toBe(HISTORY_RETRIES + 1);
    expect(c.getState().historyErrors.t2).toMatch(/connection limit/);
    expect(c.getState().historyErrors.t1).toBeUndefined();
  });

  it("a non-busy history failure is surfaced without retrying", async () => {
    const t = fakeTransport({
      "permission.list": [],
      "task.events": async () => {
        throw { kind: "rpc", code: 1, message: "no task t1" };
      },
    });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 1) });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(t.calls.filter((x) => x[0] === "task.events")).toHaveLength(1);
    expect(c.getState().historyErrors.t1).toBe("no task t1");
  });

  it("drops a task.get reply older than one already applied", async () => {
    let n = 0;
    const t = fakeTransport({
      "task.events": [],
      "permission.list": [],
      "task.get": async () => {
        n++;
        // The first call answers late, with what was true before the second.
        if (n === 1) {
          await wait(500);
          return task("t1", { status: "working" });
        }
        return task("t1", { status: "idle", queued: 3 });
      },
    });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 0) });
    t.push({ type: "event", event: ev(1, "t1", "turn_started") });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    t.push({ type: "event", event: ev(2, "t1", "turn_ended") });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    expect(c.getState().tasks.t1.queued).toBe(3);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(c.getState().tasks.t1).toMatchObject({ status: "idle", queued: 3 });
  });

  it("drops a task.list reply older than one already applied, and keeps newer task.get rows", async () => {
    let lists = 0;
    const t = fakeTransport({
      "permission.list": [],
      "task.list": async () => {
        lists++;
        if (lists === 1) {
          await wait(500);
          return [task("t1", { status: "working" }), task("t2", { status: "working" })];
        }
        return [task("t1", { status: "idle" }), task("t2", { status: "ended" })];
      },
      "task.get": task("t1", { status: "needs_you" }),
    });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([], 4, { resumed: true }) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    t.push({ type: "connected", snapshot: snapshot([], 4, { resumed: true }) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    t.push({ type: "event", event: ev(5, "t1", "status", { to: "needs_you" }) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    await vi.advanceTimersByTimeAsync(1_000);
    expect([c.getState().tasks.t1.status, c.getState().tasks.t2.status]).toEqual(["needs_you", "ended"]);
  });

  it("a permission.list answered after a newer close does not reopen the card", async () => {
    let lists = 0;
    const pending = { id: "p1", task_id: "t1", session_id: "s", tool: "Bash", detail: "", status: "pending", created_at: 1, answered_at: null, task_title: "", branch: "" };
    const t = fakeTransport({
      "task.events": [],
      "task.get": task("t1"),
      "permission.list": async () => {
        lists++;
        if (lists === 2) {
          // Asked right after the request, answered after the card closed.
          await wait(500);
          return [pending];
        }
        return lists === 1 ? [] : [];
      },
    });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 0) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    t.push({ type: "event", event: ev(1, "t1", "permission_requested", { permission_id: "p1", tool: "Bash" }) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    t.push({ type: "event", event: ev(2, "t1", "permission_answered", { permission_id: "p1", answer: "denied" }) });
    await vi.advanceTimersByTimeAsync(1_000);
    expect(c.getState().perms.p1.status).toBe("denied");
  });
});

describe("queue refresh", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it.each(["queue_changed", "message_queued", "message_requeued", "message_sent", "queue_dropped"])(
    "%s refetches that task's queue (once per burst)",
    async (kind) => {
      const t = fakeTransport({ "task.events": [], "permission.list": [], "task.get": task("t1"), "message.list_queued": [{ id: "x", text: "hi" }] });
      const c = createCockpit(t);
      await c.connect();
      t.push({ type: "connected", snapshot: snapshot([task("t1")], 0) });
      t.push({ type: "event", event: ev(1, "t1", kind, { op: "removed", id: "y", mode: "queue", text: "y" }) });
      t.push({ type: "event", event: ev(2, "t1", kind, { op: "removed", id: "z", mode: "queue", text: "z" }) });
      await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
      expect(t.calls.filter((x) => x[0] === "message.list_queued")).toEqual([["message.list_queued", { task_id: "t1" }]]);
      expect(c.getState().queues.t1).toEqual([{ id: "x", text: "hi" }]);
    },
  );

  it("a steer does not refetch the queue", async () => {
    const t = fakeTransport({ "task.events": [], "permission.list": [], "task.get": task("t1") });
    const c = createCockpit(t);
    await c.connect();
    t.push({ type: "connected", snapshot: snapshot([task("t1")], 0) });
    t.push({ type: "event", event: ev(1, "t1", "message_sent", { mode: "steer", text: "now" }) });
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    expect(t.calls.map((x) => x[0])).not.toContain("message.list_queued");
  });

  it("refreshQueue loads on demand; an older reply never overwrites a newer one", async () => {
    const replies: ((v: unknown) => void)[] = [];
    const t = fakeTransport({ "message.list_queued": () => new Promise((r) => replies.push(r)) });
    const c = createCockpit(t);
    c.refreshQueue("t1");
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    c.refreshQueue("t1");
    await vi.advanceTimersByTimeAsync(REFRESH_MS + 1);
    expect(replies).toHaveLength(2);
    replies[1]([{ id: "new", text: "new" }]);
    await vi.advanceTimersByTimeAsync(0);
    replies[0]([{ id: "old", text: "old" }]);
    await vi.advanceTimersByTimeAsync(0);
    expect(c.getState().queues.t1).toEqual([{ id: "new", text: "new" }]);
  });
});
