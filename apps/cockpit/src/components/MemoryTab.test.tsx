import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { TaskView } from "../lib/protocol";

vi.mock("./TerminalTab", () => ({
  TerminalTab: ({ t }: { t: TaskView }) => <div aria-label={`Terminal of ${t.branch}`}>terminal here</div>,
}));

import { RpcCode } from "../lib/protocol";
import type { AnchorView, NoteSummary, NoteView, ProposalView, TaskMemoryContext } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { Detail } from "./Detail";

const HEAD = "50ea8cc255941e739b13cdbe2c937abca0319834";

const anchor = (over: Partial<AnchorView> = {}): AnchorView => ({
  path: "src/spool.rs",
  symbol: "spool",
  start: 10,
  end: 20,
  commit: HEAD,
  state: "fresh",
  reason: null,
  now_start: null,
  now_end: null,
  ...over,
});

const summary = (slug: string, title: string, over: Partial<NoteSummary> = {}): NoteSummary => ({
  slug,
  title,
  kind: "gotcha",
  status: "active",
  state: "fresh",
  reviewed: true,
  anchors: [anchor()],
  ...over,
});

const note = (slug: string, title: string, over: Partial<NoteView> = {}): NoteView => ({
  ...summary(slug, title),
  body: "plain body",
  links: [],
  backlinks: [],
  content_hash: `hash-${slug}`,
  source: "manual",
  path: `/r/app/.mirante/memory/${slug}.md`,
  ...over,
});

const proposal = (over: Partial<ProposalView> = {}): ProposalView => ({
  id: "p1",
  repo: "/r/app",
  task_id: "t1",
  origin: "agent",
  title: "Tokens refresh in the session layer",
  kind: "fact",
  body: "b",
  anchors: [{ path: "src/login.rs", start: 1, end: 3, commit: HEAD, fingerprint: "0123456789abcdef" }],
  redactions: 1,
  status: "pending",
  slug: null,
  decision_note: "",
  created_at: 1,
  decided_at: null,
  ...over,
});

const context = (over: Partial<TaskMemoryContext> = {}): TaskMemoryContext => ({
  enabled: true,
  relevant: [],
  at_risk: [],
  proposals: [],
  ...over,
});

function open(answers: Record<string, unknown>, tab: "Memory" | "Steps" = "Memory") {
  const h = makeEnv(answers);
  h.load([task("t1", { repo_path: "/r/app", status: "working" })]);
  renderWith(h.env, <Detail taskId="t1" onSelect={() => {}} initialTab={tab} />);
  return h;
}

