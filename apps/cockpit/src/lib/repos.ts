// Repos the user added by path (tasks add theirs implicitly). Stored in the webview's
// localStorage: per machine, never sent anywhere.
import { t } from "./i18n";

export const REPOS_KEY = "mirante.repos";
/** Projects hidden from Projects and Home (their tasks kept). */
export const HIDDEN_KEY = "mirante.repos.hidden";
/** Project path → the folder it was picked from (a group in the rail). */
export const GROUPS_KEY = "mirante.repos.groups";
/** Groups shown folded in the rail. */
export const COLLAPSED_KEY = "mirante.repos.collapsed";

type KV = Pick<Storage, "getItem" | "setItem">;

function loadList(store: KV, key: string): string[] {
  try {
    const v = JSON.parse(store.getItem(key) ?? "[]");
    return Array.isArray(v) ? [...new Set(v.filter((p): p is string => typeof p === "string"))] : [];
  } catch {
    return [];
  }
}

export function loadRepos(store: KV): string[] {
  return loadList(store, REPOS_KEY);
}

/** Everything the rail remembers about projects. */
export interface Registry {
  repos: string[];
  hidden: string[];
  groups: Record<string, string>;
  collapsed: string[];
}

function loadGroups(store: KV): Record<string, string> {
  try {
    const v: unknown = JSON.parse(store.getItem(GROUPS_KEY) ?? "{}");
    if (!v || typeof v !== "object" || Array.isArray(v)) return {};
    return Object.fromEntries(Object.entries(v).filter((e): e is [string, string] => typeof e[1] === "string"));
  } catch {
    return {};
  }
}

export function loadRegistry(store: KV): Registry {
  return { repos: loadRepos(store), hidden: loadList(store, HIDDEN_KEY), groups: loadGroups(store), collapsed: loadList(store, COLLAPSED_KEY) };
}

/** Writes what changed and answers the whole registry (as it holds for this run when the
 * storage refuses). */
function saveRegistry(store: KV, r: Registry): Registry {
  put(store, REPOS_KEY, r.repos);
  put(store, HIDDEN_KEY, r.hidden);
  put(store, GROUPS_KEY, r.groups);
  put(store, COLLAPSED_KEY, r.collapsed);
  return r;
}

/** Adds the repositories picked in `folder` (normalised; bad ones skipped) as `folder`'s
 * group; `folder`'s own entry (a folder added before, not a repository) goes. */
export function addRepos(store: KV, raw: string[], folder: string): Registry {
  const r = loadRegistry(store);
  const repos = r.repos.filter((p) => p !== folder);
  const groups = { ...r.groups };
  for (const one of raw) {
    const n = normaliseRepoPath(one);
    if (!n.ok || n.path === "/") continue;
    if (!repos.includes(n.path)) repos.push(n.path);
    groups[n.path] = folder;
  }
  delete groups[folder];
  return saveRegistry(store, { ...r, repos, groups, hidden: r.hidden.filter((p) => p !== folder) });
}

/** Removes `paths` from Projects: the list, the hidden ones and their groups. */
export function forgetRepos(store: KV, paths: string[]): Registry {
  const gone = new Set(paths);
  const r = loadRegistry(store);
  const groups = Object.fromEntries(Object.entries(r.groups).filter(([p]) => !gone.has(p)));
  const folders = new Set(Object.values(groups));
  return saveRegistry(store, {
    repos: r.repos.filter((p) => !gone.has(p)),
    hidden: r.hidden.filter((p) => !gone.has(p)),
    groups,
    collapsed: r.collapsed.filter((f) => folders.has(f)),
  });
}

export function hideRepos(store: KV, paths: string[]): Registry {
  const r = loadRegistry(store);
  return saveRegistry(store, { ...r, hidden: [...new Set([...r.hidden, ...paths])] });
}

export function showRepos(store: KV, paths: string[]): Registry {
  const r = loadRegistry(store);
  return saveRegistry(store, { ...r, hidden: r.hidden.filter((p) => !paths.includes(p)) });
}

export function setCollapsed(store: KV, folder: string, collapsed: boolean): Registry {
  const r = loadRegistry(store);
  const rest = r.collapsed.filter((f) => f !== folder);
  return saveRegistry(store, { ...r, collapsed: collapsed ? [...rest, folder] : rest });
}

/** "~" and "~/…" against `home` (the app's, from the Tauri side); anything else unchanged
 * ("~bob/…" too: `normaliseRepoPath` then explains it). */
export function expandHome(raw: string, home: string): string {
  const text = raw.trim();
  if (text !== "~" && !text.startsWith("~/")) return text;
  return home.replace(/\/+$/, "") + text.slice(1);
}

export type Added = { ok: true; repos: string[] } | { ok: false; error: string };

/** `raw` as one absolute path: no doubled slashes or "." segments; ".." and "~" refused. */
export function normaliseRepoPath(raw: string): { ok: true; path: string } | { ok: false; error: string } {
  const text = raw.trim();
  if (text.startsWith("~")) return { ok: false, error: t("repos.tilde") };
  if (!text.startsWith("/")) return { ok: false, error: t("repos.absolute") };
  const parts = text.split("/").filter((p) => p !== "" && p !== ".");
  if (parts.includes("..")) return { ok: false, error: t("repos.dotdot") };
  return { ok: true, path: `/${parts.join("/")}` };
}

export function addRepo(store: KV, raw: string): Added {
  const n = normaliseRepoPath(raw);
  if (!n.ok) return n;
  if (n.path === "/") return { ok: false, error: t("repos.absolute") };
  const repos = loadRepos(store);
  if (!repos.includes(n.path)) repos.push(n.path);
  save(store, repos);
  return { ok: true, repos };
}

export function removeRepo(store: KV, path: string): string[] {
  const repos = loadRepos(store).filter((p) => p !== path);
  save(store, repos);
  return repos;
}

function save(store: KV, repos: string[]): void {
  put(store, REPOS_KEY, repos);
}

/** Storage that refuses (a private window, blocked site data): the value holds for this run. */
function put(store: KV, key: string, value: unknown): void {
  try {
    store.setItem(key, JSON.stringify(value));
  } catch {
    // Not remembered past this run.
  }
}
