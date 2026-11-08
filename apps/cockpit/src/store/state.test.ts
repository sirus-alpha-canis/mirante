import { describe, expect, it } from "vitest";
import { ev, snapshot, task, taskRow } from "../test/fixtures";
import { isMemoryEvent } from "../lib/memguard";
import { dnaRev, feedItem, initialState, reduce, type State } from "./state";

const connected = (s: State = initialState, tasks = [task("t1")], seq = 5) =>
  reduce(s, { type: "connected", snapshot: snapshot(tasks, seq) });

describe("reduce", () => {
  it("a full snapshot replaces the tasks and sets the resume point", () => {
    const s = connected();
    expect(s.connection).toEqual({ state: "live" });
    expect([s.bootId, s.seq, Object.keys(s.tasks)]).toEqual(["b1", 5, ["t1"]]);
  });

  it("a new daemon boot starts the per-boot state over: queues, session skills, history errors, notices, feeds", () => {
    let s = connected(initialState, [task("t1"), task("t2")]);
    s = reduce(s, { type: "event", event: ev(6, "t1", "step", { type: "tool", tool: "Read", summary: "a", ok: true }) });
    s = reduce(s, { type: "event", event: ev(7, "t2", "permission_requested", { permission_id: "p2", tool: "Bash", detail: "ls" }) });
    s = reduce(s, { type: "event", event: ev(8, "t1", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" }) });
    s = reduce(s, { type: "queue", taskId: "t1", list: [{ id: "m1", text: "hi" }] });
    s = reduce(s, { type: "historyFailed", taskId: "t1", error: "busy" });
    s = reduce(s, { type: "event", event: ev(9, "t1", "skill.injected", { session_id: "s1", names: ["x"] }) });
    s = reduce(s, { type: "event", event: ev(10, null, "memory.pressure", { level: "warn" }) });
    expect(s.memory.length).toBeGreaterThan(0);
    const r = reduce(s, { type: "connected", snapshot: snapshot([task("t1")], 1, { boot_id: "b2" }) });
    expect(r.bootId).toBe("b2");
    expect([r.queues, r.sessionSkills, r.historyErrors, r.memory, r.feed]).toEqual([{}, {}, {}, [], {}]);
    // The gone task's cards are dropped; the live task's stay until permission.list says.
    expect(Object.keys(r.perms)).toEqual(["p1"]);
  });

  it("a full snapshot of the same boot drops what belonged to tasks no longer there", () => {
    let s = connected(initialState, [task("t1"), task("t2")]);
    s = reduce(s, { type: "event", event: ev(6, "t2", "step", { type: "tool", tool: "Read", summary: "a", ok: true }) });
    s = reduce(s, { type: "event", event: ev(7, "t2", "permission_requested", { permission_id: "p2", tool: "Bash", detail: "ls" }) });
    s = reduce(s, { type: "queue", taskId: "t2", list: [] });
    const r = reduce(s, { type: "connected", snapshot: snapshot([task("t1")], 7) });
    expect([Object.keys(r.feed), Object.keys(r.perms), Object.keys(r.queues)]).toEqual([[], [], []]);
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

  it("leaving manual mode is a warning line; going back says so; the first manual report says nothing", () => {
    expect(feedItem(ev(1, "t", "permission_mode", { from: null, mode: "default", self_approves: null }))).toBeNull();
    expect(feedItem(ev(2, "t", "permission_mode", { from: "default", mode: "auto", self_approves: "commands" }))).toMatchObject({
      kind: "system",
      warn: true,
      text: "auto mode on — Claude approves its own commands",
    });
    expect(feedItem(ev(3, "t", "permission_mode", { from: "default", mode: "acceptEdits", self_approves: "edits" }))?.text).toBe(
      "accept-edits mode on — Claude edits files without asking",
    );
    expect(feedItem(ev(4, "t", "permission_mode", { from: "auto", mode: "default", self_approves: null }))?.text).toBe(
      "back to manual mode — Claude asks before acting",
    );
    expect(feedItem(ev(5, "t", "permission_mode", { from: "default", mode: "plan", self_approves: null }))).toBeNull();
  });

  it("auto-resume after a daemon restart has its lines", () => {
    expect(feedItem(ev(1, "t", "session.auto_resumed", { resumed: true }))?.text).toBe("resumed after the daemon restarted");
    expect(feedItem(ev(2, "t", "session.auto_resume_skipped", { reason: "hibernated" }))?.text).toBe("not resumed after the daemon restarted: hibernated");
    expect(feedItem(ev(3, "t", "session.auto_resume_failed", { error: "boom" }))?.text).toBe("could not resume after the daemon restarted: boom");
    expect(feedItem(ev(4, "t", "session.auto_resume_pending", { session_id: "s" }))).toBeNull();
  });

  it("a tool step keeps its tool name apart", () => {
    expect(feedItem(ev(1, "t", "step", { type: "tool", tool: "Read", summary: "a.ts", ok: true }))).toMatchObject({ tool: "Read", text: "Read a.ts" });
  });

  it("memory guard events become system lines", () => {
    expect(feedItem(ev(1, "t", "memory.warn", { state: "soft", bytes: 3 * 2 ** 30, budget: 4 * 2 ** 30 }))?.text).toBe("memory soft (3072 MB of 4096 MB)");
  });
});

// The daemon's real sequences (crates/mirante-core/src/store.rs `answer_permission` always
// emits `permission_answered`; the hub then adds its refinement right after):
//   card answered from the cockpit/CLI: requested -> answered
//   Codex grantRoot allowed:            requested -> answered{allowed_session} -> session_grant
//   settled by a hook / the dialog:     requested -> answered{answer} -> settled{answer, cause}
//   session ended / daemon restart:     requested -> answered{denied} -> expired{cause}
//   a newer dialog of the same agent:   requested(p1) -> requested(p2) -> answered(p1){denied} -> superseded(p1){by: p2}
describe("permission event pairs", () => {
  const req = (seq: number, id: string) => ev(seq, "t1", "permission_requested", { permission_id: id, tool: "Bash", detail: "x" });
  const ans = (seq: number, id: string, answer: string) => ev(seq, "t1", "permission_answered", { permission_id: id, answer });
  const replay = (s: State, events: ReturnType<typeof ev>[]) => events.reduce((acc, e) => reduce(acc, { type: "event", event: e }), s);
  const permLines = (s: State) => (s.feed.t1 ?? []).filter((i) => i.seq > 5 && i.kind === "system").map((i) => i.text);

  it.each([
    ["settled", [req(6, "p1"), req(7, "p2"), ans(8, "p1", "allowed_once"), ev(9, "t1", "permission_settled", { permission_id: "p1", answer: "allowed_once", cause: "hook:PostToolUse" })], "allowed_once"],
    ["expired", [req(6, "p1"), req(7, "p2"), ans(8, "p1", "denied"), ev(9, "t1", "permission_expired", { permission_id: "p1", cause: "mirante:stop" })], "expired"],
    ["superseded", [req(6, "p1"), req(7, "p2"), ans(8, "p1", "denied"), ev(9, "t1", "permission_superseded", { permission_id: "p1", by: "p2" })], "denied"],
  ] as const)("answered then %s lowers needs_you once and adds one feed line", (_k, events, final) => {
    const s = replay(connected(), [...events]);
    expect(s.perms.p1.status).toBe(final);
    expect(s.perms.p2.status).toBe("pending");
    expect(s.tasks.t1.needs_you).toBe(1);
    expect(permLines(s)).toHaveLength(1);
  });

  it.each(["permission_expired", "permission_superseded", "permission_settled"])("%s without a prior answered closes the card once", (kind) => {
    let s = replay(connected(), [req(6, "p1"), req(7, "p2"), ev(8, "t1", kind, { permission_id: "p1", answer: "denied", by: "p2" })]);
    expect(s.perms.p1.status).not.toBe("pending");
    expect(s.tasks.t1.needs_you).toBe(1);
    expect(permLines(s)).toHaveLength(1);
    // A late duplicate answer for the same card changes nothing more.
    s = replay(s, [ans(9, "p1", "denied")]);
    expect(s.tasks.t1.needs_you).toBe(1);
    expect(permLines(s)).toHaveLength(1);
  });

  it("a history page with the pair adds one line, and the live replay of it no second decrement", () => {
    let s = replay(connected(), [req(6, "p1")]);
    const pair = [ans(7, "p1", "denied"), ev(8, "t1", "permission_expired", { permission_id: "p1" })];
    s = reduce(s, { type: "history", taskId: "t1", events: pair });
    expect(permLines(s)).toEqual(["denied"]);
    s = replay(s, pair);
    expect(s.perms.p1.status).toBe("expired");
    expect(s.tasks.t1.needs_you).toBe(0);
    expect(permLines(s)).toEqual(["denied"]);
  });

  it("does not report a task as newly needing you again after the pair", () => {
    // Two cards; the first one settled: still needs you (one left), never 0 then back to 1.
    const counts: number[] = [];
    let s = connected();
    for (const e of [req(6, "p1"), req(7, "p2"), ans(8, "p1", "allowed_once"), ev(9, "t1", "permission_settled", { permission_id: "p1", answer: "allowed_once" })]) {
      s = reduce(s, { type: "event", event: e });
      counts.push(s.tasks.t1.needs_you);
    }
    expect(counts).toEqual([1, 2, 1, 1]);
  });
});

describe("allowed_session", () => {
  it("a session-wide grant is an allowed card, not a denied one", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "grantRoot" }) });
    s = reduce(s, { type: "event", event: ev(7, "t1", "permission_answered", { permission_id: "p1", answer: "allowed_session" }) });
    expect(s.perms.p1.status).toBe("allowed_session");
    expect(s.feed.t1.at(-1)?.text).toBe("allowed for this session");
  });

  it("permission.list rows with allowed_session keep it", () => {
    const s = reduce(connected(), {
      type: "permissions",
      list: [{ id: "p1", task_id: "t1", session_id: "s", tool: "grantRoot", detail: "", status: "allowed_session", created_at: 1, answered_at: 2, task_title: "", branch: "" }],
    });
    expect(s.perms.p1.status).toBe("allowed_session");
  });
});

