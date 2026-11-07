// The daemon's memory guard as the UI sees it (Plan 2b Tasks 18-19): per-session memory on
// TaskView (`memory`, `hibernated`), the machine's view on `daemon.status.memory`, the
// `session.hibernate` / `session.wake` calls and the `memory.*` events. An older daemon has
// none of these: the panel then says "not available" and the actions answer
// METHOD_NOT_FOUND, which the UI reports instead of failing.
import type { DaemonStatus, TaskView } from "./protocol";
import { M } from "./protocol";

export const HIBERNATE = M.sessionHibernate;
export const WAKE = M.sessionWake;

export type Pressure = "normal" | "elevated" | "critical";

export interface SystemMemory {
  level: Pressure;
  /** New sessions and queued deliveries wait (level elevated or worse). */
  paused: boolean;
  total: number;
  free: number;
  swapUsed: number;
}

export function systemMemory(status: DaemonStatus | null): SystemMemory | null {
  const m = status?.memory;
  if (!m) return null;
  const level: Pressure = m.level === "elevated" || m.level === "critical" ? m.level : "normal";
  return { level, paused: m.paused, total: m.total, free: m.free, swapUsed: m.swap_used };
}

export interface TaskMemory {
  bytes: number;
  budget: number;
  state: "ok" | "soft" | "hard";
}

export function taskMemory(t: TaskView): TaskMemory | null {
  if (!t.memory) return null;
  const state = t.memory.state === "soft" || t.memory.state === "hard" ? t.memory.state : "ok";
  return { bytes: t.memory.bytes, budget: t.memory.budget, state };
}

export function isHibernated(t: TaskView): boolean {
  return t.hibernated === true;
}

export function isMemoryEvent(kind: string): boolean {
  return kind.startsWith("memory.");
}

/** One line for the feed / memory panel. */
export function describeMemoryEvent(kind: string, p: Record<string, unknown>): string {
  const reason = typeof p.reason === "string" ? `: ${p.reason}` : "";
  switch (kind) {
    case "memory.warn":
      return `memory ${String(p.state ?? "warning")} (${mb(p.bytes)} of ${mb(p.budget)})`;
    case "memory.level":
      return `system memory pressure ${String(p.to ?? "changed")}`;
    case "memory.paused":
      return "new sessions and queued deliveries paused (memory pressure)";
    case "memory.resumed":
      return "new sessions and queued deliveries resumed";
    case "memory.offer_hibernate":
      return `hibernate suggested${reason}`;
    case "memory.hibernated":
      return `hibernated (${mb(p.bytes)} freed)`;
    case "memory.auto_hibernated":
      return `hibernated automatically${reason}`;
    case "memory.refused":
      return `not hibernated${reason}`;
    case "memory.leak":
      return `memory keeps growing (${String(p.mb_per_min ?? "?")} MB/min while idle)`;
    default:
      return kind;
  }
}

export function mb(v: unknown): string {
  return typeof v === "number" ? `${Math.round(v / (1 << 20))} MB` : "?";
}
