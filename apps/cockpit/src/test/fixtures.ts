import type { DaemonEvent, Snapshot, TaskView } from "../lib/protocol";

export function task(id: string, over: Partial<TaskView> = {}): TaskView {
  return {
    id,
    title: `goal of ${id}`,
    repo_path: "/repos/shop",
    worktree_path: `/wt/${id}`,
    branch: `feat/${id}`,
    agent: "claude",
    goal: `goal of ${id}`,
    notes: "",
    lifecycle: "active",
    status: "idle",
    created_at: 1,
    updated_at: 1,
    live: true,
    boot: "ready",
    needs_you: 0,
    queued: 0,
    detail: null,
    ...over,
  };
}

export function snapshot(tasks: TaskView[], seq = 0, over: Partial<Snapshot> = {}): Snapshot {
  return { boot_id: "b1", seq, tasks, resumed: false, ...over };
}

let ts = 1_000;
export function ev(seq: number, task_id: string | null, kind: string, payload: Record<string, unknown> = {}, boot_id = "b1"): DaemonEvent {
  return { seq, boot_id, task_id, kind, payload, ts_ms: ts++ };
}

/** The stored task as `task_created` carries it (no live facts). */
export function taskRow(id: string, over: Partial<TaskView> = {}): Record<string, unknown> {
  const row: Record<string, unknown> = { ...task(id, over) };
  for (const k of ["live", "boot", "needs_you", "queued", "detail"]) delete row[k];
  return row;
}
