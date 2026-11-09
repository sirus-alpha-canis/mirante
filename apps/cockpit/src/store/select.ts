// Derived views of the store. Pure functions, unit-tested.
import type { TaskView } from "../lib/protocol";
import type { FeedItem, Perm, State } from "./state";
import { type Key, t } from "../lib/i18n";

export type Column = "needs_you" | "working" | "review" | "idle" | "archived";

const COLUMN_KEY: Record<Column, Key> = {
  needs_you: "column.needs_you",
  working: "column.working",
  review: "column.review",
  idle: "column.idle",
  archived: "column.archived",
};

/** A column's name in the current language. */
export function columnLabel(c: Column): string {
  return t(COLUMN_KEY[c]);
}

/** The three board columns, in order. */
export const BOARD: Column[] = ["needs_you", "working", "review"];

export function needsYou(t: TaskView): boolean {
  return t.lifecycle !== "archived" && (t.status === "needs_you" || t.needs_you > 0 || t.boot === "trust_prompt");
}

export function turnsEnded(feed: FeedItem[] | undefined): number {
  return (feed ?? []).filter((i) => i.kind === "turn" && i.phase === "ended").length;
}

export function columnOf(t: TaskView, feed: FeedItem[] | undefined): Column {
  if (t.lifecycle === "archived" || t.lifecycle === "archiving") return "archived";
  if (needsYou(t)) return "needs_you";
  if (t.status === "working" || t.boot === "booting") return "working";
  if (turnsEnded(feed) > 0) return "review";
  return "idle";
}

export function tasksIn(s: State, col: Column, repo?: string): TaskView[] {
  return Object.values(s.tasks)
    .filter((t) => (repo ? t.repo_path === repo : true) && columnOf(t, s.feed[t.id]) === col)
    .sort((a, b) => b.updated_at - a.updated_at);
}

const NO_PERMS: Perm[] = [];
// Pending requests by task, built once per permissions object (every card and the detail
// ask for theirs on each change).
const pendingIndex = new WeakMap<State["perms"], Map<string, Perm[]>>();

/** `taskId`'s pending requests, oldest first (the same array while `perms` is the same). */
export function pendingPerms(perms: State["perms"], taskId: string): Perm[] {
  let idx = pendingIndex.get(perms);
  if (!idx) {
    idx = new Map();
    for (const p of Object.values(perms)) {
      if (p.status !== "pending") continue;
      const list = idx.get(p.task_id);
      if (list) list.push(p);
      else idx.set(p.task_id, [p]);
    }
    for (const list of idx.values()) list.sort((a, b) => a.created_at - b.created_at);
    pendingIndex.set(perms, idx);
  }
  return idx.get(taskId) ?? NO_PERMS;
}

export function repoName(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || path;
}

export interface Project {
  path: string;
  name: string;
  /** Its tasks not archived, and how many of them have a live session. */
  tasks: number;
  live: number;
  needsYou: number;
  /** Hidden from Projects and Home (its tasks kept). */
  hidden: boolean;
  /** The folder it was picked from (its group in the rail). */
  group: string | null;
}

/** Repos from the registry plus every repo a task runs in, by name. */
export function projects(s: Pick<State, "tasks">, registry: string[], opts: { hidden?: string[]; groups?: Record<string, string> } = {}): Project[] {
  const paths = new Set(registry);
  for (const t of Object.values(s.tasks)) if (t.lifecycle !== "archived") paths.add(t.repo_path);
  const hidden = new Set(opts.hidden ?? []);
  return [...paths]
    .map((path) => {
      const ts = Object.values(s.tasks).filter((t) => t.repo_path === path && t.lifecycle !== "archived");
      return {
        path,
        name: repoName(path),
        tasks: ts.length,
        live: ts.filter((t) => t.live).length,
        needsYou: ts.filter(needsYou).length,
        hidden: hidden.has(path),
        group: opts.groups?.[path] ?? null,
      };
    })
    .sort((a, b) => a.name.localeCompare(b.name) || a.path.localeCompare(b.path));
}

/** The last `n` feed lines worth showing on a small tile. */
export function lastLines(feed: FeedItem[] | undefined, n: number): FeedItem[] {
  return (feed ?? []).filter((i) => i.kind !== "turn").slice(-n);
}

/** Tasks that just started needing the user (for notifications). Only the transition
 * counts: a task that already needs the user and gets a second request (another permission
 * while one is open) is not news again, so it does not notify twice; the open card and the
 * needs-you badge already say so. */
export function newlyNeedingYou(prev: State, next: State): TaskView[] {
  return Object.values(next.tasks).filter((t) => {
    const before = prev.tasks[t.id];
    return needsYou(t) && !(before && needsYou(before));
  });
}

/** The grid's tiles: needs-you first, then working, then the most recently active; 2..6. */
export function gridTasks(s: State, max = 6): TaskView[] {
  const rank: Record<Column, number> = { needs_you: 0, working: 1, review: 2, idle: 3, archived: 9 };
  return Object.values(s.tasks)
    .filter((t) => t.lifecycle !== "archived" && (t.live || columnOf(t, s.feed[t.id]) !== "idle"))
    .sort((a, b) => rank[columnOf(a, s.feed[a.id])] - rank[columnOf(b, s.feed[b.id])] || b.updated_at - a.updated_at)
    .slice(0, max);
}
