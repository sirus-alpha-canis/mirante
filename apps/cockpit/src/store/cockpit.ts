// The store instance and its wiring to the daemon: subscribe, then on every event refresh
// the task it names with `task.get` (coalesced), reload pending permissions after
// permission events, and load each task's history (`task.events`) after a full snapshot.
// Replies can arrive out of order: each one is dropped when a newer one for the same thing
// was already applied.
import { useRef, useSyncExternalStore } from "react";
import type { DaemonEvent, PermissionView, QueuedMessage, StreamMsg, TaskView } from "../lib/protocol";
import { M, RpcCode } from "../lib/protocol";
import { type Action, initialState, reduce, type State } from "./state";

export interface Transport {
  rpc<T>(method: string, params?: object): Promise<T>;
  subscribe(onMsg: (m: StreamMsg) => void): Promise<void>;
}

export const REFRESH_MS = 120;
/** History loads in flight at once after a snapshot (the daemon serves 64 connections). */
export const HISTORY_CONCURRENCY = 4;
/** Retries of a history load the daemon refused as busy (or could not take), with backoff. */
export const HISTORY_RETRIES = 3;
export const HISTORY_BACKOFF_MS = 250;

/** The bridge error inside `e` (a `DaemonError` carries it as `info`). */
function bridgeError(e: unknown): { kind?: string; code?: number; message?: string } {
  if (!e || typeof e !== "object") return { message: String(e) };
  const o = e as { info?: object };
  return (o.info ?? e) as { kind?: string; code?: number; message?: string };
}

