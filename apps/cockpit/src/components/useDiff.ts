import { useCallback, useEffect, useState } from "react";
import { useBoot, useEnv } from "../env";
import { M, type TaskDiff } from "../lib/protocol";

export type DiffState = { state: "loading" } | { state: "ready"; diff: TaskDiff } | { state: "error"; error: string };

/** Diffs fetched lately, by task, version and daemon boot: Home's cards mount again on every
 * visit and would each fetch a full patch. */
export const DIFF_CACHE_MAX = 32;
const cache = new Map<string, TaskDiff>();

const cacheKey = (taskId: string, version: number, boot: string | null) => `${boot ?? ""}\u0000${taskId}\u0000${version}`;

function remember(key: string, diff: TaskDiff) {
  cache.delete(key);
  cache.set(key, diff);
  while (cache.size > DIFF_CACHE_MAX) cache.delete(cache.keys().next().value!);
}

/** Forgets every cached diff (tests). */
export function clearDiffCache(): void {
  cache.clear();
}

/**
 * `task.diff`, refetched when `version` changes (e.g. the number of finished turns), when the
 * daemon restarts, or on `retry()`. A diff fetched for the same (task, version) is shown at
 * once; `revalidate` fetches it again behind it (the Diff tab: files may change between
 * turns), else the cached one stands (a card). While a refetch for the same task runs the
 * last diff stays up; another task starts at "loading", so one task's diff is never shown
 * under another.
 */
export function useDiff(taskId: string, version: number, revalidate = false): DiffState & { retry(): void } {
  const { rpc } = useEnv();
  const boot = useBoot();
  const key = cacheKey(taskId, version, boot);
  const [nonce, setNonce] = useState(0);
  const [s, setS] = useState<{ taskId: string; diff: DiffState }>(() => {
    const hit = cache.get(key);
    return { taskId, diff: hit ? { state: "ready", diff: hit } : { state: "loading" } };
  });
  useEffect(() => {
    const hit = cache.get(key);
    if (hit) setS((cur) => (cur.taskId === taskId && cur.diff.state === "ready" && cur.diff.diff === hit ? cur : { taskId, diff: { state: "ready", diff: hit } }));
    if (hit && !revalidate && nonce === 0) return;
    let live = true;
    rpc<TaskDiff>(M.taskDiff, { task: taskId })
      .then((diff) => {
        remember(key, diff);
        if (live) setS({ taskId, diff: { state: "ready", diff } });
      })
      .catch((e) => live && setS({ taskId, diff: { state: "error", error: e instanceof Error ? e.message : String(e) } }));
    return () => {
      live = false;
    };
  }, [rpc, taskId, key, revalidate, nonce]);
  const retry = useCallback(() => setNonce((n) => n + 1), []);
  const shown: DiffState = s.taskId === taskId ? s.diff : { state: "loading" };
  return { ...shown, retry } as DiffState & { retry(): void };
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
