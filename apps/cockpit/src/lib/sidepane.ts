// The Detail's side pane, remembered per task: open or closed, its width, which side, and
// what it shows. Per viewer, in this machine's storage (the last 200 tasks); storage that
// refuses means a closed pane. Every field read back is checked: a bad one is the default.

export type PaneTarget =
  | { k: "file"; path: string; line?: number }
  | { k: "diff" }
  | { k: "steps" }
  | { k: "terminal"; task: string }
  | { k: "note"; repo: string; slug: string }
  | { k: "skill"; name: string }
  | { k: "subagent"; id: string }
  | { k: "notes" };

export interface PaneState {
  open: boolean;
  /** Percent of the area beside the task list. */
  width: number;
  target: PaneTarget | null;
  /** The pane is on the left (Swap). */
  swapped: boolean;
}

export const PANE_MIN = 25;
export const PANE_MAX = 70;
export const PANE_LRU = 200;
export const PANE_INDEX = "mirante.pane.index";
export const paneKey = (taskId: string) => `mirante.pane.${taskId}`;
export const DEFAULT_PANE: PaneState = { open: false, width: 40, target: null, swapped: false };

type KV = Pick<Storage, "getItem" | "setItem">;

export function clampWidth(w: number): number {
  if (!Number.isFinite(w)) return DEFAULT_PANE.width;
  return Math.min(PANE_MAX, Math.max(PANE_MIN, Math.round(w)));
}

const str = (v: unknown, max = 4096): v is string => typeof v === "string" && v.length > 0 && v.length <= max;

/** A stored target, checked field by field; null when it is not one. */
export function checkTarget(v: unknown): PaneTarget | null {
  if (!v || typeof v !== "object") return null;
  const o = v as Record<string, unknown>;
  switch (o.k) {
    case "file": {
      if (!str(o.path)) return null;
      const line = typeof o.line === "number" && Number.isInteger(o.line) && o.line > 0 ? o.line : undefined;
      return line ? { k: "file", path: o.path, line } : { k: "file", path: o.path };
    }
    case "diff":
    case "steps":
    case "notes":
      return { k: o.k };
    case "terminal":
      return str(o.task, 200) ? { k: "terminal", task: o.task } : null;
    case "note":
      return str(o.repo) && str(o.slug, 200) ? { k: "note", repo: o.repo, slug: o.slug } : null;
    case "skill":
      return str(o.name, 200) ? { k: "skill", name: o.name } : null;
    case "subagent":
      return str(o.id, 64) ? { k: "subagent", id: o.id } : null;
    default:
      return null;
  }
}

/** Whether `target` may show in the pane of `taskId`: never the task's own terminal (it
 * stays the main view), never the terminal of a task that is gone or archived. */
export function targetAllowed(target: PaneTarget, taskId: string, taskAlive: (id: string) => boolean): boolean {
  if (target.k !== "terminal") return true;
  return target.task !== taskId && taskAlive(target.task);
}

/** The pane of `taskId` as saved; `taskAlive` says whether another task still exists. */
export function loadPane(store: KV, taskId: string, taskAlive: (id: string) => boolean): PaneState {
  let raw: unknown;
  try {
    raw = JSON.parse(store.getItem(paneKey(taskId)) ?? "null");
  } catch {
    return DEFAULT_PANE;
  }
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return DEFAULT_PANE;
  const o = raw as Record<string, unknown>;
  const width = typeof o.width === "number" ? clampWidth(o.width) : DEFAULT_PANE.width;
  const swapped = o.swapped === true;
  let target = checkTarget(o.target);
  if (target && !targetAllowed(target, taskId, taskAlive)) target = null;
  // An open pane with nothing in it stays open; one whose target was dropped is closed.
  const dropped = o.target != null && target === null;
  return { open: o.open === true && !dropped, width, target, swapped };
}

function readIndex(store: KV): string[] {
  try {
    const v = JSON.parse(store.getItem(PANE_INDEX) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** Remembers the pane of `taskId`; the least recently saved past 200 are forgotten. */
export function savePane(store: KV, taskId: string, state: PaneState): void {
  try {
    const index = readIndex(store).filter((id) => id !== taskId);
    index.push(taskId);
    for (const old of index.splice(0, Math.max(0, index.length - PANE_LRU))) store.setItem(paneKey(old), "null");
    store.setItem(PANE_INDEX, JSON.stringify(index));
    store.setItem(paneKey(taskId), JSON.stringify(state));
  } catch {
    // Not remembered.
  }
}
