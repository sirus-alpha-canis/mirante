// Store hooks for skills views: refetch counters (re-rendering only when they change).
import { useSyncExternalStore } from "react";
import { useEnv } from "../env";

/** `skillsVersion`: library, proposal, learning and origin events. */
export function useSkillsVersion(): number {
  const { cockpit } = useEnv();
  return useSyncExternalStore(cockpit.subscribe, () => cockpit.getState().skillsVersion);
}

/** `skillsVersion` plus the task's own skill events (injected, refused, unavailable, used). */
export function useTaskSkillsVersion(task: string): number {
  const { cockpit } = useEnv();
  return useSyncExternalStore(cockpit.subscribe, () => {
    const s = cockpit.getState();
    return s.skillsVersion + (s.skillTaskVersion[task] ?? 0);
  });
}
