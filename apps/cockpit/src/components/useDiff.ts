import { useEffect, useState } from "react";
import { useEnv } from "../env";
import { M, type TaskDiff } from "../lib/protocol";

export type DiffState = { state: "loading" } | { state: "ready"; diff: TaskDiff } | { state: "error"; error: string };

/** `task.diff`, refetched when `version` changes (e.g. the number of finished turns). */
export function useDiff(taskId: string, version: number): DiffState {
  const { rpc } = useEnv();
  const [s, setS] = useState<DiffState>({ state: "loading" });
  useEffect(() => {
    let live = true;
    rpc<TaskDiff>(M.taskDiff, { task: taskId })
      .then((diff) => live && setS({ state: "ready", diff }))
      .catch((e) => live && setS({ state: "error", error: e instanceof Error ? e.message : String(e) }));
    return () => {
      live = false;
    };
  }, [rpc, taskId, version]);
  return s;
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
