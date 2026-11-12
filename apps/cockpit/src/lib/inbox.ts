// Home as an inbox across every project: what needs the person first (oldest wait first),
// then what is ready for review, then what is working, each grouped by project with a
// stable color per project. Plus the focus-mode order behind "Next ⌘J".
import { t } from "./i18n";
import type { TaskView } from "./protocol";
import { columnOf, type Column, pendingPerms, repoName } from "../store/select";
import type { State } from "../store/state";

/** Colors a project chip can take (`proj-0` … `proj-7` in the stylesheet). */
export const PROJECT_COLORS = 8;

/** A project's color, the same on every run and machine: a hash of its path. */
export function projectColor(path: string): number {
  let h = 2166136261;
  for (let i = 0; i < path.length; i++) {
    h ^= path.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0) % PROJECT_COLORS;
}

/** Since when `t` waits on the person (ms): its oldest pending request, else its last change. */
export function waitingSince(t: TaskView, perms: State["perms"]): number {
  const p = pendingPerms(perms, t.id);
  return p.length > 0 ? Math.min(...p.map((x) => x.created_at)) : t.updated_at;
}

export type Section = "needs_you" | "review" | "working" | "idle";

export const SECTIONS: Section[] = ["needs_you", "review", "working", "idle"];

export interface ProjectGroup {
  path: string;
  name: string;
  color: number;
  ids: string[];
}

/** Everything Home and "Próximo" read, worked out in one pass. */
export interface Inbox {
  /** Each section's task ids grouped by project (a fan-out group's other members fold into
   * one `group:<id>` entry). */
  sections: Record<Section, ProjectGroup[]>;
  /** The ids needing the person, longest wait first (the focus-mode order). */
  needing: string[];
  /** Since when each task needing the person waits (ms). */
  waits: Map<string, number>;
  /** Every task's column. */
  columns: Record<string, Column>;
}

type Slices = Pick<State, "tasks" | "feed" | "perms">;

// One answer per (tasks, perms, feed): App, Home, the nudge and "Próximo" all read it, and
// an event recomputes it once (not once per reader).
const memo = new WeakMap<object, { perms: object; feed: object; value: Inbox }>();

/** The inbox of `s` (see [`Inbox`]), memoized on its slices. */
export function inboxOf(s: Slices): Inbox {
  const hit = memo.get(s.tasks);
  if (hit && hit.perms === s.perms && hit.feed === s.feed) return hit.value;
  const value = computeInbox(s);
  memo.set(s.tasks, { perms: s.perms, feed: s.feed, value });
  return value;
}

