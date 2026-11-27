// The Detail's side pane, remembered per task: open or closed, its width, which side, its
// tabs (and at most one split), and which tab shows. Per viewer, in this machine's storage
// (the last 200 tasks); storage that refuses means a closed pane. Every field read back is
// checked: a bad one is the default, a bad tab is dropped.

export type PaneTarget =
  | { k: "file"; path: string; line?: number }
  /** `task`: another task's Diff (a fan-out sibling's, beside this one's own). */
  | { k: "diff"; task?: string }
  | { k: "steps" }
  | { k: "terminal"; task: string }
  | { k: "note"; repo: string; slug: string }
  | { k: "skill"; name: string }
  | { k: "subagent"; id: string }
  | { k: "notes" }
  /** The person's own shell in this task's worktree, number `n` (1..MAX_SHELLS). */
  | { k: "shell"; n: number };

export interface PaneTab {
  id: string;
  t: PaneTarget;
}

/** Two tabs side by side: the active one first, `second` beside it (`row`) or below (`col`);
 * `ratio` is the first one's share. */
export interface PaneSplit {
  dir: "row" | "col";
  second: string;
  ratio: number;
}

export interface PaneState {
  open: boolean;
  /** Percent of the area beside the task list. */
  width: number;
  tabs: PaneTab[];
  /** The tab shown (null: no tabs). */
  active: string | null;
  split: PaneSplit | null;
  /** The pane is on the left (Swap). */
  swapped: boolean;
}

export const PANE_MIN = 25;
export const PANE_MAX = 70;
export const PANE_LRU = 200;
export const PANE_TABS = 8;
export const MAX_SHELLS = 4;
export const SPLIT_MIN = 0.2;
export const SPLIT_MAX = 0.8;
export const PANE_INDEX = "mirante.pane.index";
export const paneKey = (taskId: string) => `mirante.pane.${taskId}`;
export const DEFAULT_PANE: PaneState = { open: false, width: 40, tabs: [], active: null, split: null, swapped: false };

type KV = Pick<Storage, "getItem" | "setItem">;

export function clampWidth(w: number): number {
  if (!Number.isFinite(w)) return DEFAULT_PANE.width;
  return Math.min(PANE_MAX, Math.max(PANE_MIN, Math.round(w)));
}

/** A split's share for its first tab: 20..80%. */
export function clampRatio(r: number): number {
  if (!Number.isFinite(r)) return 0.5;
  return Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, r));
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
      return str(o.task, 200) ? { k: "diff", task: o.task } : { k: "diff" };
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
    case "shell":
      return typeof o.n === "number" && Number.isInteger(o.n) && o.n >= 1 && o.n <= MAX_SHELLS ? { k: "shell", n: o.n } : null;
    default:
      return null;
  }
}

/** Whether `target` may show in the pane of `taskId`: never the task's own terminal (it
 * stays the main view), never the terminal of a task that is gone or archived. */
export function targetAllowed(target: PaneTarget, taskId: string, taskAlive: (id: string) => boolean): boolean {
  if (target.k === "diff") return !target.task || target.task === taskId || taskAlive(target.task);
  if (target.k !== "terminal") return true;
  return target.task !== taskId && taskAlive(target.task);
}

/** The same thing shown (a file whatever its line; one shell by its number). */
export function sameTarget(a: PaneTarget, b: PaneTarget): boolean {
  if (a.k !== b.k) return false;
  switch (a.k) {
    case "file":
      return a.path === (b as typeof a).path;
    case "diff":
      return (a.task ?? null) === ((b as typeof a).task ?? null);
    default:
      return JSON.stringify(a) === JSON.stringify(b);
  }
}

/** A target as this task's pane keeps it (its own Diff, named, is the plain one); null when
 * it may not show here. */
function normalize(t: PaneTarget, taskId: string, taskAlive: (id: string) => boolean): PaneTarget | null {
  if (!targetAllowed(t, taskId, taskAlive)) return null;
  if (t.k === "diff" && t.task === taskId) return { k: "diff" };
  return t;
}