describe("stale replies", () => {
  const listed = (id: string) => ({ id, task_id: "t1", session_id: "s", tool: "Bash", detail: "", status: "pending" as const, created_at: 1, answered_at: null, task_title: "", branch: "" });

  it("a permission.list issued before a newer event does not reopen the card", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "Bash" }) });
    const rev = s.permRev;
    s = reduce(s, { type: "event", event: ev(7, "t1", "permission_answered", { permission_id: "p1", answer: "denied" }) });
    s = reduce(s, { type: "permissions", list: [listed("p1")], rev });
    expect(s.perms.p1.status).toBe("denied");
  });

  it("a permission.list issued before a request does not expire it", () => {
    let s = connected();
    const rev = s.permRev;
    s = reduce(s, { type: "event", event: ev(6, "t1", "permission_requested", { permission_id: "p1", tool: "Bash" }) });
    s = reduce(s, { type: "permissions", list: [], rev });
    expect(s.perms.p1.status).toBe("pending");
  });

  it("a task.list reply keeps rows refreshed more recently", () => {
    let s = connected(initialState, [task("t1"), task("t2")]);
    s = reduce(s, { type: "task", view: task("t1", { status: "working" }) });
    s = reduce(s, { type: "tasks", views: [task("t1", { status: "idle" }), task("t2", { status: "ended" })], keep: ["t1"] });
    expect([s.tasks.t1.status, s.tasks.t2.status]).toEqual(["working", "ended"]);
  });
});

