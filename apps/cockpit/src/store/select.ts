// Derived views of the store. Pure functions, unit-tested.
import type { TaskView } from "../lib/protocol";
import type { FeedItem, Perm, State } from "./state";

export type Column = "needs_you" | "working" | "review" | "idle" | "archived";

export const COLUMN_LABEL: Record<Column, string> = {
  needs_you: "Needs you",
  working: "Working",
  review: "Ready for review",
  idle: "Idle",
  archived: "Archived",
};

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

export function pendingPerms(s: State, taskId: string): Perm[] {
  return Object.values(s.perms)
    .filter((p) => p.task_id === taskId && p.status === "pending")
    .sort((a, b) => a.created_at - b.created_at);
}

export function repoName(path: string): string {
  const parts = path.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || path;
}

export interface Project {
  path: string;
  name: string;
  tasks: number;
  needsYou: number;
}

/** Repos from the registry plus every repo a task runs in, by name. */
export function projects(s: State, registry: string[]): Project[] {
  const paths = new Set(registry);
  for (const t of Object.values(s.tasks)) if (t.lifecycle !== "archived") paths.add(t.repo_path);
  return [...paths]
    .map((path) => {
      const ts = Object.values(s.tasks).filter((t) => t.repo_path === path && t.lifecycle !== "archived");
      return { path, name: repoName(path), tasks: ts.length, needsYou: ts.filter(needsYou).length };
    })
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** The last `n` feed lines worth showing on a small tile. */
export function lastLines(feed: FeedItem[] | undefined, n: number): FeedItem[] {
  return (feed ?? []).filter((i) => i.kind !== "turn").slice(-n);
}

/** Tasks that just started needing the user (for notifications). */
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