function computeInbox(s: Slices): Inbox {
  // Oldest pending request per task, in one pass over the requests.
  const oldest = new Map<string, number>();
  for (const p of Object.values(s.perms)) {
    if (p.status !== "pending") continue;
    const cur = oldest.get(p.task_id);
    if (cur === undefined || p.created_at < cur) oldest.set(p.task_id, p.created_at);
  }
  const by: Record<Section, TaskView[]> = { needs_you: [], review: [], working: [], idle: [] };
  const columns: Record<string, Column> = {};
  const waits = new Map<string, number>();
  for (const t of Object.values(s.tasks)) {
    const col: Column = columnOf(t, s.feed[t.id]);
    columns[t.id] = col;
    if (col === "archived") continue;
    by[col].push(t);
    if (col === "needs_you") waits.set(t.id, oldest.get(t.id) ?? t.updated_at);
  }
  const wait = (t: TaskView) => waits.get(t.id) ?? t.updated_at;
  // Siblings of a fan-out group stay adjacent: the group goes where its oldest wait puts it.
  const groupWait = new Map<string, number>();
  for (const t of by.needs_you) if (t.group_id) groupWait.set(t.group_id, Math.min(groupWait.get(t.group_id) ?? Infinity, wait(t)));
  const outer = (t: TaskView) => (t.group_id ? (groupWait.get(t.group_id) ?? wait(t)) : wait(t));
  by.needs_you.sort((a, b) => outer(a) - outer(b) || (a.group_id ?? a.id).localeCompare(b.group_id ?? b.id) || wait(a) - wait(b) || a.id.localeCompare(b.id));
  for (const k of ["review", "working", "idle"] as const) by[k].sort((a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id));
  // A group's members that do not need the person fold into one row (`group:<id>`), in the
  // most urgent of their sections, where its newest member sorts.
  const rank: Record<Section, number> = { needs_you: 0, review: 1, working: 2, idle: 3 };
  const home = new Map<string, Section>();
  for (const k of ["review", "working", "idle"] as const) {
    for (const t of by[k]) if (t.group_id && !home.has(t.group_id)) home.set(t.group_id, k);
  }
  for (const [gid, k] of home) for (const k2 of ["review", "working", "idle"] as const) if (rank[k2] < rank[k] && by[k2].some((t) => t.group_id === gid)) home.set(gid, k2);
  const group = (ts: TaskView[], section: Section): ProjectGroup[] => {
    const m = new Map<string, ProjectGroup>();
    const placed = new Set<string>();
    for (const t of ts) {
      let id = t.id;
      if (t.group_id && section !== "needs_you") {
        if (home.get(t.group_id) !== section || placed.has(t.group_id)) continue;
        placed.add(t.group_id);
        id = `group:${t.group_id}`;
      }
      const g = m.get(t.repo_path) ?? { path: t.repo_path, name: repoName(t.repo_path), color: projectColor(t.repo_path), ids: [] };
      g.ids.push(id);
      m.set(t.repo_path, g);
    }
    return [...m.values()].sort((a, b) => a.name.localeCompare(b.name) || a.path.localeCompare(b.path));
  };
  return {
    sections: {
      needs_you: group(by.needs_you, "needs_you"),
      review: group(by.review, "review"),
      working: group(by.working, "working"),
      idle: group(by.idle, "idle"),
    },
    needing: by.needs_you.map((t) => t.id),
    waits,
    columns,
  };
}

/** Each section's task ids grouped by project (projects by name, tasks in the section's
 * order: needs-you by longest wait, the others by most recent change). Archived tasks are
 * left out. Ids only, so a selector can compare it shallowly. */
export function inboxSections(s: Slices): Record<Section, ProjectGroup[]> {
  return inboxOf(s).sections;
}

/** Same groups (ids in the same order): what a selector compares to skip a render. */
export function sameSections(a: Record<Section, ProjectGroup[]>, b: Record<Section, ProjectGroup[]>): boolean {
  return SECTIONS.every(
    (k) =>
      a[k].length === b[k].length &&
      a[k].every((g, i) => g.path === b[k][i].path && g.ids.length === b[k][i].ids.length && g.ids.every((id, j) => id === b[k][i].ids[j])),
  );
}

/** The ids needing the person, longest wait first (the focus-mode order). */
export function needingOrder(s: Slices): string[] {
  return inboxOf(s).needing;
}

/** Where "Próximo" goes from `current`: the item after it (wrapping), else the first; and
 * the 1-based position shown ("i de n"; 0 when `current` is not one of them). */
export function nextNeeding(order: string[], current: string | null): { id: string | null; at: number } {
  if (order.length === 0) return { id: null, at: 0 };
  const i = current ? order.indexOf(current) : -1;
  return { id: order[(i + 1) % order.length], at: i + 1 };
}

/** How many tasks have waited on the person longer than `ms` at `now`. */
export function waitingLongerThan(s: Slices, now: number, ms: number): number {
  let n = 0;
  for (const since of inboxOf(s).waits.values()) if (now - since > ms) n++;
  return n;
}

/** "3 min", "1 h 5 min", "2 d": a short elapsed time. */
export function elapsed(ms: number): string {
  const min = Math.max(0, Math.floor(ms / 60_000));
  if (min < 1) return t("inbox.now");
  if (min < 60) return t("inbox.min", { m: min });
  const h = Math.floor(min / 60);
  if (h < 24) return min % 60 ? t("inbox.hMin", { h, m: min % 60 }) : t("inbox.h", { h });
  return t("inbox.d", { d: Math.floor(h / 24) });
}