/** The pane of `taskId` as saved; `taskAlive` says whether another task still exists. A
 * pane saved before tabs (one `target`) comes back as one tab. */
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
  // The stored tabs, or the one target of a pane saved before tabs.
  const stored: unknown[] = Array.isArray(o.tabs) ? o.tabs : o.target != null ? [{ id: "t1", t: o.target }] : [];
  const tabs: PaneTab[] = [];
  for (const s of stored) {
    if (tabs.length >= PANE_TABS) break;
    if (!s || typeof s !== "object") continue;
    const { id, t } = s as Record<string, unknown>;
    const target = checkTarget(t);
    const kept = target && normalize(target, taskId, taskAlive);
    if (!str(id, 32) || !kept || tabs.some((x) => x.id === id || sameTarget(x.t, kept))) continue;
    tabs.push({ id, t: kept });
  }
  const has = (id: unknown): id is string => typeof id === "string" && tabs.some((t) => t.id === id);
  const active = has(o.active) ? o.active : (tabs[0]?.id ?? null);
  let split: PaneSplit | null = null;
  if (o.split && typeof o.split === "object") {
    const sp = o.split as Record<string, unknown>;
    if ((sp.dir === "row" || sp.dir === "col") && has(sp.second) && sp.second !== active) {
      split = { dir: sp.dir, second: sp.second, ratio: clampRatio(typeof sp.ratio === "number" ? sp.ratio : 0.5) };
    }
  }
  // An open pane with nothing in it stays open; one whose every tab was dropped is closed.
  const dropped = stored.length > 0 && tabs.length === 0;
  return { open: o.open === true && !dropped, width, tabs, active, split, swapped };
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

/** What the pane shows now (its active tab's target). */
export function activeTarget(p: PaneState): PaneTarget | null {
  return p.tabs.find((t) => t.id === p.active)?.t ?? null;
}

function newId(tabs: PaneTab[]): string {
  let n = tabs.length + 1;
  while (tabs.some((t) => t.id === `t${n}`)) n++;
  return `t${n}`;
}

/** The lowest shell number no tab uses; null when every one is open. */
export function freeShell(p: PaneState): number | null {
  for (let n = 1; n <= MAX_SHELLS; n++) if (!p.tabs.some((t) => t.t.k === "shell" && t.t.n === n)) return n;
  return null;
}

/** Opens `target` in the pane (and opens the pane): a tab already showing it is focused
 * (a file's line updated). Otherwise `fresh` adds a tab; else the active tab is reused,
 * unless it is a shell (a shell tab is never replaced) or there is none. Past
 * [`PANE_TABS`] the oldest tab that is neither shown nor a shell goes. */
export function openTab(p: PaneState, target: PaneTarget, fresh = false): PaneState {
  const same = p.tabs.find((t) => sameTarget(t.t, target));
  if (same) {
    const tabs = p.tabs.map((t) => (t.id === same.id ? { id: t.id, t: target } : t));
    return focusTab({ ...p, open: true, tabs }, same.id);
  }
  const current = p.tabs.find((t) => t.id === p.active);
  if (!fresh && current && current.t.k !== "shell" && target.k !== "shell") {
    const tabs = p.tabs.map((t) => (t.id === current.id ? { id: t.id, t: target } : t));
    return { ...p, open: true, tabs };
  }
  let tabs = p.tabs;
  if (tabs.length >= PANE_TABS) {
    const drop = tabs.find((t) => t.id !== p.active && t.id !== p.split?.second && t.t.k !== "shell");
    if (!drop) return { ...p, open: true };
    tabs = tabs.filter((t) => t.id !== drop.id);
  }
  const id = newId(tabs);
  return { ...p, open: true, tabs: [...tabs, { id, t: target }], active: id };
}

/** Closes tab `id`: the one beside it shows instead; a split with it ends. */
export function closeTab(p: PaneState, id: string): PaneState {
  const i = p.tabs.findIndex((t) => t.id === id);
  if (i < 0) return p;
  const tabs = p.tabs.filter((t) => t.id !== id);
  let active = p.active;
  if (active === id) active = p.split?.second ?? tabs[Math.min(i, tabs.length - 1)]?.id ?? null;
  const split = p.split && p.split.second !== id && p.active !== id ? p.split : null;
  return { ...p, tabs, active, split };
}

/** Shows tab `id`. In a split, it takes the first place; clicking the second swaps them. */
export function focusTab(p: PaneState, id: string): PaneState {
  if (!p.tabs.some((t) => t.id === id) || p.active === id) return p;
  const split = p.split && p.split.second === id && p.active ? { ...p.split, second: p.active } : p.split;
  return { ...p, active: id, split };
}

/** Splits the active tab with another (the next, else the previous); split again ends it. */
export function toggleSplit(p: PaneState, dir: PaneSplit["dir"] = "row"): PaneState {
  if (p.split) return { ...p, split: null };
  const i = p.tabs.findIndex((t) => t.id === p.active);
  const other = p.tabs[i + 1] ?? p.tabs[i - 1];
  if (i < 0 || !other) return p;
  return { ...p, split: { dir, second: other.id, ratio: 0.5 } };
}
