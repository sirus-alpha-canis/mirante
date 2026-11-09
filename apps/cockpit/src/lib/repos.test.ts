import { describe, expect, it } from "vitest";
import { addRepo, addRepos, expandHome, forgetRepos, GROUPS_KEY, HIDDEN_KEY, hideRepos, loadRegistry, loadRepos, removeRepo, REPOS_KEY, setCollapsed, showRepos } from "./repos";

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

  it("normalises doubled slashes and '.' segments", () => {
    const s = mem();
    expect(addRepo(s, "//Users//me/./shop/.")).toEqual({ ok: true, repos: ["/Users/me/shop"] });
  });

  it("refuses '..' segments and explains '~'", () => {
    const s = mem();
    expect(addRepo(s, "/Users/me/../you/shop")).toEqual({ ok: false, error: expect.stringContaining("'..'") });
    expect(addRepo(s, "~/code/shop")).toEqual({ ok: false, error: expect.stringContaining("~") });
    expect(loadRepos(s)).toEqual([]);
  });

  it("refuses relative paths and survives garbage in storage", () => {
    const s = mem();
    expect(addRepo(s, "shop")).toMatchObject({ ok: false });
    s.setItem(REPOS_KEY, "{not json");
    expect(loadRepos(s)).toEqual([]);
  });
});

describe("expandHome", () => {
  it("expands ~ and ~/… with the home the app was given", () => {
    expect(expandHome("~/code/shop", "/Users/me")).toBe("/Users/me/code/shop");
    expect(expandHome("  ~/code/shop ", "/Users/me/")).toBe("/Users/me/code/shop");
    expect(expandHome("~", "/Users/me")).toBe("/Users/me");
  });

  it("leaves other paths alone, and ~user unexpanded (addRepo then explains it)", () => {
    expect(expandHome("/a/b", "/Users/me")).toBe("/a/b");
    expect(expandHome("~bob/x", "/Users/me")).toBe("~bob/x");
  });
});

describe("the registry's hidden projects, groups and collapsed groups", () => {
  it("starts empty and survives garbage and a storage that throws", () => {
    const s = mem();
    expect(loadRegistry(s)).toEqual({ repos: [], hidden: [], groups: {}, collapsed: [] });
    s.setItem(HIDDEN_KEY, "[1, \"/a\"]");
    s.setItem(GROUPS_KEY, "[\"no\"]");
    expect(loadRegistry(s)).toEqual({ repos: [], hidden: ["/a"], groups: {}, collapsed: [] });
    const broken = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); } };
    expect(loadRegistry(broken)).toEqual({ repos: [], hidden: [], groups: {}, collapsed: [] });
    expect(addRepos(broken, ["/g/a"], "/g").repos).toEqual(["/g/a"]);
  });

  it("adds a folder's picked repositories as one group, replacing the folder's own entry", () => {
    const s = mem();
    addRepo(s, "/Users/me/gol");
    const r = addRepos(s, ["/Users/me/gol/api/", "/Users/me/gol/web"], "/Users/me/gol");
    expect(r.repos).toEqual(["/Users/me/gol/api", "/Users/me/gol/web"]);
    expect(r.groups).toEqual({ "/Users/me/gol/api": "/Users/me/gol", "/Users/me/gol/web": "/Users/me/gol" });
    expect(loadRegistry(s)).toEqual(r);
  });

  it("forgets projects everywhere: the list, hidden and groups", () => {
    const s = mem();
    addRepos(s, ["/g/a", "/g/b"], "/g");
    hideRepos(s, ["/g/a"]);
    const r = forgetRepos(s, ["/g/a"]);
    expect(r).toEqual({ repos: ["/g/b"], hidden: [], groups: { "/g/b": "/g" }, collapsed: [] });
  });

  it("hides and shows projects, and remembers collapsed groups", () => {
    const s = mem();
    expect(hideRepos(s, ["/x", "/y", "/x"]).hidden).toEqual(["/x", "/y"]);
    expect(showRepos(s, ["/x"]).hidden).toEqual(["/y"]);
    expect(setCollapsed(s, "/g", true).collapsed).toEqual(["/g"]);
    expect(setCollapsed(s, "/g", false).collapsed).toEqual([]);
  });
});
