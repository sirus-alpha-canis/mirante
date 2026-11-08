// A clock for "elapsed" labels: one shared timer per period for every component reading it,
// so twenty working rows do not run twenty timers. Re-renders only its readers.
import { useSyncExternalStore } from "react";

const clocks = new Map<number, { now: number; listeners: Set<() => void>; timer: ReturnType<typeof setInterval> | null }>();

function clock(period: number) {
  let c = clocks.get(period);
  if (!c) {
    c = { now: Date.now(), listeners: new Set(), timer: null };
    clocks.set(period, c);
  }
  return c;
}

/** `Date.now()`, refreshed every `period` ms while something reads it. */
export function useNow(period = 30_000): number {
  const c = clock(period);
  return useSyncExternalStore(
    (l) => {
      c.listeners.add(l);
      if (!c.timer) {
        c.now = Date.now();
        c.timer = setInterval(() => {
          c.now = Date.now();
          for (const f of c.listeners) f();
        }, period);
      }
      return () => {
        c.listeners.delete(l);
        if (c.listeners.size === 0 && c.timer) {
          clearInterval(c.timer);
          c.timer = null;
        }
      };
    },
    () => c.now,
  );
}
