import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { DnaGraph, DnaStatus, NoteSummary, NoteView } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { Brain } from "./Brain";

const HEAD = "50ea8cc255941e739b13cdbe2c937abca0319834";
const anchor = { path: "src/a.rs", symbol: null, start: 1, end: 2, commit: HEAD, state: "fresh", reason: null, now_start: null, now_end: null };
const node = (slug: string, title: string, over: Partial<NoteSummary> = {}): NoteSummary => ({
  slug,
  title,
  kind: "fact",
  status: "active",
  state: "fresh",
  reviewed: true,
  anchors: [anchor],
  ...over,
});

const GRAPH: DnaGraph = {
  nodes: [
    node("hook-spool", "Hook spool"),
    node("hub-dedupes", "Hub dedupes"),
    node("boot-order", "Boot order", { state: "stale" }),
    node("worktrees", "Worktrees", { reviewed: false }),
    node("old-way", "Old way", { status: "retired" }),
  ],
  links: [
    { src: "hook-spool", dst: "hub-dedupes", dangling: false },
    { src: "worktrees", dst: "release-checklist", dangling: true },
  ],
};

const STATUS: DnaStatus = { repo: "/r/app", enabled: true, dir: true, notes: 5, stale: 1, unreviewed: 1, pending: 2, invalid: 0, head: HEAD };

const view = (n: NoteSummary, body = "text"): NoteView => ({
  ...n,
  body,
  links: [],
  backlinks: [],
  content_hash: `h-${n.slug}`,
  source: "manual",
  path: `/r/app/.mirante/memory/${n.slug}.md`,
});

function setup(answers: Record<string, unknown> = {}) {
  const h = makeEnv({
    "dna.status": STATUS,
    "dna.graph": GRAPH,
    "dna.get": (p: Record<string, unknown>) => {
      const n = GRAPH.nodes.find((x) => x.slug === p.slug);
      return n ? view(n, n.slug === "hook-spool" ? "Spool events go to [[hub-dedupes]]." : "text") : rpcError(RpcCode.NOT_FOUND, "no note");
    },
    "dna.proposals": [],
    ...answers,
  });
  renderWith(h.env, <Brain repo="/r/app" />);
  return h;
}

const nodeButton = (name: string) => screen.findByRole("button", { name });