describe("history errors", () => {
  it("a failed history load is recorded and cleared by a later one", () => {
    let s = reduce(connected(), { type: "historyFailed", taskId: "t1", error: "busy" });
    expect(s.historyErrors.t1).toBe("busy");
    s = reduce(s, { type: "history", taskId: "t1", events: [] });
    expect(s.historyErrors.t1).toBeUndefined();
  });
});

describe("queue", () => {
  const q = (id: string, text = id) => ({ id, text });
  const s0 = () => {
    const s = reduce(initialState, { type: "connected", snapshot: snapshot([task("t1")], 0) });
    return reduce(s, { type: "queue", taskId: "t1", list: [q("a"), q("b"), q("c")] });
  };

  it("a fetched list replaces the task's queue", () => {
    const s = s0();
    expect(s.queues.t1).toEqual([q("a"), q("b"), q("c")]);
    expect(reduce(s, { type: "queue", taskId: "t1", list: [] }).queues.t1).toEqual([]);
  });

  it("queue_changed applies edits, removals and reorders right away (the refetch confirms)", () => {
    let s = s0();
    s = reduce(s, { type: "event", event: ev(1, "t1", "queue_changed", { op: "edited", id: "b", text: "B!" }) });
    expect(s.queues.t1).toEqual([q("a"), q("b", "B!"), q("c")]);
    s = reduce(s, { type: "event", event: ev(2, "t1", "queue_changed", { op: "removed", id: "a", text: "a" }) });
    expect(s.queues.t1).toEqual([q("b", "B!"), q("c")]);
    s = reduce(s, { type: "event", event: ev(3, "t1", "queue_changed", { op: "reordered", ids: ["c", "b"] }) });
    expect(s.queues.t1).toEqual([q("c"), q("b", "B!")]);
    expect(s.feed.t1.map((i) => i.text)).toEqual(["queued message edited", "queued message removed", "queue reordered"]);
  });

  it("a queued message is delivered off the front; a dropped queue empties", () => {
    let s = s0();
    s = reduce(s, { type: "event", event: ev(1, "t1", "message_sent", { mode: "queue", text: "a", id: "a" }) });
    expect(s.queues.t1.map((m) => m.id)).toEqual(["b", "c"]);
    s = reduce(s, { type: "event", event: ev(2, "t1", "queue_dropped", { messages: ["b", "c"] }) });
    expect(s.queues.t1).toEqual([]);
  });
});

