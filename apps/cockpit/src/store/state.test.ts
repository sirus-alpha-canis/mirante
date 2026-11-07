import { describe, expect, it } from "vitest";
import { ev, snapshot, task, taskRow } from "../test/fixtures";
import { feedItem, initialState, reduce, type State } from "./state";

const connected = (s: State = initialState, tasks = [task("t1")], seq = 5) =>
  reduce(s, { type: "connected", snapshot: snapshot(tasks, seq) });

describe("reduce", () => {
  it("a full snapshot replaces the tasks and sets the resume point", () => {
    const s = connected();
    expect(s.connection).toEqual({ state: "live" });
    expect([s.bootId, s.seq, Object.keys(s.tasks)]).toEqual(["b1", 5, ["t1"]]);
  });

  it("a resumed snapshot keeps the state", () => {
    const s = connected();
    const r = reduce(s, { type: "connected", snapshot: snapshot([], 5, { resumed: true }) });
    expect(r.tasks).toBe(s.tasks);
  });

  it("events apply in seq order and replays are ignored", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "status", { from: "idle", to: "working" }) });
    expect(s.tasks.t1.status).toBe("working");
    const again = reduce(s, { type: "event", event: ev(6, "t1", "status", { from: "working", to: "idle" }) });
    expect(again).toBe(s);
    const old = reduce(s, { type: "event", event: ev(3, "t1", "status", { to: "ended" }) });
    expect(old).toBe(s);
  });

  it("an event from a new boot is applied even with a lower seq", () => {
    const s = reduce(connected(), { type: "event", event: ev(2, "t1", "status", { to: "working" }, "b2") });
    expect([s.bootId, s.seq, s.tasks.t1.status]).toEqual(["b2", 2, "working"]);
  });

  it("task_created adds a row; refreshed views overwrite guesses", () => {
    let s = connected(initialState, []);
    s = reduce(s, { type: "event", event: ev(6, "t2", "task_created", taskRow("t2")) });
    expect(s.tasks.t2.live).toBe(false);
    s = reduce(s, { type: "task", view: task("t2", { needs_you: 3 }) });
    expect(s.tasks.t2.needs_you).toBe(3);
  });

  it("permission cards follow requested -> answered and never go back", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "rm -rf build" }) });
    expect(s.perms.p1).toMatchObject({ status: "pending", tool: "Bash", task_id: "t1" });
    expect(s.tasks.t1.needs_you).toBe(1);
    s = reduce(s, { type: "event", event: ev(7, "t1", "permission_answered", { permission_id: "p1", answer: "allowed_once" }) });
    expect(s.perms.p1.status).toBe("allowed_once");
    // An older history page replaying the request does not reopen it.
    s = reduce(s, { type: "history", taskId: "t1", events: [ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "Bash" })] });
    expect(s.perms.p1.status).toBe("allowed_once");
  });

  it("a superseded card closes as denied; an unrecorded answer leaves the card as settled", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "Bash" }) });
    s = reduce(s, { type: "event", event: ev(7, "t1", "permission_superseded", { permission_id: "p1" }) });
    expect(s.perms.p1.status).toBe("denied");
    s = reduce(s, { type: "event", event: ev(8, "t1", "permission_requested", { permission_id: "p2", tool: "Bash" }) });
    s = reduce(s, { type: "event", event: ev(9, "t1", "permission_answered", { permission_id: "p2", answer: "allowed_once" }) });
    s = reduce(s, { type: "event", event: ev(10, "t1", "permission_answer_unrecorded", { permission_id: "p2", allow: false }) });
    expect(s.perms.p2.status).toBe("allowed_once");
  });

  it("permission.list marks pending cards it no longer lists as expired", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "Bash" }) });
    s = reduce(s, { type: "permissions", list: [] });
    expect(s.perms.p1.status).toBe("expired");
  });

  it("history merges into the feed without duplicates, sorted by seq", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(9, "t1", "step", { type: "tool", tool: "Bash", summary: "ls", ok: true }) });
    s = reduce(s, {
      type: "history",
      taskId: "t1",
      events: [ev(7, "t1", "turn_started"), ev(9, "t1", "step", { type: "tool", tool: "Bash", summary: "ls" })],
    });
    expect(s.feed.t1.map((i) => [i.seq, i.kind])).toEqual([
      [7, "turn"],
      [9, "tool"],
    ]);
  });

  it("disconnects keep the data and record the retry", () => {
    const s = reduce(connected(), { type: "disconnected", error: "gone", retryMs: 500 });
    expect(s.connection).toEqual({ state: "down", error: "gone", retryMs: 500 });
    expect(s.tasks.t1).toBeDefined();
  });
});

describe("feedItem", () => {
  it("normalizes Claude and Codex steps", () => {
    expect(feedItem(ev(1, "t", "step", { type: "tool", tool: "Edit", summary: "src/a.rs", ok: false }))).toMatchObject({ kind: "tool", text: "Edit src/a.rs", ok: false });
    expect(feedItem(ev(2, "t", "step", { type: "command", command: "npm test", exit_code: 0 }))).toMatchObject({ kind: "command", text: "$ npm test", ok: true });
    expect(feedItem(ev(3, "t", "step", { type: "agent_message", text: "done" }))).toMatchObject({ kind: "agent_message", text: "done" });
    expect(feedItem(ev(4, "t", "step", { type: "steer_fell_back", error: "no turn" }))).toMatchObject({ kind: "system", text: "steer_fell_back: no turn" });
  });

  it("shows the human's messages with their mode and the delivery verdict", () => {
    expect(feedItem(ev(1, "t", "message_queued", { text: "also docs" }))).toMatchObject({ kind: "message", mode: "queue" });
    expect(feedItem(ev(2, "t", "message_sent", { mode: "steer", text: "stop" }))).toMatchObject({ kind: "message", mode: "steer", text: "stop" });
    expect(feedItem(ev(3, "t", "message_delivery", { delivery: "steered" }))?.text).toBe("message steered");
    expect(feedItem(ev(4, "t", "hook_orphan"))).toBeNull();
  });

  it("memory guard events become system lines", () => {
    expect(feedItem(ev(1, "t", "memory.warn", { state: "soft", bytes: 3 * 2 ** 30, budget: 4 * 2 ** 30 }))?.text).toBe("memory soft (3072 MB of 4096 MB)");
  });
});
