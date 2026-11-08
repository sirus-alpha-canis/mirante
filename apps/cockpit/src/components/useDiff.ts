import { useEffect, useState } from "react";
import { useEnv } from "../env";
import { M, type TaskDiff } from "../lib/protocol";

export type DiffState = { state: "loading" } | { state: "ready"; diff: TaskDiff } | { state: "error"; error: string };

/**
 * `task.diff`, refetched when `version` changes (e.g. the number of finished turns). While a
 * refetch for the same task runs the last diff stays up; another task starts at "loading", so
 * one task's diff is never shown under another.
 */
export function useDiff(taskId: string, version: number): DiffState {
  const { rpc } = useEnv();
  const [s, setS] = useState<{ taskId: string; diff: DiffState }>({ taskId, diff: { state: "loading" } });
  useEffect(() => {
    let live = true;
    rpc<TaskDiff>(M.taskDiff, { task: taskId })
      .then((diff) => live && setS({ taskId, diff: { state: "ready", diff } }))
      .catch((e) => live && setS({ taskId, diff: { state: "error", error: e instanceof Error ? e.message : String(e) } }));
    return () => {
      live = false;
    };
  }, [rpc, taskId, version]);
  return s.taskId === taskId ? s.diff : { state: "loading" };
}

/** Changed (tracked) files with their line counts, and untracked files apart: git has no
 * line counts for those, so they are never shown as "+0 −0". */
export function diffTotals(d: TaskDiff): { changed: number; untracked: number; added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const f of d.files) {
    added += f.added ?? 0;
    removed += f.removed ?? 0;
  }
  return { changed: d.files.length, untracked: d.untracked.length, added, removed };
}

export const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
