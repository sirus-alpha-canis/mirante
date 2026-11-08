import { describe, expect, it } from "vitest";
import { addRepo, expandHome, loadRepos, removeRepo, REPOS_KEY } from "./repos";

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