describe("repo memory (dna.*) events", () => {
  it("dna events land in the task feed and never in the memory guard", () => {
    expect(feedItem(ev(1, "t", "dna.proposed", { id: "p", title: "Spool" }))?.text).toBe("proposed memory: Spool (awaiting review)");
    expect(feedItem(ev(2, "t", "dna.decided", { id: "p", accepted: true, slug: "spool" }))?.text).toBe("memory proposal accepted as spool");
    expect(feedItem(ev(3, "t", "dna.decided", { id: "p", accepted: false, slug: null }))?.text).toBe("memory proposal rejected");
    expect(feedItem(ev(4, "t", "dna.unavailable", { reason: "repo memory is off for this session: no binary" }))?.text).toBe(
      "repo memory is off for this session: no binary",
    );
    expect(isMemoryEvent("dna.proposed")).toBe(false);
    let s = connected(initialState, [task("t1", { repo_path: "/r" })]);
    s = reduce(s, { type: "event", event: ev(6, "t1", "dna.proposed", { id: "p", title: "Spool" }) });
    expect(s.memory).toEqual([]);
    expect(s.feed.t1.map((i) => i.text)).toEqual(["proposed memory: Spool (awaiting review)"]);
  });

  it("indexed, changed, proposed and decided bump the repo's dna version so views refetch", () => {
    let s = connected(initialState, [task("t1", { repo_path: "/r" })]);
    expect(dnaRev(s, "/r")).toBe(0);
    s = reduce(s, { type: "event", event: ev(6, null, "dna.indexed", { repo: "/r", head: "h", notes: 1, stale: 0, unreviewed: 0, invalid: 0 }) });
    s = reduce(s, { type: "event", event: ev(7, null, "dna.changed", { repo: "/r", slug: "a" }) });
    s = reduce(s, { type: "event", event: ev(8, "t1", "dna.decided", { id: "p", accepted: true, slug: "a" }) });
    s = reduce(s, { type: "event", event: ev(9, "t1", "dna.proposed", { id: "q", title: "Q" }) });
    expect(dnaRev(s, "/r")).toBe(4);
    expect(dnaRev(s, "/other")).toBe(0);
    // A decision with no task (a CLI proposal) does not know its repo: every repo refetches.
    s = reduce(s, { type: "event", event: ev(10, null, "dna.decided", { id: "c", accepted: false, slug: null }) });
    expect([dnaRev(s, "/r"), dnaRev(s, "/other")]).toEqual([5, 1]);
    // Unrelated events leave it alone.
    const same = reduce(s, { type: "event", event: ev(11, "t1", "turn_started") });
    expect(same.dnaVersion).toBe(s.dnaVersion);
  });

  it("an event keyed by the daemon's canonical repo refetches views that know the repo by another path", () => {
    // The task was created from a subdirectory; the daemon keys memory by git's top level.
    let s = connected(initialState, [task("t1", { repo_path: "/tmp/app/sub" })]);
    s = reduce(s, { type: "dnaKey", path: "/tmp/app/sub", key: "/private/tmp/app" });
    s = reduce(s, { type: "dnaKey", path: "/tmp/app", key: "/private/tmp/app" });
    s = reduce(s, { type: "event", event: ev(6, null, "dna.changed", { repo: "/private/tmp/app", slug: "a" }) });
    s = reduce(s, { type: "event", event: ev(7, "t1", "dna.proposed", { id: "q", title: "Q", repo: "/private/tmp/app" }) });
    expect([dnaRev(s, "/tmp/app/sub"), dnaRev(s, "/tmp/app"), dnaRev(s, "/private/tmp/app"), dnaRev(s, "/other")]).toEqual([2, 2, 2, 0]);
    // Learning the same key again changes nothing.
    expect(reduce(s, { type: "dnaKey", path: "/tmp/app", key: "/private/tmp/app" })).toBe(s);
  });
});

