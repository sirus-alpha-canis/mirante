// The daemon's memory guard as the UI sees it (Plan 2b Tasks 18-19): per-session memory on
// TaskView (`memory`, `hibernated`), the machine's view on `daemon.status.memory`, the
// `session.hibernate` / `session.wake` calls and the `memory.*` events. An older daemon has
// none of these: the panel then says "not available" (only once a status has loaded; with
// no status at all it says "unknown") and the actions answer
// METHOD_NOT_FOUND, which the UI reports instead of failing.
import type { DaemonStatus, TaskView } from "./protocol";
import { M } from "./protocol";

export const HIBERNATE = M.sessionHibernate;
export const WAKE = M.sessionWake;

/** "unknown": a level this UI does not know (a newer daemon) — shown as such, never guessed. */
export type Pressure = "normal" | "elevated" | "critical" | "unknown";

export interface SystemMemory {
  level: Pressure;
  /** New sessions and queued deliveries wait (level elevated or worse). */
  paused: boolean;
  total: number;
  free: number;
  /** What the OS calls available (macOS: its free percentage of the RAM); null from an
   * older daemon (then `free` is all there is). */
  available: number | null;
  swapUsed: number;
  swapTotal: number;
  /** The OS's own measure when it set the level ("normal" | "warn" | "critical"); null when
   * the daemon judged from swap and free memory, or is older. */
  kernelLevel: string | null;
  /** "macos" or "linux-psi", with `kernelLevel`. */
  kernelSource: string | null;
}

export function systemMemory(status: DaemonStatus | null): SystemMemory | null {
  const m = status?.memory;
  if (!m) return null;
  const level: Pressure = m.level === "normal" || m.level === "elevated" || m.level === "critical" ? m.level : "unknown";
  return {
    level,
    paused: m.paused,
    total: m.total,
    free: m.free,
    available: typeof m.available === "number" ? m.available : null,
    swapUsed: m.swap_used,
    swapTotal: m.swap_total,
    kernelLevel: m.kernel_level ?? null,
    kernelSource: m.kernel_level ? (m.kernel_source ?? null) : null,
  };
}

const KERNEL_NAME: Record<string, string> = { macos: "macOS", "linux-psi": "Linux (PSI)" };

/** Why new sessions wait (or that they do not), from the daemon's numbers only; the room
 * left is secondary: what the OS calls available (macOS counts the file cache it gives
 * back), else the strict free pages from an older daemon. */
export function pressureText(m: SystemMemory): { main: string; secondary: string } {
  const swap = m.swapTotal > 0 ? `swap ${Math.round((m.swapUsed * 100) / m.swapTotal)}%` : null;
  const who = m.kernelLevel ? (KERNEL_NAME[m.kernelSource ?? ""] ?? "The system") : null;
  const parts: string[] = [];
  if (!m.paused) {
    parts.push(`Memory pressure ${m.level}${who ? ` (${who})` : ""}`);
    if (swap) parts.push(swap);
  } else if (who && m.kernelLevel === "normal") {
    parts.push(`${who} reports normal memory pressure again`);
    if (swap) parts.push(swap);
    parts.push("new sessions paused until it settles");
  } else if (who) {
    parts.push(`${who} reported ${m.kernelLevel === "critical" ? "critical " : ""}memory pressure`);
    if (swap) parts.push(swap);
    parts.push("new sessions paused until it eases");
  } else {
    parts.push(swap ? `Memory tight (${swap} used, little free)` : "Memory tight (little free memory)");
    parts.push("new sessions paused until it eases");
  }
  const room = m.available !== null ? `${size(m.available)} available` : `${size(m.free)} free`;
  return { main: parts.join(" · "), secondary: `${room} of ${size(m.total)}` };
}

/** Bytes as MB, or GB with one decimal from 1 GB. */
export function size(v: number): string {
  return v >= 1 << 30 ? `${(v / (1 << 30)).toFixed(1)} GB` : mb(v);
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
