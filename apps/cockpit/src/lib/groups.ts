// Fan-out groups as the cockpit shows them (founder decision 2): a group is the person's
// shared goal with its children (ordinary tasks, each with its own worktree, agent and
// permissions). It has no worktree or session of its own: Home folds its children under one
// row with a rollup, and every member's Detail shows the group's panel.
import { t, tn } from "./i18n";
import type { TaskGroup, TaskView } from "./protocol";
import type { Column } from "../store/select";

/** The most children one group holds (the daemon's `MAX_CHILDREN`). */
export const MAX_CHILDREN = 8;

/** How many of a group's tasks are in each column (archived ones are left out). */
export interface Rollup {
  total: number;
  needs_you: number;
  working: number;
  review: number;
  idle: number;
}

export function rollupOf(ids: readonly string[], columns: Record<string, Column>): Rollup {
  const r: Rollup = { total: 0, needs_you: 0, working: 0, review: 0, idle: 0 };
  for (const id of ids) {
    const c = columns[id];
    if (!c || c === "archived") continue;
    r.total++;
    r[c]++;
  }
  return r;
}

/** "3 children: 1 needs you, 2 working" (the columns that have any, most urgent first). */
export function rollupText(r: Rollup): string {
  const parts: string[] = [];
  if (r.needs_you) parts.push(tn(r.needs_you, "group.needsOne", "group.needsMany"));
  if (r.working) parts.push(t("group.working", { n: r.working }));
  if (r.review) parts.push(t("group.review", { n: r.review }));
  if (r.idle) parts.push(t("group.idle", { n: r.idle }));
  const children = tn(r.total, "group.childrenOne", "group.childrenMany");
  return parts.length ? t("group.rollup", { children, parts: parts.join(", ") }) : children;
}

/** A group's tasks that are not archived, in creation order (children, then any synthesizer). */
export function membersOf(tasks: Record<string, TaskView>, groupId: string): TaskView[] {
  return Object.values(tasks)
    .filter((x) => x.group_id === groupId && x.lifecycle !== "archived")
    .sort((a, b) => a.created_at - b.created_at || a.id.localeCompare(b.id));
}

/** The key a group's row takes among task ids in Home's sections. */
export const groupKey = (groupId: string) => `group:${groupId}`;
export const groupOfKey = (key: string): string | null => (key.startsWith("group:") ? key.slice(6) : null);

/** "compare" / "fan-out", in the current language. */
export const kindLabel = (g: Pick<TaskGroup, "kind">) => t(g.kind === "compare" ? "group.compare" : "group.fanout");

/** Where a group's row remembers being unfolded (per viewer, this machine). */
export const openKey = (groupId: string) => `mirante.fanout.${groupId}`;

type KV = Pick<Storage, "getItem" | "setItem">;

/** Folded unless the person unfolded it (storage that refuses: folded). */
export function loadOpen(store: KV, groupId: string): boolean {
  try {
    return store.getItem(openKey(groupId)) === "1";
  } catch {
    return false;
  }
}

export function saveOpen(store: KV, groupId: string, open: boolean): void {
  try {
    store.setItem(openKey(groupId), open ? "1" : "0");
  } catch {
    // Not remembered: it starts folded next time.
  }
}
