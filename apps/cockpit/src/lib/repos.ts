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

export type Added = { ok: true; repos: string[] } | { ok: false; error: string };

export function addRepo(store: KV, raw: string): Added {
  const path = raw.trim().replace(/\/+$/, "");
  if (!path.startsWith("/")) return { ok: false, error: "use the repo's absolute path, e.g. /Users/me/code/shop" };
  const repos = loadRepos(store);
  if (!repos.includes(path)) repos.push(path);
  store.setItem(REPOS_KEY, JSON.stringify(repos));
  return { ok: true, repos };
}

export function removeRepo(store: KV, path: string): string[] {
  const repos = loadRepos(store).filter((p) => p !== path);
  store.setItem(REPOS_KEY, JSON.stringify(repos));
  return repos;
}