describe("skills (skill.*) events", () => {
  it("proposed, decided, changed, mined, learning and origin events bump skillsVersion", () => {
    let s = connected();
    expect(s.skillsVersion).toBe(0);
    const kinds = ["skill.proposed", "skill.decided", "skill.changed", "skill.mined", "skill.learning", "skill.origin_confirmed", "skill.origin_pinned"];
    kinds.forEach((k, i) => {
      s = reduce(s, { type: "event", event: ev(6 + i, null, k, {}) });
    });
    expect(s.skillsVersion).toBe(kinds.length);
    s = reduce(s, { type: "event", event: ev(20, "t1", "status", { to: "working" }) });
    expect(s.skillsVersion).toBe(kinds.length);
  });

  it("injected, refused, unavailable and used bump the task's skill version and record what its session got", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "skill.injected", { session_id: "s1", names: ["a", "b"] }) });
    s = reduce(s, { type: "event", event: ev(7, "t1", "skill.refused", { session_id: "s1", name: "c", reason: "modified on disk: SKILL.md" }) });
    expect(s.skillTaskVersion.t1).toBe(2);
    expect(s.sessionSkills.t1).toEqual({ session: "s1", names: ["a", "b"], refused: [{ name: "c", reason: "modified on disk: SKILL.md" }], used: [] });
    // A new session starts over.
    s = reduce(s, { type: "event", event: ev(8, "t1", "skill.injected", { session_id: "s2", names: ["a"] }) });
    expect(s.sessionSkills.t1).toEqual({ session: "s2", names: ["a"], refused: [], used: [] });
    s = reduce(s, { type: "event", event: ev(9, "t1", "skill.used", { session_id: "s2", name: "a", version: 1 }) });
    expect(s.skillTaskVersion.t1).toBe(4);
    expect(s.sessionSkills.t1.used).toEqual(["a"]);
  });

  it("history replays what the session got", () => {
    const s = reduce(connected(), {
      type: "history",
      taskId: "t1",
      events: [ev(2, "t1", "skill.refused", { session_id: "s1", name: "c", reason: "over budget" }), ev(1, "t1", "skill.injected", { session_id: "s1", names: ["a"] })],
    });
    expect(s.sessionSkills.t1).toEqual({ session: "s1", names: ["a"], refused: [{ name: "c", reason: "over budget" }], used: [] });
  });

  it("skill events have feed lines", () => {
    const line = (kind: string, p: Record<string, unknown>) => feedItem(ev(1, "t1", kind, p))?.text;
    expect(line("skill.injected", { session_id: "s", names: ["a", "b"] })).toBe("skills given to this session: a, b");
    expect(line("skill.refused", { session_id: "s", name: "c", reason: "modified on disk: SKILL.md" })).toBe("skill c refused: modified on disk: SKILL.md");
    expect(line("skill.unavailable", { reason: "no origin" })).toBe("no skills: no origin");
    expect(line("skill.used", { session_id: "s", name: "a", version: 2 })).toBe("skill a used");
    expect(line("skill.proposed", { id: "p", name: "x", origin: "agent" })).toBe("proposed skill: x (awaiting review)");
    expect(line("skill.decided", { id: "p", accepted: true, name: "x", version: 2 })).toBe("skill proposal accepted as x v2");
    expect(line("skill.decided", { id: "p", accepted: false, name: "x", version: null })).toBe("skill proposal rejected");
    expect(line("step", { type: "skills_failed", error: "disk full" })).toBe("skills not given to this session: disk full");
  });

  it("words versions, withdrawals, lost first turns and a session's failed bundle", () => {
    const line = (kind: string, p: Record<string, unknown>) => feedItem(ev(1, "t1", kind, p));
    expect(line("skill.injected", { session_id: "s", names: ["a", "b"], versions: [2, 1] })?.text).toBe("skills given to this session: a v2, b v1");
    expect(line("skill.withdrawn", { session_id: "s", names: ["a", "b"] })?.text).toBe(
      "skills withdrawn from this conversation (retired or changed since): a, b",
    );
    expect(line("step", { type: "skills_withdrawn", names: ["a"] })?.text).toBe(
      "the agent was told to stop following withdrawn skills and their files were removed: a",
    );
    const lost = line("step", { type: "skills_maybe_lost", error: "timed out" });
    expect(lost?.text).toBe("the first turn carrying this session's skills may not have reached the agent (not sent again): timed out");
    expect(lost?.warn).toBe(true);
    const failed = line("skill.unavailable", { session_id: "s", reason: "the skills could not be written for this session: disk full" });
    expect(failed?.text).toBe("skills taken back from this session: the skills could not be written for this session: disk full");
    expect(failed?.warn).toBe(true);
  });

  it("a withdrawal or a failed bundle takes the names back from what the session got", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, "t1", "skill.injected", { session_id: "s1", names: ["a", "b"] }) });
    s = reduce(s, { type: "event", event: ev(7, "t1", "skill.withdrawn", { session_id: "s1", names: ["a"] }) });
    expect(s.sessionSkills.t1.names).toEqual(["b"]);
    expect(s.skillTaskVersion.t1).toBe(2);
    s = reduce(s, { type: "event", event: ev(8, "t1", "skill.unavailable", { session_id: "s1", reason: "x" }) });
    expect(s.sessionSkills.t1.names).toEqual([]);
    // Another session's failure leaves this one alone.
    s = reduce(s, { type: "event", event: ev(9, "t1", "skill.injected", { session_id: "s2", names: ["c"] }) });
    s = reduce(s, { type: "event", event: ev(10, "t1", "skill.unavailable", { session_id: "s1", reason: "x" }) });
    expect(s.sessionSkills.t1.names).toEqual(["c"]);
  });

  it("skillsRev follows skillsVersion only when the cockpit settles it", () => {
    let s = connected();
    s = reduce(s, { type: "event", event: ev(6, null, "skill.proposed", {}) });
    s = reduce(s, { type: "event", event: ev(7, null, "skill.proposed", {}) });
    expect([s.skillsVersion, s.skillsRev]).toEqual([2, 0]);
    s = reduce(s, { type: "skillsSettled" });
    expect(s.skillsRev).toBe(2);
    expect(reduce(s, { type: "skillsSettled" })).toBe(s);
  });
});

