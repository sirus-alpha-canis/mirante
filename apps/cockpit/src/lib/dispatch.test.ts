import { describe, expect, it } from "vitest";
import {
  AGENT_OPTIONS,
  CONTEXT_HEADER,
  contextKey,
  withContext,
  applySuggestion,
  branchFromGoal,
  branchProblem,
  isPowerSyntax,
  matchProjects,
  nextFreeBranch,
  parseDispatch,
  resolveRepo,
  suggest,
} from "./dispatch";

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

describe("parseDispatch with no branch", () => {
  it("'repo/' with nothing after the slash leaves the branch to be named from the goal", () => {
    expect(parseDispatch("@claude shop/ fix the login")).toEqual({
      ok: true,
      value: { agent: "claude", repo: "shop", branch: "", goal: "fix the login" },
    });
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

const projs = [
  { name: "shop", path: "/a/shop" },
  { name: "api", path: "/a/api" },
  { name: "workshop", path: "/b/workshop" },
  { name: "mirante", path: "/c/mirante" },
];

describe("matchProjects", () => {
  it("prefix matches first, then substring, ignoring case; empty lists all", () => {
    expect(matchProjects("sho", projs).map((p) => p.name)).toEqual(["shop", "workshop"]);
    expect(matchProjects("HOP", projs).map((p) => p.name)).toEqual(["shop", "workshop"]);
    expect(matchProjects("", projs).map((p) => p.name)).toEqual(["shop", "api", "workshop", "mirante"]);
    expect(matchProjects("zzz", projs)).toEqual([]);
  });
});

describe("suggest", () => {
  const at = (text: string, caret = text.length) => suggest(text, caret, projs);

  it("an @ word suggests the agents that match it", () => {
    expect(at("@")?.items.map((i) => i.label)).toEqual(["@claude", "@codex"]);
    expect(at("@co")?.items.map((i) => i.label)).toEqual(["@codex"]);
    expect(at("@x")).toBeNull();
  });

  it("after the agent and a space, the projects filtered by what is typed", () => {
    expect(at("@claude ")?.items.map((i) => i.label)).toEqual(["shop", "api", "workshop", "mirante"]);
    expect(at("@codex mir")?.items.map((i) => i.label)).toEqual(["mirante"]);
  });

  it("nothing inside the goal, past the branch, or in plain text", () => {
    expect(at("@claude shop/feat/x ")).toBeNull();
    expect(at("@claude shop/feat/x fix @")).toBeNull();
    expect(at("@claude shop/fe")).toBeNull();
    expect(at("fix the @co")).toBeNull();
    expect(at("@gpt sh")).toBeNull();
  });

  it("applying replaces the word: an agent gets a space, a project its slash", () => {
    const a = at("@cl")!;
    expect(applySuggestion("@cl", a, a.items[0])).toEqual({ text: "@claude ", caret: 8 });
    const r = at("@claude sh")!;
    expect(applySuggestion("@claude sh", r, r.items[0])).toEqual({ text: "@claude shop/", caret: 13 });
  });

  it("a project picked mid-text keeps the branch already typed after it", () => {
    const text = "@claude sh/feat/x go";
    const r = suggest(text, 10, projs)!;
    expect(applySuggestion(text, r, r.items[0])).toEqual({ text: "@claude shop/feat/x go", caret: 13 });
  });
});

describe("withContext", () => {
  it("appends the attached references after the goal, labeled, one per line", () => {
    const goal = "Fix the login\nkeep the API";
    const out = withContext(goal, [
      { kind: "file", path: "src/login.ts" },
      { kind: "note", slug: "tokens", title: "Tokens are refreshed by the session layer" },
      { kind: "skill", name: "cargo-tests-first", description: "run the tests first" },
    ]);
    expect(out).toBe(
      [
        "Fix the login",
        "keep the API",
        "",
        CONTEXT_HEADER,
        "- file: src/login.ts",
        '- repo memory note "Tokens are refreshed by the session layer": .mirante/memory/tokens.md',
        "- skill: cargo-tests-first (run the tests first)",
      ].join("\n"),
    );
    expect(out.split("\n")[0]).toBe("Fix the login");
    expect(withContext("just this", [])).toBe("just this");
    expect(contextKey({ kind: "skill", name: "x", description: "" })).toBe("skill:x");
  });

  it("offers Claude and Codex, in that order", () => {
    expect(AGENT_OPTIONS.map((a) => a.id)).toEqual(["claude", "codex"]);
  });
});
