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

export function diffTotals(d: TaskDiff): { files: number; added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const f of d.files) {
    added += f.added ?? 0;
    removed += f.removed ?? 0;
  }
  return { files: d.files.length + d.untracked.length, added, removed };
}
