import { describe, expect, it } from "vitest";
import { addRepo, loadRepos, removeRepo, REPOS_KEY } from "./repos";

function mem() {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
}

describe("repo registry", () => {
  it("adds absolute paths once, without trailing slashes", () => {
    const s = mem();
    expect(addRepo(s, "/Users/me/shop/")).toEqual({ ok: true, repos: ["/Users/me/shop"] });
    addRepo(s, "/Users/me/shop");
    expect(loadRepos(s)).toEqual(["/Users/me/shop"]);
    expect(removeRepo(s, "/Users/me/shop")).toEqual([]);
  });

  it("refuses relative paths and survives garbage in storage", () => {
    const s = mem();
    expect(addRepo(s, "shop")).toMatchObject({ ok: false });
    s.setItem(REPOS_KEY, "{not json");
    expect(loadRepos(s)).toEqual([]);
  });
});
