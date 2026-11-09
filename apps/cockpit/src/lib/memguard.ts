// The daemon's memory guard as the UI sees it (Plan 2b Tasks 18-19): per-session memory on
// TaskView (`memory`, `hibernated`), the machine's view on `daemon.status.memory`, the
// `session.hibernate` / `session.wake` calls and the `memory.*` events. An older daemon has
// none of these: the panel then says "not available" (only once a status has loaded; with
// no status at all it says "unknown") and the actions answer
// METHOD_NOT_FOUND, which the UI reports instead of failing.
import { type Key, type Params, t } from "./i18n";
import type { DaemonStatus, TaskView } from "./protocol";
import { M } from "./protocol";

export const HIBERNATE = M.sessionHibernate;
export const WAKE = M.sessionWake;

/** "unknown": a level this UI does not know (a newer daemon) — shown as such, never guessed. */
export type Pressure = "normal" | "elevated" | "critical" | "unknown";

export interface SystemMemory {
  level: Pressure;
  /** New sessions and queued deliveries wait (only when the person set the guard to pause). */
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

/** A pressure level in words (an unknown one says so). */
function levelText(l: Pressure): string {
  return t(l === "normal" ? "mem.level.normal" : l === "elevated" ? "mem.level.elevated" : l === "critical" ? "mem.level.critical" : "mem.level.unknown");
}

/** Why new sessions wait (or that they do not), from the daemon's numbers only; the room
 * left is secondary: what the OS calls available (macOS counts the file cache it gives
 * back), else the strict free pages from an older daemon. */
export function pressureText(m: SystemMemory): { main: string; secondary: string } {
  const swap = m.swapTotal > 0 ? t("mem.swap", { pct: Math.round((m.swapUsed * 100) / m.swapTotal) }) : null;
  const who = m.kernelLevel ? (KERNEL_NAME[m.kernelSource ?? ""] ?? t("mem.theSystem")) : null;
  const parts: string[] = [];
  if (!m.paused) {
    parts.push(who ? t("mem.pressureBy", { level: levelText(m.level), who }) : t("mem.pressure", { level: levelText(m.level) }));
    if (swap) parts.push(swap);
  } else if (who && m.kernelLevel === "normal") {
    parts.push(t("mem.normalAgain", { who }));
    if (swap) parts.push(swap);
    parts.push(t("mem.pausedSettles"));
  } else if (who) {
    parts.push(t(m.kernelLevel === "critical" ? "mem.reportedCritical" : "mem.reported", { who }));
    if (swap) parts.push(swap);
    parts.push(t("mem.pausedEases"));
  } else {
    parts.push(swap ? t("mem.tightSwap", { swap }) : t("mem.tight"));
    parts.push(t("mem.pausedEases"));
  }
  const room = m.available !== null ? t("mem.available", { size: size(m.available), total: size(m.total) }) : t("mem.free", { size: size(m.free), total: size(m.total) });
  return { main: parts.join(" · "), secondary: room };
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

/** What a memory event says, as a key and its parameters (so a line can be said again in
 * another language). Daemon words (states, reasons) pass through. */
export function memoryMsg(kind: string, p: Record<string, unknown>): { key: Key; params?: Params } | null {
  const reason = typeof p.reason === "string" ? `: ${p.reason}` : "";
  switch (kind) {
    case "memory.warn":
      return { key: "mem.ev.warn", params: { state: typeof p.state === "string" ? p.state : t("mem.ev.warning"), bytes: mb(p.bytes), budget: mb(p.budget) } };
    case "memory.level":
      return { key: "mem.ev.level", params: { to: p.to == null ? t("mem.ev.changed") : String(p.to) } };
    case "memory.paused":
      return { key: "mem.ev.paused" };
    case "memory.resumed":
      return { key: "mem.ev.resumed" };
    case "memory.offer_hibernate":
      return { key: "mem.ev.offerHibernate", params: { reason } };
    case "memory.hibernated":
      return { key: "mem.ev.hibernated", params: { bytes: mb(p.bytes) } };
    case "memory.auto_hibernated":
      return { key: "mem.ev.autoHibernated", params: { reason } };
    case "memory.refused":
      return { key: "mem.ev.refused", params: { reason } };
    case "memory.leak":
      return { key: "mem.ev.leak", params: { rate: String(p.mb_per_min ?? "?") } };
    default:
      return null;
  }
}

/** One line for the feed / memory panel (an unknown kind is shown as it is). */
export function describeMemoryEvent(kind: string, p: Record<string, unknown>): string {
  const m = memoryMsg(kind, p);
  return m ? t(m.key, m.params) : kind;
}

export function mb(v: unknown): string {
  return typeof v === "number" ? `${Math.round(v / (1 << 20))} MB` : "?";
}