describe("no-op updates keep the state (nothing re-renders)", () => {
  it("a task.get that changed nothing", () => {
    const s = connected();
    expect(reduce(s, { type: "task", view: { ...s.tasks.t1 } })).toBe(s);
    expect(reduce(s, { type: "task", view: { ...s.tasks.t1, status: "working" } })).not.toBe(s);
  });

  it("a task.list that changed nothing, and one that changed a row keeps the others' objects", () => {
    const s = connected(initialState, [task("t1"), task("t2")]);
    expect(reduce(s, { type: "tasks", views: [{ ...s.tasks.t1 }, { ...s.tasks.t2 }] })).toBe(s);
    const r = reduce(s, { type: "tasks", views: [{ ...s.tasks.t1 }, { ...s.tasks.t2, status: "working" }] });
    expect(r.tasks.t1).toBe(s.tasks.t1);
    expect(r.tasks.t2.status).toBe("working");
  });

  it("a queue, permission list or history that brings nothing new", () => {
    let s = connected();
    s = reduce(s, { type: "queue", taskId: "t1", list: [{ id: "q1", text: "x" }] });
    expect(reduce(s, { type: "queue", taskId: "t1", list: [{ id: "q1", text: "x" }] })).toBe(s);
    const perm = { id: "p1", task_id: "t1", session_id: "s1", tool: "Bash", detail: "ls", status: "pending" as const, created_at: 1, answered_at: null, task_title: "", branch: "feat/t1" };
    s = reduce(s, { type: "permissions", list: [perm] });
    expect(reduce(s, { type: "permissions", list: [{ ...perm }] })).toBe(s);
    const events = [ev(6, "t1", "turn_ended")];
    s = reduce(s, { type: "history", taskId: "t1", events });
    expect(reduce(s, { type: "history", taskId: "t1", events })).toBe(s);
  });
});
