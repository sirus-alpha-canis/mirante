// Home as an inbox across every project: what needs the person first (oldest wait first),
// then what is ready for review, then what is working, each grouped by project with a
// stable color per project. Plus the focus-mode order behind "Próximo ⌘J".
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

/** Each section's task ids grouped by project (projects by name, tasks in the section's
 * order: needs-you by longest wait, the others by most recent change). Archived tasks are
 * left out. Ids only, so a selector can compare it shallowly. */
export function inboxSections(s: Pick<State, "tasks" | "feed" | "perms">): Record<Section, ProjectGroup[]> {
  const by: Record<Section, TaskView[]> = { needs_you: [], review: [], working: [], idle: [] };
  for (const t of Object.values(s.tasks)) {
    const col: Column = columnOf(t, s.feed[t.id]);
    if (col !== "archived") by[col].push(t);
  }
  const wait = (t: TaskView) => waitingSince(t, s.perms);
  by.needs_you.sort((a, b) => wait(a) - wait(b) || a.id.localeCompare(b.id));
  for (const k of ["review", "working", "idle"] as const) by[k].sort((a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id));
  const group = (ts: TaskView[]): ProjectGroup[] => {
    const m = new Map<string, ProjectGroup>();
    for (const t of ts) {
      const g = m.get(t.repo_path) ?? { path: t.repo_path, name: repoName(t.repo_path), color: projectColor(t.repo_path), ids: [] };
      g.ids.push(t.id);
      m.set(t.repo_path, g);
    }
    return [...m.values()].sort((a, b) => a.name.localeCompare(b.name) || a.path.localeCompare(b.path));
  };
  return { needs_you: group(by.needs_you), review: group(by.review), working: group(by.working), idle: group(by.idle) };
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
export function needingOrder(s: Pick<State, "tasks" | "feed" | "perms">): string[] {
  return inboxSections(s).needs_you.flatMap((g) => g.ids).sort((a, b) => {
    const ta = s.tasks[a];
    const tb = s.tasks[b];
    return waitingSince(ta, s.perms) - waitingSince(tb, s.perms) || a.localeCompare(b);
  });
}

/** Where "Próximo" goes from `current`: the item after it (wrapping), else the first; and
 * the 1-based position shown ("i de n"; 0 when `current` is not one of them). */
export function nextNeeding(order: string[], current: string | null): { id: string | null; at: number } {
  if (order.length === 0) return { id: null, at: 0 };
  const i = current ? order.indexOf(current) : -1;
  return { id: order[(i + 1) % order.length], at: i + 1 };
}

/** How many tasks have waited on the person longer than `ms` at `now`. */
export function waitingLongerThan(s: Pick<State, "tasks" | "feed" | "perms">, now: number, ms: number): number {
  return needingOrder(s).filter((id) => now - waitingSince(s.tasks[id], s.perms) > ms).length;
}

/** "3 min", "1 h 5 min", "2 d": a short elapsed time. */
export function elapsed(ms: number): string {
  const min = Math.max(0, Math.floor(ms / 60_000));
  if (min < 1) return "agora";
  if (min < 60) return `${min} min`;
  const h = Math.floor(min / 60);
  if (h < 24) return min % 60 ? `${h} h ${min % 60} min` : `${h} h`;
  return `${Math.floor(h / 24)} d`;
}
