import { describe, expect, it } from "vitest";
import { branchFromGoal, branchProblem, isPowerSyntax, nextFreeBranch, parseDispatch, resolveRepo } from "./dispatch";

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

describe("branchFromGoal", () => {
  it("slugs Portuguese text: lowercase ASCII, accents stripped, under mirante/", () => {
    expect(branchFromGoal("Leia este repositório e me explique")).toBe("mirante/leia-este-repositorio-e-me-explique");
    expect(branchFromGoal("  Ação: corrigir o 'login' (urgente!)  ")).toBe("mirante/acao-corrigir-o-login-urgente");
  });

  it("caps the slug near 40 characters at a word boundary", () => {
    const b = branchFromGoal("refactor the payment gateway adapter so retries use exponential backoff");
    expect(b).toBe("mirante/refactor-the-payment-gateway-adapter-so");
    expect(b.length - "mirante/".length).toBeLessThanOrEqual(40);
    // One very long word is cut, not dropped.
    expect(branchFromGoal("a".repeat(60))).toBe(`mirante/${"a".repeat(40)}`);
  });

  it("falls back to mirante/task when nothing ASCII is left", () => {
    expect(branchFromGoal("日本語のテキスト")).toBe("mirante/task");
    expect(branchFromGoal("   ")).toBe("mirante/task");
  });

  it("always yields a branch the dispatch rules accept", () => {
    for (const g of ["..lock", "-rf /", "x.lock", "@{u}", "a//b", "ÀÉÎõü"]) expect(branchProblem(branchFromGoal(g)), g).toBeNull();
  });
});

describe("nextFreeBranch", () => {
  it("appends -2, -3… past the taken names", () => {
    expect(nextFreeBranch("mirante/x", new Set())).toBe("mirante/x");
    expect(nextFreeBranch("mirante/x", new Set(["mirante/x"]))).toBe("mirante/x-2");
    expect(nextFreeBranch("mirante/x", new Set(["mirante/x", "mirante/x-2"]))).toBe("mirante/x-3");
  });
});

describe("isPowerSyntax", () => {
  it("is the @claude / @codex prefix only", () => {
    expect(isPowerSyntax("@claude shop/x go")).toBe(true);
    expect(isPowerSyntax("  @Codex shop/x go")).toBe(true);
    expect(isPowerSyntax("email @claude later")).toBe(false);
    expect(isPowerSyntax("@claudeX hi")).toBe(false);
  });
});