/** Worth retrying: the daemon is at its connection limit, or the connection failed. */
function transient(e: unknown): boolean {
  const b = bridgeError(e);
  return (b.kind === "rpc" && b.code === RpcCode.BUSY) || b.kind === "disconnected";
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Runs `jobs` with at most `n` at once. */
async function pool(jobs: (() => Promise<void>)[], n: number): Promise<void> {
  let next = 0;
  const worker = async () => {
    while (next < jobs.length) await jobs[next++]();
  };
  await Promise.all(Array.from({ length: Math.min(n, jobs.length) }, worker));
}

/** Events after which a task's queue may differ from what was loaded. */
function touchesQueue(e: DaemonEvent): boolean {
  switch (e.kind) {
    case "queue_changed":
    case "message_queued":
    case "message_requeued":
    case "queue_dropped":
      return true;
    case "message_sent":
      return e.payload?.mode === "queue";
    default:
      return false;
  }
}

export interface Cockpit {
  getState(): State;
  subscribe(listener: () => void): () => void;
  dispatch(a: Action): void;
  /** Subscribes to the daemon's stream; later calls share the first (idempotent). */
  connect(): Promise<void>;
  /** Reloads a task's queued messages (`message.list_queued`), coalesced like refreshes. */
  refreshQueue(taskId: string): void;
  /** Called after each state change with (previous, next). */
  onChange(fn: (prev: State, next: State) => void): () => void;
}

export function createCockpit(t: Transport): Cockpit {
  let state = initialState;
  const listeners = new Set<() => void>();
  const watchers = new Set<(prev: State, next: State) => void>();
  const timers = new Map<string, ReturnType<typeof setTimeout>>();

  const dispatch = (a: Action) => {
    const prev = state;
    state = reduce(state, a);
    if (state === prev) return;
    for (const w of watchers) w(prev, state);
    for (const l of listeners) l();
  };

  const later = (key: string, fn: () => Promise<void>) => {
    if (timers.has(key)) return;
    timers.set(
      key,
      setTimeout(() => {
        timers.delete(key);
        fn().catch(() => {});
      }, REFRESH_MS),
    );
  };

  // Request numbers, in send order; per key, the number of the reply last applied.
  let sent = 0;
  const applied = new Map<string, number>();
  const fresh = (key: string, n: number) => (applied.get(key) ?? 0) < n;

  const refreshTask = (id: string) =>
    later(`task:${id}`, async () => {
      const n = ++sent;
      const view = await t.rpc<TaskView>(M.taskGet, { task: id });
      if (!fresh(`task:${id}`, n)) return;
      applied.set(`task:${id}`, n);
      dispatch({ type: "task", view });
    });
  const refreshList = () =>
    later("list", async () => {
      const n = ++sent;
      const views = await t.rpc<TaskView[]>(M.taskList);
      if (!fresh("list", n)) return;
      applied.set("list", n);
      // Rows a `task.get` sent after this list already refreshed keep that newer view.
      const keep = views.map((v) => v.id).filter((id) => !fresh(`task:${id}`, n));
      for (const v of views) if (fresh(`task:${v.id}`, n)) applied.set(`task:${v.id}`, n);
      dispatch({ type: "tasks", views, keep });
    });
  const refreshQueue = (id: string) =>
    later(`queue:${id}`, async () => {
      const n = ++sent;
      const list = await t.rpc<QueuedMessage[]>(M.messageListQueued, { task_id: id });
      if (!fresh(`queue:${id}`, n)) return;
      applied.set(`queue:${id}`, n);
      dispatch({ type: "queue", taskId: id, list: Array.isArray(list) ? list : [] });
    });
  const loadPermissions = () =>
    later("perms", async () => {
      const rev = state.permRev;
      const list = await t.rpc<PermissionView[]>(M.permissionList);
      dispatch({ type: "permissions", list, rev });
    });
  const loadHistory = async (id: string) => {
    for (let attempt = 0; ; attempt++) {
      try {
        const events = await t.rpc<DaemonEvent[]>(M.taskEvents, { task: id, limit: 200 });
        dispatch({ type: "history", taskId: id, events });
        return;
      } catch (e) {
        if (attempt < HISTORY_RETRIES && transient(e)) {
          await sleep(HISTORY_BACKOFF_MS * 2 ** attempt);
          continue;
        }
        dispatch({ type: "historyFailed", taskId: id, error: bridgeError(e).message ?? String(e) });
        return;
      }
    }
  };

  const onMsg = (m: StreamMsg) => {
    switch (m.type) {
      case "connected":
        dispatch({ type: "connected", snapshot: m.snapshot });
        loadPermissions();
        if (m.snapshot.resumed) {
          // Missed events follow; the rows they touch are refreshed one by one, but rows
          // changed by events older than the stream's buffer are only right after a list.
          refreshList();
        } else {
          const ids = m.snapshot.tasks.map((v) => v.id);
          pool(ids.map((id) => () => loadHistory(id)), HISTORY_CONCURRENCY).catch(() => {});
        }
        break;
      case "event":
        dispatch({ type: "event", event: m.event });
        if (m.event.task_id) refreshTask(m.event.task_id);
        if (m.event.kind.startsWith("permission_")) loadPermissions();
        if (m.event.task_id && touchesQueue(m.event)) refreshQueue(m.event.task_id);
        break;
      case "disconnected":
        dispatch({ type: "disconnected", error: m.error, retryMs: m.retry_ms });
        break;
    }
  };

  // One subscription per store: React StrictMode runs the connecting effect twice, and each
  // `subscribe` would open a stream (the shell replaces the old one, but not for free). A
  // failed subscribe is forgotten so the next call can try again.
  let subscribed: Promise<void> | null = null;
  const connect = () => {
    if (!subscribed) {
      subscribed = t.subscribe(onMsg).catch((e) => {
        subscribed = null;
        throw e;
      });
    }
    return subscribed;
  };

  return {
    getState: () => state,
    subscribe(l) {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    onChange(fn) {
      watchers.add(fn);
      return () => watchers.delete(fn);
    },
    dispatch,
    refreshQueue,
    connect,
  };
}


/**
 * What `select` picks from the store, re-rendering only when that changes (`equal`, by
 * default `Object.is`): a component reading one task's feed does not render for another
 * task's event. A selector that builds a new array or object each time needs a shallow
 * `equal` (see `shallow` in env.tsx).
 */
export function useCockpitSelect<T>(c: Cockpit, select: (s: State) => T, equal: (a: T, b: T) => boolean = Object.is): T {
  const last = useRef<{ state: State; select: (s: State) => T; value: T } | null>(null);
  const snapshot = () => {
    const state = c.getState();
    const prev = last.current;
    if (prev && prev.state === state && prev.select === select) return prev.value;
    const picked = select(state);
    const value = prev && equal(prev.value, picked) ? prev.value : picked;
    last.current = { state, select, value };
    return value;
  };
  return useSyncExternalStore(c.subscribe, snapshot);
}
