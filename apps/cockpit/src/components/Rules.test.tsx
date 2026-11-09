import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { DnaStatus, RuleLayer, RulesView } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { Brain } from "./Brain";
import { bytes, Rules } from "./Rules";

const layer = (over: Partial<RuleLayer>): RuleLayer => ({
  scope: "user",
  agent: "claude",
  kind: "instructions",
  path: "~/.claude/CLAUDE.md",
  exists: true,
  truncated: false,
  note: "",
  loaded: true,
  skills: 0,
  ...over,
});

const CLAUDE: RulesView = {
  agent: "claude",
  root: "/r/app",
  researched: "Claude Code 2.1.285",
  summary: { instruction_files: 3, instruction_bytes: 2048, settings_files: 1, skills: 2 },
  layers: [
    layer({ scope: "managed", path: "/Library/Application Support/ClaudeCode/CLAUDE.md", exists: false, loaded: false, note: "Organization policy" }),
    layer({
      content: "Be brief. <b>bold</b> <img src=x onerror=alert(1)>\n@a.md",
      size: 999,
      imports: [{ spec: "a.md", path: "~/.claude/a.md", exists: true, size: 10, content: "A body", truncated: false, imports: [{ spec: "CLAUDE.md", path: "~/.claude/CLAUDE.md", exists: true, truncated: false, error: "import cycle: this file is already being read above" }] }],
    }),
    layer({ kind: "skills", path: "~/.claude/skills/", content: "find: Find skills\nplay: Playwright", skills: 2 }),
    layer({ scope: "project", path: "/r/app/AGENTS.md", loaded: false, note: "Not read: CLAUDE.md wins" }),
    layer({ scope: "mirante", kind: "mode", path: "claude --permission-mode manual", content: "manual", loaded: false }),
  ],
};

const CODEX: RulesView = { ...CLAUDE, agent: "codex", researched: "Codex CLI 0.167.0", layers: [layer({ agent: "codex", path: "~/.codex/AGENTS.md", content: "codex global" })] };

const rules = (p: Record<string, unknown>) => (p.agent === "codex" ? CODEX : CLAUDE);

describe("Rules", () => {
  it("sums up, lists every layer by scope, and greys missing places", async () => {
    const h = makeEnv({ "rules.layers": rules });
    renderWith(h.env, <Rules at={{ repo: "/r/app" }} />);
    expect(await screen.findByRole("status")).toHaveTextContent("This Claude session reads 3 instruction files (2.0 KB), 1 settings files, 2 skills");
    expect(h.calls).toContainEqual(["rules.layers", { repo: "/r/app", agent: "claude" }]);
    const managed = screen.getByRole("listitem", { name: /Managed \/Library/ });
    expect(managed).toHaveClass("rule-missing");
    expect(within(managed).getByText("not present")).toBeInTheDocument();
    expect(screen.getByRole("listitem", { name: "Project /r/app/AGENTS.md" })).toHaveTextContent("not loaded at start");
    // No task: Mirante's layers are what it adds per task.
    expect(screen.getByRole("heading", { name: "Per task, Mirante adds" })).toBeInTheDocument();
  });

  it("shows contents as plain text, never as HTML", async () => {
    const h = makeEnv({ "rules.layers": rules });
    const { container } = renderWith(h.env, <Rules at={{ repo: "/r/app" }} />);
    await screen.findByRole("status");
    const pre = container.querySelector("pre.rule-text");
    expect(pre?.textContent).toContain("<b>bold</b> <img src=x onerror=alert(1)>");
    expect(container.querySelector("pre b, pre img, img")).toBeNull();
  });

  it("expands imports, and their imports, with cycles said", async () => {
    const h = makeEnv({ "rules.layers": rules });
    renderWith(h.env, <Rules at={{ repo: "/r/app" }} />);
    const btn = await screen.findByRole("button", { name: "Imports (1)" });
    expect(btn).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("A body")).toBeNull();
    await userEvent.click(btn);
    const list = screen.getByRole("list", { name: "Imported files" });
    await userEvent.click(within(list).getByRole("button", { name: "Show" }));
    expect(screen.getByText("A body")).toBeInTheDocument();
    expect(screen.getByText(/import cycle/)).toBeInTheDocument();
  });

  it("switches to Codex", async () => {
    const h = makeEnv({ "rules.layers": rules });
    renderWith(h.env, <Rules at={{ task: "t1" }} agent="claude" />);
    await screen.findByRole("status");
    await userEvent.click(screen.getByRole("button", { name: "Codex" }));
    expect(await screen.findByText("~/.codex/AGENTS.md")).toBeInTheDocument();
    expect(h.calls).toContainEqual(["rules.layers", { task: "t1", agent: "codex" }]);
  });

  it("an older daemon says to update; an error offers Retry", async () => {
    const h = makeEnv({ "rules.layers": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method") });
    renderWith(h.env, <Rules at={{ repo: "/r/app" }} />);
    expect(await screen.findByText(/Update Mirante/)).toBeInTheDocument();
    let fail = true;
    const h2 = makeEnv({ "rules.layers": () => (fail ? rpcError(4, "boom") : CLAUDE) });
    renderWith(h2.env, <Rules at={{ repo: "/r/b" }} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Rules unavailable");
    fail = false;
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText(/This Claude session reads/)).toBeInTheDocument();
  });

  it("formats sizes", () => {
    expect(bytes(812)).toBe("812 B");
    expect(bytes(4300)).toBe("4.2 KB");
  });
});

describe("Brain → Rules", () => {
  it("works with repo memory off", async () => {
    const off: DnaStatus = { repo: "/r/app", enabled: false, dir: false, notes: 0, stale: 0, unreviewed: 0, pending: 0, invalid: 0, head: null };
    const h = makeEnv({ "dna.status": off, "rules.layers": rules });
    renderWith(h.env, <Brain repo="/r/app" />);
    expect(await screen.findByRole("button", { name: /Turn on|Enable/i })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Rules" }));
    expect(await screen.findByText(/This Claude session reads/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Rules" })).toHaveAttribute("aria-pressed", "true");
    await userEvent.click(screen.getByRole("button", { name: "Notes" }));
    expect(await screen.findByRole("button", { name: /Turn on|Enable/i })).toBeInTheDocument();
  });
});
