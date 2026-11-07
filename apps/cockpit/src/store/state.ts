// The client store: one reducer over (snapshot | event | refreshed view | history). The
// daemon's event stream is a hint (seq-ordered, may replay); task views fetched with
// `task.get` / `task.list` are the authority and overwrite what events guessed.
import type { DaemonEvent, PermissionView, SendMode, Snapshot, TaskView } from "../lib/protocol";
import { describeMemoryEvent, isMemoryEvent } from "../lib/memguard";

export type FeedKind = "tool" | "command" | "agent_message" | "message" | "turn" | "system" | "permission";

export interface FeedItem {
  seq: number;
  ts: number;
  kind: FeedKind;
  text: string;
  ok?: boolean;
  mode?: SendMode;
  phase?: "started" | "ended";
}

export type PermState = "pending" | "allowed_once" | "denied" | "expired";

export interface Perm {
  id: string;
  task_id: string;
  tool: string;
  detail: string;
  status: PermState;
  created_at: number;
  /** seq of the last event that changed it (older replays never win). */
  seq: number;
}

export interface MemoryNotice {
  seq: number;
  task_id: string | null;
  kind: string;
  text: string;
}

export type Connection =
  | { state: "connecting" }
  | { state: "live" }
  | { state: "down"; error: string; retryMs: number };

export interface State {
  connection: Connection;
  bootId: string | null;
  seq: number;
  tasks: Record<string, TaskView>;
  feed: Record<string, FeedItem[]>;
  perms: Record<string, Perm>;
  memory: MemoryNotice[];
}

export const FEED_CAP = 300;

export const initialState: State = {
  connection: { state: "connecting" },
  bootId: null,
  seq: 0,
  tasks: {},
  feed: {},
  perms: {},
  memory: [],
};

export type Action =
  | { type: "connected"; snapshot: Snapshot }
  | { type: "disconnected"; error: string; retryMs: number }
  | { type: "event"; event: DaemonEvent }
  | { type: "task"; view: TaskView }
  | { type: "tasks"; views: TaskView[] }
  | { type: "permissions"; list: PermissionView[] }
  | { type: "history"; taskId: string; events: DaemonEvent[] };

const str = (v: unknown) => (typeof v === "string" ? v : v == null ? "" : String(v));

/** The feed line an event adds, if any. */
export function feedItem(e: DaemonEvent): FeedItem | null {
  const p = e.payload ?? {};
  const base = { seq: e.seq, ts: e.ts_ms };
  switch (e.kind) {
    case "step": {
      switch (p.type) {
        case "tool":
          return { ...base, kind: "tool", text: `${str(p.tool)} ${str(p.summary)}`.trim(), ok: p.ok !== false };
        case "command":
          return { ...base, kind: "command", text: `$ ${str(p.command)}`, ok: p.exit_code === 0 };
        case "agent_message":
          return { ...base, kind: "agent_message", text: str(p.text) };
        default:
          return { ...base, kind: "system", text: [str(p.type), str(p.error)].filter(Boolean).join(": ") };
      }
    }
    case "message_queued":
      return { ...base, kind: "message", mode: "queue", text: str(p.text) };
    case "message_sent":
      return p.mode === "queue"
        ? { ...base, kind: "system", text: "queued message delivered" }
        : { ...base, kind: "message", mode: "steer", text: str(p.text) };
    case "message_delivery":
      return { ...base, kind: "system", text: `message ${str(p.delivery)}` };
    case "message_failed":
      return { ...base, kind: "system", text: `message failed: ${str(p.error)}` };
    case "queue_dropped":
      return { ...base, kind: "system", text: "queued messages dropped (session ended)" };
    case "turn_started":
      return { ...base, kind: "turn", phase: "started", text: "turn started" };
    case "turn_ended":
      return { ...base, kind: "turn", phase: "ended", text: "turn ended" };
    case "permission_requested":
      return { ...base, kind: "permission", text: `asks: ${str(p.tool)} ${str(p.detail)}`.trim() };
    case "permission_answered":
    case "permission_settled":
      return { ...base, kind: "system", text: p.answer === "allowed_once" ? "allowed once" : "denied" };
    case "permission_expired":
      return { ...base, kind: "system", text: "permission request expired" };
    case "boot":
      return p.state === "trust_prompt" || p.state === "exited"
        ? { ...base, kind: "system", text: p.state === "exited" ? "agent exited during start" : "asks to trust the folder" }
        : null;
    case "process_exited":
      return { ...base, kind: "system", text: `agent exited (code ${str(p.code) || "none"})` };
    case "interrupt_sent":
      return { ...base, kind: "system", text: "interrupted" };
    default:
      return isMemoryEvent(e.kind) ? { ...base, kind: "system", text: describeMemoryEvent(e.kind, p) } : null;
  }
}

function addFeed(feed: Record<string, FeedItem[]>, taskId: string, items: FeedItem[]): Record<string, FeedItem[]> {
  if (items.length === 0) return feed;
  const cur = feed[taskId] ?? [];
  const seen = new Set(cur.map((i) => i.seq));
  const fresh = items.filter((i) => !seen.has(i.seq));
  if (fresh.length === 0) return feed;
  const merged = [...cur, ...fresh].sort((a, b) => a.seq - b.seq).slice(-FEED_CAP);
  return { ...feed, [taskId]: merged };
}

