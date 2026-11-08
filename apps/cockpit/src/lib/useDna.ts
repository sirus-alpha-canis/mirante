// Store hooks for repo memory views: the refetch counter of a repo (re-rendering only when
// it changes, not on every event), and learning the daemon's key for a path.
import { useEffect, useSyncExternalStore } from "react";
import { useEnv } from "../env";
import type { Cockpit } from "../store/cockpit";
import { dnaRev } from "../store/state";
import { dnaStatus } from "./dna";
import type { DnaStatus } from "./protocol";

/** `dnaRev` of `repo` (0 without one). */
export function useDnaRev(repo: string | null): number {
  const { cockpit } = useEnv();
  return useSyncExternalStore(cockpit.subscribe, () => (repo ? dnaRev(cockpit.getState(), repo) : 0));
}

/** Records that `path`'s memory is `status.repo` (what `dna.*` events name). */
export function learnDnaKey(cockpit: Cockpit, path: string, status: Pick<DnaStatus, "repo"> | null | undefined) {
  if (status?.repo) cockpit.dispatch({ type: "dnaKey", path, key: status.repo });
}

/** Asks `dna.status` once for a path whose key is not known yet (a task's `repo_path` may
 * be a subdirectory or a symlinked path), so its views refetch on the repo's events. */
export function useDnaKey(path: string | null) {
  const { cockpit, rpc } = useEnv();
  const known = useSyncExternalStore(cockpit.subscribe, () => (path ? path in cockpit.getState().dnaKeys : true));
  useEffect(() => {
    if (!path || known) return;
    let live = true;
    dnaStatus(rpc, path)
      .then((r) => live && r.available && learnDnaKey(cockpit, path, r.status))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [cockpit, rpc, path, known]);
}
