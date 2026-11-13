// Archiving a project's tasks (Remove from Projects): each live session stopped first (the
// daemon refuses to archive a live one), then `task.archive`, which removes the worktree and
// keeps the branch. The daemon refuses a dirty worktree; those come back with its reason.
import type { Env } from "../env";
import { DaemonError } from "./daemon";
import { M, RpcCode, type TaskView } from "./protocol";

export interface ArchiveFailure {
  id: string;
  title: string;
  branch: string;
  reason: string;
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** `tasks` with each thread or sub-task before its parent (when both are there): the daemon
 * refuses to archive a parent whose children are still open. */
export function childrenFirst(tasks: TaskView[]): TaskView[] {
  const ids = new Set(tasks.map((t) => t.id));
  const isChild = (t: TaskView) => !!t.parent_id && ids.has(t.parent_id);
  return [...tasks.filter(isChild), ...tasks.filter((t) => !isChild(t))];
}

/** Archives `tasks` one at a time (git works on one worktree at a time), children before
 * their parents; the ones that could not be, with why. Each archived view goes into the
 * store at once. */
export async function archiveTasks(env: Pick<Env, "rpc" | "cockpit">, tasks: TaskView[]): Promise<ArchiveFailure[]> {
  const failed: ArchiveFailure[] = [];
  for (const t of childrenFirst(tasks)) {
    const fail = (e: unknown) => failed.push({ id: t.id, title: t.title, branch: t.branch, reason: message(e) });
    if (t.live) {
      try {
        const v = await env.rpc<TaskView>(M.sessionStop, { task: t.id });
        if (v) env.cockpit.dispatch({ type: "task", view: v });
      } catch (e) {
        // Ended meanwhile (no live session to stop): archiving can go on.
        if (!(e instanceof DaemonError && e.code === RpcCode.CONFLICT)) {
          fail(e);
          continue;
        }
      }
    }
    try {
      const v = await env.rpc<TaskView>(M.taskArchive, { task: t.id });
      if (v) env.cockpit.dispatch({ type: "task", view: v });
    } catch (e) {
      fail(e);
    }
  }
  return failed;
}

/** `task.archive` of one task from its Detail: with `children` (its open threads and
 * sub-tasks, which the person saw listed), a cascade; live sessions are stopped by the
 * daemon after it checked every one (all or nothing). The archived view goes into the store. */
export async function archiveTask(env: Pick<Env, "rpc" | "cockpit">, t: TaskView, children: TaskView[]): Promise<void> {
  const params = children.length > 0 ? { task: t.id, cascade: true, stop: true } : { task: t.id, stop: true };
  const v = await env.rpc<TaskView>(M.taskArchive, params);
  if (v) env.cockpit.dispatch({ type: "task", view: v });
}
