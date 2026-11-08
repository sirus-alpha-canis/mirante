// Repos the user added by path (tasks add theirs implicitly). Stored in the webview's
// localStorage: per machine, never sent anywhere.
export const REPOS_KEY = "mirante.repos";

type KV = Pick<Storage, "getItem" | "setItem">;

export function loadRepos(store: KV): string[] {
  try {
    const v = JSON.parse(store.getItem(REPOS_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((p): p is string => typeof p === "string") : [];
  } catch {
    return [];
  }
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
  if (text.startsWith("~")) return { ok: false, error: "'~user' is not expanded here: type the full path, e.g. /Users/me/code/shop" };
  if (!text.startsWith("/")) return { ok: false, error: "use the repo's absolute path, e.g. /Users/me/code/shop" };
  const parts = text.split("/").filter((p) => p !== "" && p !== ".");
  if (parts.includes("..")) return { ok: false, error: "use a path without '..' segments" };
  return { ok: true, path: `/${parts.join("/")}` };
}

export function addRepo(store: KV, raw: string): Added {
  const n = normaliseRepoPath(raw);
  if (!n.ok) return n;
  if (n.path === "/") return { ok: false, error: "use the repo's absolute path, e.g. /Users/me/code/shop" };
  const repos = loadRepos(store);
  if (!repos.includes(n.path)) repos.push(n.path);
  store.setItem(REPOS_KEY, JSON.stringify(repos));
  return { ok: true, repos };
}

export function removeRepo(store: KV, path: string): string[] {
  const repos = loadRepos(store).filter((p) => p !== path);
  store.setItem(REPOS_KEY, JSON.stringify(repos));
  return repos;
}
