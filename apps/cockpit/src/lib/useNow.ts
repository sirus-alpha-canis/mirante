// A clock for "elapsed" labels: one shared timer per period for every component reading it,
// so twenty working rows do not run twenty timers. Re-renders only its readers.
import { useSyncExternalStore } from "react";

interface Clock {
  now: number;
  listeners: Set<() => void>;
  timer: ReturnType<typeof setInterval> | null;
  /** One function per clock, never one per render: React resubscribes whenever `subscribe`
   * changes, the resubscribe restarted the clock and read a new `Date.now()`, and that
   * re-rendered (and resubscribed) again, without end once a render takes a millisecond. */
  subscribe: (l: () => void) => () => void;
}

const clocks = new Map<number, Clock>();

function clock(period: number): Clock {
  const known = clocks.get(period);
  if (known) return known;
  const c: Clock = {
    now: Date.now(),
    listeners: new Set(),
    timer: null,
    subscribe: (l) => {
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
  };
  clocks.set(period, c);
  return c;
}

/** `Date.now()`, refreshed every `period` ms while something reads it. */
export function useNow(period = 30_000): number {
  const c = clock(period);
  return useSyncExternalStore(c.subscribe, () => c.now);
}
