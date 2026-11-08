import { describe, expect, it } from "vitest";
import { parseDispatch, resolveRepo } from "./dispatch";

describe("parseDispatch", () => {
  it("parses agent, repo, a branch with slashes and a multi-word goal", () => {
    expect(parseDispatch("@claude shop/feat/login add a login page")).toEqual({
      ok: true,
      value: { agent: "claude", repo: "shop", branch: "feat/login", goal: "add a login page" },
    });
    expect(parseDispatch("  @Codex   api/fix-42   fix the\nflaky test ")).toEqual({
      ok: true,
      value: { agent: "codex", repo: "api", branch: "fix-42", goal: "fix the\nflaky test" },
    });
  });

  it.each([
    ["fix it", "start with an agent"],
    ["@gpt shop/x do", 'unknown agent "@gpt"'],
    ["@claude", "say where"],
    ["@claude shop do it", '"shop" must be <repo>/<branch>'],
    ["@claude /main do it", "must be <repo>/<branch>"],
    ["@claude shop/ do it", "must be <repo>/<branch>"],
    ["@claude shop/feat/x", "say what the agent should do"],
    ["@claude shop/-rf do", "cannot start with '-'"],
    ["@claude shop/a..b do", "cannot contain '..'"],
    ["@claude shop/x.lock do", "cannot end with '.lock'"],
    ["@claude shop/a:b do", "git does not allow"],
    ["@claude shop//x do", "cannot start with '/'"],
    ["@claude shop/a//b do", "empty component"],
    ["@claude shop/.hidden do", "cannot start with '.'"],
    ["@claude shop/feat/.x do", "cannot start with '.'"],
    ["@claude shop/a.lock/b do", "cannot end with '.lock'"],
    ["@claude shop/@ do", "not a valid branch name"],
  ])("rejects %j", (input, msg) => {
    const r = parseDispatch(input);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain(msg);
  });
});

describe("resolveRepo", () => {
  const known = [
    { name: "shop", path: "/a/shop" },
    { name: "api", path: "/a/api" },
    { name: "api", path: "/b/api" },
  ];
  it("finds a unique name, refuses ambiguous and unknown ones", () => {
    expect(resolveRepo("shop", known)).toEqual({ ok: true, path: "/a/shop" });
    expect(resolveRepo("api", known)).toMatchObject({ ok: false });
    expect(resolveRepo("web", known)).toEqual({ ok: false, error: 'unknown repo "web": add it under Projects first' });
  });
});
