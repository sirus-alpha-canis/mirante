// Store hooks for skills views: refetch counters (re-rendering only when they change) and
// the pending-proposal count every view shares.
import { useEffect, useState, useSyncExternalStore } from "react";
import { useEnv } from "../env";
import type { Rpc } from "./dna";
import { skillPending } from "./skills";

/** `skillsRev`: library, proposal, learning and origin events, a burst counted once. */
export function useSkillsVersion(): number {
  const { cockpit } = useEnv();
  return useSyncExternalStore(cockpit.subscribe, () => cockpit.getState().skillsRev);
}

/** `skillsRev` plus the task's own skill events (injected, refused, unavailable, used, withdrawn). */
export function useTaskSkillsVersion(task: string): number {
  const { cockpit } = useEnv();
  return useSyncExternalStore(cockpit.subscribe, () => {
    const s = cockpit.getState();
    return s.skillsRev + (s.skillTaskVersion[task] ?? 0);
  });
}

/** One `skill.pending` per rpc and `skillsRev`, shared by the rail, the header and the queue. */
const pendingCache = new WeakMap<Rpc, { rev: number; nonce: number; answer: Promise<number | null> }>();

/** How many proposals wait (every client's), or null (not known yet, or a daemon that
 * cannot count them). `nonce`: a view's own reason to ask again (it just decided one). */
export function usePendingSkills(nonce = 0): number | null {
  const { rpc } = useEnv();
  const rev = useSkillsVersion();
  const [pending, setPending] = useState<number | null>(null);
  useEffect(() => {
    let live = true;
    let c = pendingCache.get(rpc);
    if (!c || c.rev !== rev || c.nonce < nonce) {
      c = { rev, nonce, answer: skillPending(rpc).catch(() => null) };
      pendingCache.set(rpc, c);
    }
    c.answer.then((n) => live && setPending(n));
    return () => {
      live = false;
    };
  }, [rpc, rev, nonce]);
  return pending;
}
