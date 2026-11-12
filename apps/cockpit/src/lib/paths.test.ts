import { describe, expect, it } from "vitest";
import { detectPaths, fileUrlTarget, toWorktreeRel } from "./paths";

const single = { worktree_path: "/Users/me/.mirante/worktrees/shop/feat-a" };
const ws = {
  worktree_path: "/m/_workspaces/x/Site-B2C",
  workspace_path: "/m/_workspaces/x",
  repos: [
    { name: "Site-B2C", worktree_path: "/m/_workspaces/x/Site-B2C" },
    { name: "bff", worktree_path: "/m/_workspaces/x/bff" },
  ],
};

describe("toWorktreeRel", () => {
  it("maps abs paths inside the worktree (and workspace repos) and refuses others", () => {
    expect(toWorktreeRel("/Users/me/.mirante/worktrees/shop/feat-a/src/x.rs", single)).toBe("src/x.rs");
    expect(toWorktreeRel("src/x.rs", single)).toBe("src/x.rs");
    expect(toWorktreeRel("./src/x.rs", single)).toBe("src/x.rs");
    expect(toWorktreeRel("/etc/passwd", single)).toBeNull();
    expect(toWorktreeRel("/Users/me/.mirante/worktrees/shop/feat-a-other/x", single)).toBeNull();
    expect(toWorktreeRel("../x", single)).toBeNull();
    expect(toWorktreeRel("/Users/me/.mirante/worktrees/shop/feat-a/../b/x", single)).toBeNull();
    expect(toWorktreeRel("~/x", single)).toBeNull();
    expect(toWorktreeRel("/m/_workspaces/x/bff/src/a.ts", ws)).toBe("bff/src/a.ts");
    expect(toWorktreeRel("/m/_workspaces/x/MIRANTE_WORKSPACE.md", ws)).toBe("MIRANTE_WORKSPACE.md");
    expect(toWorktreeRel("Site-B2C/README.md", ws)).toBe("Site-B2C/README.md");
  });

  it("/private/tmp and /tmp are the same place on macOS", () => {
    const t = { worktree_path: "/private/tmp/wt" };
    expect(toWorktreeRel("/tmp/wt/a.rs", t)).toBe("a.rs");
    expect(toWorktreeRel("/private/tmp/wt/a.rs", { worktree_path: "/tmp/wt" })).toBe("a.rs");
  });
});

describe("detectPaths", () => {
  const found = (s: string) => detectPaths(s).map(({ path, line, col }) => ({ path, line, col }));

  it("detects path[:line[:col]] tokens and ignores URLs", () => {
    expect(found("error at src/main.rs:12:5 and lib/x.ts:3")).toEqual([
      { path: "src/main.rs", line: 12, col: 5 },
      { path: "lib/x.ts", line: 3, col: undefined },
    ]);
    expect(found("see https://github.com/o/r/blob/main/src/a.ts and http://localhost:5173/x.js")).toEqual([]);
    expect(found("/tmp/w/src/a.ts#L7, README.md.")).toEqual([
      { path: "/tmp/w/src/a.ts", line: 7, col: undefined },
      { path: "README.md", line: undefined, col: undefined },
    ]);
    expect(found("[src/main.rs:3](/tmp/w/src/main.rs:3)")).toEqual([
      { path: "src/main.rs", line: 3, col: undefined },
      { path: "/tmp/w/src/main.rs", line: 3, col: undefined },
    ]);
    expect(found("version 1.2.3 costs 3.50 e.g. done")).toEqual([]);
  });

  it("reports where each one is, bounded", () => {
    const [m] = detectPaths("  at src/a.ts:9");
    expect([m.start, m.end, m.text]).toEqual([5, 15, "src/a.ts:9"]);
    expect(detectPaths("a/" + "b".repeat(500) + ".ts")).toEqual([]);
    expect(() => detectPaths("x/".repeat(20_000))).not.toThrow();
  });
});

describe("fileUrlTarget", () => {
  it("file:// with an empty or localhost host is a path; other hosts are not", () => {
    expect(fileUrlTarget("file:///tmp/w/src/a%20b.ts#L3")).toEqual({ path: "/tmp/w/src/a b.ts", line: 3 });
    expect(fileUrlTarget("file://localhost/tmp/w/a.ts")).toEqual({ path: "/tmp/w/a.ts" });
    expect(fileUrlTarget("file:///tmp/w/a.ts:12")).toEqual({ path: "/tmp/w/a.ts", line: 12 });
    expect(fileUrlTarget("file://evil.example/tmp/a.ts")).toBe("other-host");
    expect(fileUrlTarget("https://github.com/x")).toBeNull();
  });
});
