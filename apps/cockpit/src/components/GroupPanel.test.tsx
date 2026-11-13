import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { GroupView, Synthesis, TaskGroup, TaskView } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith } from "../test/env";
import { GroupPanel } from "./GroupPanel";
import { type PaneApi, PaneContext } from "./paneContext";

const g: TaskGroup = { id: "g1", kind: "compare", title: "fix login", goal: "fix login", branch: "mirante/fix-login", created_at: 1 };
const c1 = task("c1", { status: "working", group_id: "g1", group_role: "child", group: g, branch: "mirante/fix-login--1-claude", title: "fix login" });
const c2 = task("c2", { status: "idle", group_id: "g1", group_role: "child", group: g, agent: "codex", branch: "mirante/fix-login--2-codex", title: "fix login", created_at: 2 });

const view = (members: TaskView[]): GroupView => ({
  group: g,
  members: members.map((v) => ({
    view: v,
    last_answer: v.id === "c2" ? "Done: <b>fixed</b> the form\nline 2\nline 3\nline 4" : null,
    diff: v.id === "c2" ? { files: 3, added: 10, removed: 2, untracked: 1 } : null,
  })),
});

function setup(answers: Record<string, unknown> = {}, tasks = [c1, c2], events = [] as ReturnType<typeof ev>[]) {
  const h = makeEnv({ "group.get": view(tasks), ...answers });
  h.load(tasks, events);
  const onSelect = vi.fn();
  const pane: PaneApi = { taskId: "c1", openFile: vi.fn(), openTarget: vi.fn() };
  renderWith(
    h.env,
    <PaneContext.Provider value={pane}>
      <GroupPanel taskId="c1" onSelect={onSelect} />
    </PaneContext.Provider>,
  );
  return { ...h, onSelect, pane };
}

const card = (branch: string) => screen.getByRole("article", { name: new RegExp(branch) });

describe("GroupPanel", () => {
  it("cards show status stripe, title, agent, last answer (as text) and diff stat; the header has the rollup", async () => {
    setup();
    const panel = screen.getByRole("region", { name: "Fan-out fix login" });
    expect(within(panel).getByText("2 children: 1 working, 1 idle")).toBeInTheDocument();
    const two = card("mirante/fix-login--2-codex");
    expect(two).toHaveClass("card-idle");
    expect(within(two).getByText("@codex")).toBeInTheDocument();
    await waitFor(() => expect(within(two).getByText(/Done: <b>fixed<\/b> the form/)).toBeInTheDocument());
    expect(two.querySelector("b")).toBeNull();
    expect(within(two).getByText("3 files +10 −2 · 1 untracked")).toBeInTheDocument();
    expect(within(card("mirante/fix-login--1-claude")).getByText("this task")).toBeInTheDocument();
  });

  it("Steer sends to that child only", async () => {
    const h = setup({ "message.send": { delivered: "steer", queued: 0 } });
    const two = card("mirante/fix-login--2-codex");
    await userEvent.type(within(two).getByLabelText("Message to this task"), "use the new API");
    await userEvent.click(within(two).getByRole("button", { name: "Steer" }));
    expect(h.calls.filter(([m]) => m === "message.send")).toEqual([["message.send", { task: "c2", text: "use the new API", mode: "steer" }]]);
  });

  it("Open goes to the child's Detail; Terminal and Diff open it in the side pane", async () => {
    const h = setup();
    const two = card("mirante/fix-login--2-codex");
    await userEvent.click(within(two).getByRole("button", { name: "Open" }));
    expect(h.onSelect).toHaveBeenCalledWith("c2");
    await userEvent.click(within(two).getByRole("button", { name: "Terminal" }));
    expect(h.pane.openTarget).toHaveBeenCalledWith({ k: "terminal", task: "c2" });
    await userEvent.click(within(two).getByRole("button", { name: "Diff beside" }));
    expect(h.pane.openTarget).toHaveBeenCalledWith({ k: "diff", task: "c2" });
  });

  it("a sibling needing you shows its permission card inline (the same PermissionCard)", async () => {
    const h = setup({ "permission.answer": { status: "allowed_once" } }, [c1, { ...c2, status: "needs_you", needs_you: 1 }], [
      ev(1, "c2", "permission_requested", { permission_id: "p9", tool: "Bash", detail: "npm test" }),
    ]);
    const perm = within(card("mirante/fix-login--2-codex")).getByRole("group", { name: "Permission request: Bash" });
    await userEvent.click(within(perm).getByRole("button", { name: "Allow once" }));
    expect(h.calls).toContainEqual(["permission.answer", { id: "p9", allow: true }]);
  });

  it("Synthesize previews the fenced request read-only, then creates the synthesizer with that preview's hash and starts it", async () => {
    const preview: Synthesis = { text: "Read the results below <<<m1 …", expect: "h1" };
    const synth = task("s1", { group_id: "g1", group_role: "synth", group: g, branch: "mirante/fix-login--synth", live: false });
    const h = setup({
      "group.synthesize": (p: Record<string, unknown>) => (p.preview ? preview : { ...preview, task: synth }),
      "session.start": { boot: "booting" },
    });
    await userEvent.click(screen.getByRole("button", { name: "Synthesize…" }));
    await waitFor(() => expect(screen.getByLabelText("The request, as the agent will read it")).toHaveTextContent("Read the results below <<<m1 …"));
    expect(screen.getByLabelText("The request, as the agent will read it").tagName).not.toBe("TEXTAREA");
    expect(h.calls.filter(([m]) => m === "session.start")).toEqual([]);
    await userEvent.click(screen.getByRole("button", { name: "Create the synthesizer and start it" }));
    await waitFor(() => expect(h.calls.filter(([m]) => m === "session.start")).toHaveLength(1));
    expect(h.calls.filter(([m]) => m === "group.synthesize").map(([, p]) => p)).toEqual([
      { group: "g1", preview: true },
      { group: "g1", expect: "h1" },
    ]);
    expect(h.calls.find(([m]) => m === "session.start")?.[1]).toEqual({ task: "s1", prompt: preview.text });
  });

  it("Synthesize into a child queues it there instead", async () => {
    const preview: Synthesis = { text: "Compare with your siblings", expect: "h2" };
    const h = setup({
      "group.synthesize": (p: Record<string, unknown>) => (p.preview ? preview : { ...preview, sent: { delivered: "queue", queued: 1 } }),
    });
    await userEvent.click(screen.getByRole("button", { name: "Synthesize…" }));
    await userEvent.selectOptions(screen.getByLabelText("Who synthesizes"), "c2");
    await waitFor(() => expect(screen.getByLabelText("The request, as the agent will read it")).toHaveTextContent("Compare with your siblings"));
    await userEvent.click(screen.getByRole("button", { name: "Send it to that task" }));
    await waitFor(() => expect(h.calls.filter(([m]) => m === "group.synthesize")).toHaveLength(3));
    expect(h.calls.filter(([m]) => m === "group.synthesize").map(([, p]) => p).at(-1)).toEqual({ group: "g1", into: "c2", expect: "h2" });
    expect(h.calls.filter(([m]) => m === "session.start")).toEqual([]);
  });
});