describe("Memory tab", () => {
  it("memory tab: disabled repo offers an explicit enable", async () => {
    let enabled = false;
    const h = open({
      "dna.task_context": () => context({ enabled }),
      "dna.enable": () => {
        enabled = true;
        return { repo: "/r/app", enabled: true };
      },
    });
    expect(await screen.findByText(/creates .mirante\/memory\/ in the repo/)).toBeInTheDocument();
    await userEvent.click(await screen.findByRole("button", { name: "Enable repo memory" }));
    expect(h.calls.filter((c) => c[0] === "dna.enable").map((c) => c[1])).toEqual([{ repo: "/r/app" }]);
    expect(await screen.findByRole("region", { name: "Relevant notes" })).toBeInTheDocument();
  });

  it("memory tab: at-risk notes first, then relevant, then this task's proposals", async () => {
    open({
      "dna.task_context": context({
        at_risk: [{ ...summary("hook-spool", "Hook spool"), reason: "src/spool.rs: the code cited at src/spool.rs:10-20 changed" }],
        relevant: [summary("worktrees", "Worktree per task", { reviewed: false })],
        proposals: [proposal()],
      }),
    });
    const risk = await screen.findByRole("region", { name: "At risk from this task" });
    expect(within(risk).getByRole("button", { name: "Hook spool" })).toBeInTheDocument();
    expect(risk).toHaveTextContent("the code cited at src/spool.rs:10-20 changed");
    expect(risk).toHaveTextContent("src/spool.rs#spool:10-20");
    const relevant = screen.getByRole("region", { name: "Relevant notes" });
    expect(within(relevant).getByRole("button", { name: "Worktree per task" })).toBeInTheDocument();
    expect(relevant).toHaveTextContent("unreviewed");
    const props = screen.getByRole("region", { name: "Proposals from this task" });
    expect(props).toHaveTextContent("Tokens refresh in the session layer");
    expect(props).toHaveTextContent("pending");
    expect(props).toHaveTextContent("1 secret was redacted");
    const order = ["At risk from this task", "Relevant notes", "Proposals from this task"].map((n) => screen.getByRole("region", { name: n }));
    expect(order[0].compareDocumentPosition(order[1]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(order[1].compareDocumentPosition(order[2]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("opening a note shows anchors with states and renders the body as text only", async () => {
    const h = open({
      "dna.task_context": context({ relevant: [summary("hook-spool", "Hook spool")] }),
      "dna.get": (p: Record<string, unknown>) =>
        p.slug === "other"
          ? note("other", "The other note")
          : note("hook-spool", "Hook spool", {
              body: "<img src=x onerror=alert(1)> [[other]]",
              anchors: [anchor({ state: "relocated", now_start: 14, now_end: 24 }), anchor({ path: "src/b.rs", symbol: null, state: "stale", reason: "the code cited at src/b.rs:10-20 changed" })],
              state: "stale",
            }),
    });
    await userEvent.click(await screen.findByRole("button", { name: "Hook spool" }));
    const view = await screen.findByRole("article", { name: "Hook spool" });
    expect(view.querySelector("img")).toBeNull();
    expect(view).toHaveTextContent("<img src=x onerror=alert(1)>");
    expect(view).toHaveTextContent("stale: cited code changed");
    expect(view).toHaveTextContent("now L14-24");
    expect(view).toHaveTextContent("50ea8cc");
    expect(view).toHaveTextContent("the code cited at src/b.rs:10-20 changed");
    expect(h.calls).toContainEqual(["dna.get", { task: "t1", slug: "hook-spool" }]);
    await userEvent.click(within(view).getByRole("button", { name: "other" }));
    expect(await screen.findByRole("article", { name: "The other note" })).toBeInTheDocument();
  });

  it("confirm and retire send the shown note's hash; a changed note asks to reload", async () => {
    let changed = true;
    const h = open({
      "dna.task_context": context({ relevant: [summary("hook-spool", "Hook spool")] }),
      "dna.get": () => note("hook-spool", "Hook spool", { content_hash: changed ? "h1" : "h2" }),
      "dna.note_action": (p: Record<string, unknown>) =>
        p.expect_hash === "h1"
          ? rpcError(RpcCode.CONFLICT, "the note changed on disk since it was read; reload and try again")
          : note("hook-spool", "Hook spool", { status: "retired", content_hash: "h3" }),
    });
    await userEvent.click(await screen.findByRole("button", { name: "Hook spool" }));
    await userEvent.click(await screen.findByRole("button", { name: "Confirm" }));
    expect(h.calls).toContainEqual(["dna.note_action", { repo: "/r/app", slug: "hook-spool", action: "confirm", expect_hash: "h1" }]);
    expect(await screen.findByRole("alert")).toHaveTextContent("The note changed since it was shown. Reload it and try again.");
    changed = false;
    await userEvent.click(screen.getByRole("button", { name: "Reload" }));
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    await userEvent.click(screen.getByRole("button", { name: "Retire" }));
    expect(h.calls).toContainEqual(["dna.note_action", { repo: "/r/app", slug: "hook-spool", action: "retire", expect_hash: "h2" }]);
    expect(await screen.findByText("retired")).toBeInTheDocument();
  });

  it("asks the agent to propose memory with a queued operator message", async () => {
    const h = open({ "dna.task_context": context(), "message.send": { delivered: "queue", queued: 1 } });
    await userEvent.click(await screen.findByRole("button", { name: "Ask agent to propose memory" }));
    const sent = h.calls.find((c) => c[0] === "message.send");
    expect(sent?.[1]).toMatchObject({ task: "t1", mode: "queue" });
    expect(String(sent?.[1].text)).toContain("memory_propose");
    expect(await screen.findByText(/queued for the agent/)).toBeInTheDocument();
  });

  it("tab label counts at-risk notes", async () => {
    open({ "dna.task_context": context({ at_risk: [{ ...summary("a", "A"), reason: "r" }] }) }, "Steps");
    expect(await screen.findByRole("tab", { name: "Memory (1 at risk)" })).toBeInTheDocument();
  });

  it("refetches when the repo's memory changes", async () => {
    const h = open({ "dna.task_context": context() });
    await screen.findByRole("region", { name: "Relevant notes" });
    const n = () => h.calls.filter((c) => c[0] === "dna.task_context").length;
    const before = n();
    h.env.cockpit.dispatch({ type: "event", event: ev(50, null, "dna.changed", { repo: "/r/app", slug: "x" }) });
    await waitFor(() => expect(n()).toBe(before + 1));
  });

  it("an older daemon says memory is not available", async () => {
    open({ "dna.task_context": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method") });
    expect(await screen.findByText(/This daemon has no repo memory/)).toBeInTheDocument();
  });
});