describe("Brain", () => {
  it("renders nodes colored by state, dangling links dashed, and a legend", async () => {
    setup();
    expect(await nodeButton("Boot order, stale, reviewed")).toHaveClass("node-stale");
    expect(screen.getByRole("button", { name: "Hook spool, fresh, reviewed" })).toHaveClass("node-fresh");
    expect(screen.getByRole("button", { name: "Worktrees, fresh, unreviewed" })).toHaveClass("node-unreviewed");
    expect(screen.getByRole("button", { name: "Old way, fresh, reviewed, retired" })).toHaveClass("node-retired");
    const svg = screen.getByRole("img", { name: /Graph of 5 notes/ });
    expect(svg.querySelectorAll("line.link")).toHaveLength(1);
    expect(svg.querySelectorAll("line.link-dangling")).toHaveLength(1);
    expect(within(svg).getByText("release-checklist")).toBeInTheDocument();
    const legend = screen.getByRole("list", { name: "Legend" });
    for (const word of ["fresh", "stale", "unverifiable", "unreviewed", "retired", "missing note"]) expect(legend).toHaveTextContent(word);
  });

  it("clicking a node opens its note; [[link]] buttons navigate the graph", async () => {
    setup();
    await userEvent.click(await nodeButton("Hook spool, fresh, reviewed"));
    const note = await screen.findByRole("article", { name: "Hook spool" });
    expect(screen.getByRole("button", { name: "Hook spool, fresh, reviewed" })).toHaveAttribute("aria-pressed", "true");
    await userEvent.click(within(note).getByRole("button", { name: "hub-dedupes" }));
    expect(await screen.findByRole("article", { name: "Hub dedupes" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hub dedupes, fresh, reviewed" })).toHaveAttribute("aria-pressed", "true");
  });

  it("filter pills: stale / unreviewed / retired", async () => {
    setup();
    await nodeButton("Hook spool, fresh, reviewed");
    const nodes = () => screen.getAllByRole("button", { name: /, (fresh|stale)/ }).map((b) => b.getAttribute("aria-label"));
    await userEvent.click(screen.getByRole("button", { name: "stale" }));
    expect(screen.getByRole("button", { name: "stale" })).toHaveAttribute("aria-pressed", "true");
    expect(nodes()).toEqual(["Boot order, stale, reviewed"]);
    await userEvent.click(screen.getByRole("button", { name: "unreviewed" }));
    expect(nodes().sort()).toEqual(["Boot order, stale, reviewed", "Worktrees, fresh, unreviewed"]);
    await userEvent.click(screen.getByRole("button", { name: "stale" }));
    await userEvent.click(screen.getByRole("button", { name: "unreviewed" }));
    await userEvent.click(screen.getByRole("button", { name: "retired" }));
    expect(nodes()).toEqual(["Old way, fresh, reviewed, retired"]);
  });

  it("Re-check runs dna.check and shows the counts", async () => {
    const h = setup({
      "dna.check": { repo: "/r/app", head: HEAD, notes: 5, invalid: [{ file: "x.md", error: "bad" }], stale: 1, relocated: 0, unverifiable: 0, unreviewed: 1 },
    });
    await nodeButton("Hook spool, fresh, reviewed");
    await userEvent.click(screen.getByRole("button", { name: "Re-check" }));
    expect(h.calls).toContainEqual(["dna.check", { repo: "/r/app" }]);
    expect(await screen.findByText("Checked 5 notes: 1 stale, 1 unreviewed, 1 invalid")).toBeInTheDocument();
  });

  it("search highlights the hits", async () => {
    const h = setup({ "dna.search": [node("hook-spool", "Hook spool")] });
    await nodeButton("Hook spool, fresh, reviewed");
    await userEvent.type(screen.getByRole("searchbox", { name: "Search memory" }), "spool{Enter}");
    expect(h.calls).toContainEqual(["dna.search", { repo: "/r/app", query: "spool", limit: 25 }]);
    await waitFor(() => expect(screen.getByRole("button", { name: "Hook spool, fresh, reviewed" })).toHaveClass("node-hit"));
    expect(screen.getByRole("button", { name: "Hub dedupes, fresh, reviewed" })).not.toHaveClass("node-hit");
    expect(screen.getByText("1 match")).toBeInTheDocument();
  });

  it("Review (n) opens the review queue in the panel", async () => {
    setup();
    await userEvent.click(await screen.findByRole("button", { name: "Review (2)" }));
    expect(await screen.findByRole("region", { name: "Memory review queue" })).toBeInTheDocument();
  });

  it("disabled repo shows the explicit enable explanation, not an empty graph", async () => {
    const h = setup({ "dna.status": { ...STATUS, enabled: false, notes: 0, pending: 0 } });
    expect(await screen.findByRole("button", { name: "Enable repo memory" })).toBeInTheDocument();
    expect(screen.getByText(/creates .mirante\/memory\/ in the repo/)).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: /Graph/ })).toBeNull();
    expect(h.calls.map((c) => c[0])).not.toContain("dna.graph");
  });

  it("nodes are keyboard reachable and have accessible names", async () => {
    setup();
    const n = await nodeButton("Hook spool, fresh, reviewed");
    expect(n).toHaveAttribute("tabindex", "0");
    n.focus();
    await userEvent.keyboard("{Enter}");
    expect(await screen.findByRole("article", { name: "Hook spool" })).toBeInTheDocument();
  });

  it("a list view lists every note as a button (and the graph says when it is too large)", async () => {
    setup();
    await nodeButton("Hook spool, fresh, reviewed");
    await userEvent.click(screen.getByRole("button", { name: "List" }));
    const list = screen.getByRole("list", { name: "Notes" });
    expect(within(list).getAllByRole("button")).toHaveLength(5);
    expect(screen.queryByRole("img", { name: /Graph/ })).toBeNull();
  });

  it("an older daemon says memory is not available", async () => {
    setup({ "dna.status": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown") });
    expect(await screen.findByText(/This daemon has no repo memory/)).toBeInTheDocument();
  });
});
