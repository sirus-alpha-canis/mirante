// The store instance and its wiring to the daemon: subscribe, then on every event refresh
// the task it names with `task.get` (coalesced), reload pending permissions after
// permission events, and load each task's history (`task.events`) after a full snapshot.
import { useSyncExternalStore } from "react";
import type { DaemonEvent, PermissionView, StreamMsg, TaskView } from "../lib/protocol";
import { M } from "../lib/protocol";
import { type Action, initialState, reduce, type State } from "./state";

export interface Transport {
  rpc<T>(method: string, params?: object): Promise<T>;
  subscribe(onMsg: (m: StreamMsg) => void): Promise<void>;
}

export const REFRESH_MS = 120;

export interface Cockpit {
  getState(): State;
  subscribe(listener: () => void): () => void;
  dispatch(a: Action): void;
  connect(): Promise<void>;
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

  const refreshTask = (id: string) =>
    later(`task:${id}`, async () => dispatch({ type: "task", view: await t.rpc<TaskView>(M.taskGet, { task: id }) }));
  const loadPermissions = () =>
    later("perms", async () => dispatch({ type: "permissions", list: await t.rpc<PermissionView[]>(M.permissionList) }));
  const loadHistory = async (id: string) =>
    dispatch({ type: "history", taskId: id, events: await t.rpc<DaemonEvent[]>(M.taskEvents, { task: id, limit: 200 }) });

  const onMsg = (m: StreamMsg) => {
    switch (m.type) {
      case "connected":
        dispatch({ type: "connected", snapshot: m.snapshot });
        loadPermissions();
        if (m.snapshot.resumed) {
          // Missed events follow; the rows they touch are refreshed one by one, but rows
          // changed by events older than the stream's buffer are only right after a list.
          later("list", async () => dispatch({ type: "tasks", views: await t.rpc<TaskView[]>(M.taskList) }));
        } else {
          for (const v of m.snapshot.tasks) loadHistory(v.id).catch(() => {});
        }
        break;
      case "event":
        dispatch({ type: "event", event: m.event });
        if (m.event.task_id) refreshTask(m.event.task_id);
        if (m.event.kind.startsWith("permission_")) loadPermissions();
        break;
      case "disconnected":
        dispatch({ type: "disconnected", error: m.error, retryMs: m.retry_ms });
        break;
    }
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
    connect: () => t.subscribe(onMsg),
  };
}

/** The whole state (a stable reference until it changes); derive with useMemo. */
export function useCockpitState(c: Cockpit): State {
  return useSyncExternalStore(c.subscribe, c.getState);
}