function permEffect(perms: Record<string, Perm>, e: DaemonEvent): Record<string, Perm> {
  const p = e.payload ?? {};
  const id = str(p.permission_id);
  // An answer typed after the card was settled is audit only: the card keeps its outcome.
  // (`permission_superseded` is recorded denied by the daemon, so it settles as one.)
  if (!id || !e.task_id || e.kind === "permission_answer_unrecorded") return perms;
  const cur = perms[id];
  if (cur && cur.seq >= e.seq) return perms;
  if (e.kind === "permission_requested") {
    const status: PermState = cur?.status ?? "pending";
    return {
      ...perms,
      [id]: { id, task_id: e.task_id, tool: str(p.tool), detail: str(p.detail), status, created_at: e.ts_ms, seq: e.seq },
    };
  }
  const status: PermState =
    e.kind === "permission_expired" ? "expired" : p.answer === "allowed_once" ? "allowed_once" : "denied";
  const known = cur ?? { id, task_id: e.task_id, tool: "", detail: "", created_at: e.ts_ms, status, seq: e.seq };
  return { ...perms, [id]: { ...known, status, seq: e.seq } };
}

/** What an event says about the task row itself (refined later by `task.get`). */
function taskEffect(tasks: Record<string, TaskView>, e: DaemonEvent): Record<string, TaskView> {
  const id = e.task_id;
  const p = e.payload ?? {};
  if (!id) return tasks;
  if (e.kind === "task_created") {
    const defaults = { live: false, boot: null, needs_you: 0, queued: 0, detail: null };
    return { ...tasks, [id]: { ...defaults, ...p } as TaskView };
  }
  const t = tasks[id];
  if (!t) return tasks;
  const put = (patch: Partial<TaskView>) => ({ ...tasks, [id]: { ...t, ...patch } });
  switch (e.kind) {
    case "status":
      return put({ status: p.to as TaskView["status"] });
    case "lifecycle":
      return put({ lifecycle: p.to as TaskView["lifecycle"] });
    case "boot":
      return put({ boot: p.state as TaskView["boot"], live: p.state !== "exited" });
    case "session_started":
      return put({ live: true });
    case "session_ended":
    case "process_exited":
      return put({ live: false, boot: null });
    case "permission_requested":
      return put({ needs_you: t.needs_you + 1 });
    case "permission_answered":
    case "permission_settled":
    case "permission_expired":
      return put({ needs_you: Math.max(0, t.needs_you - 1) });
    case "message_queued":
      return put({ queued: t.queued + 1 });
    case "task_updated":
      return put({ goal: str(p.goal), notes: str(p.notes) });
    default:
      return tasks;
  }
}

export function reduce(s: State, a: Action): State {
  switch (a.type) {
    case "connected": {
      const snap = a.snapshot;
      if (snap.resumed) return { ...s, connection: { state: "live" }, bootId: snap.boot_id, seq: snap.seq };
      const tasks: Record<string, TaskView> = {};
      for (const t of snap.tasks) tasks[t.id] = t;
      return { ...s, connection: { state: "live" }, bootId: snap.boot_id, seq: snap.seq, tasks };
    }
    case "disconnected":
      return { ...s, connection: { state: "down", error: a.error, retryMs: a.retryMs } };
    case "event": {
      const e = a.event;
      if (e.boot_id === s.bootId && e.seq <= s.seq) return s;
      const item = feedItem(e);
      const memory = isMemoryEvent(e.kind)
        ? [...s.memory, { seq: e.seq, task_id: e.task_id, kind: e.kind, text: describeMemoryEvent(e.kind, e.payload ?? {}) }].slice(-20)
        : s.memory;
      return {
        ...s,
        bootId: e.boot_id,
        seq: e.seq,
        tasks: taskEffect(s.tasks, e),
        perms: e.kind.startsWith("permission_") ? permEffect(s.perms, e) : s.perms,
        feed: item && e.task_id ? addFeed(s.feed, e.task_id, [item]) : s.feed,
        memory,
      };
    }
    case "task":
      return { ...s, tasks: { ...s.tasks, [a.view.id]: a.view } };
    case "tasks": {
      const tasks: Record<string, TaskView> = {};
      for (const t of a.views) tasks[t.id] = t;
      return { ...s, tasks };
    }
    case "permissions": {
      const perms = { ...s.perms };
      for (const p of a.list) {
        const cur = perms[p.id];
        perms[p.id] = {
          id: p.id,
          task_id: p.task_id,
          tool: p.tool,
          detail: p.detail,
          status: p.status,
          created_at: p.created_at,
          seq: cur?.seq ?? 0,
        };
      }
      // A pending card the daemon no longer lists was answered elsewhere (CLI) or expired.
      for (const id of Object.keys(perms)) {
        if (perms[id].status === "pending" && !a.list.some((p) => p.id === id)) {
          perms[id] = { ...perms[id], status: "expired" };
        }
      }
      return { ...s, perms };
    }
    case "history": {
      const items = a.events.map(feedItem).filter((i): i is FeedItem => i !== null);
      let perms = s.perms;
      for (const e of a.events) if (e.kind.startsWith("permission_")) perms = permEffect(perms, e);
      return { ...s, feed: addFeed(s.feed, a.taskId, items), perms };
    }
  }
}
