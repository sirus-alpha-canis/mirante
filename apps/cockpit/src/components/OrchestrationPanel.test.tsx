import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { SpawnProposalView, TaskView } from "../lib/protocol";
import { task } from "../test/fixtures";
import { makeEnv, renderWith } from "../test/env";
import { OrchestrationPanel } from "./OrchestrationPanel";

const proposal: SpawnProposalView = {
  id: "sp1",
  parent_id: "p1",
  status: "pending",
  goal: "write the tests",
  agent: "claude",
  created_at: 1,
  parent_title: "goal of p1",
  parent_branch: "feat/p1",
};

function setup(parent: TaskView, others: TaskView[] = [], answers: Record<string, unknown> = {}) {
  const h = makeEnv({ "task.spawn_proposals": [proposal], ...answers });
  h.load([parent, ...others]);
  const onSelect = vi.fn();
  const view = () => h.env.cockpit.getState().tasks[parent.id];
  const r = renderWith(h.env, <OrchestrationPanel t={parent} onSelect={onSelect} />);
  return { ...h, onSelect, view, r };
}

describe("OrchestrationPanel", () => {
  it("Orchestration toggle says it applies from the next session", async () => {
    const parent = task("p1", { live: true, orchestration: false, orchestrator_live: false });
    const h = setup(parent, [], { "task.orchestrate": { ...parent, orchestration: true, orchestrator_live: false } });
    const sw = screen.getByRole("switch", { name: /Orchestration/ });
    expect(sw).not.toBeChecked();
    await userEvent.click(sw);
    expect(h.calls).toContainEqual(["task.orchestrate", { task: "p1", enabled: true }]);
    await waitFor(() => expect(h.view().orchestration).toBe(true));
    h.r.unmount();
    renderWith(h.env, <OrchestrationPanel t={h.view()} onSelect={h.onSelect} />);
    expect(screen.getByRole("switch", { name: /Orchestration/ })).toBeChecked();
    expect(screen.getByText(/applies from the next session/)).toBeInTheDocument();
  });

  it("pending proposals are cards, refetched when the count changes", async () => {
    const h = setup(task("p1", { orchestration: true, spawn_pending: 1 }));
    expect(await screen.findByRole("article", { name: "Proposed sub-task: write the tests" })).toBeInTheDocument();
    expect(h.calls).toContainEqual(["task.spawn_proposals", { task: "p1", status: "pending" }]);
  });

  it("approved sub-tasks are listed under their parent; a sub-task links back", async () => {
    const child = task("c1", { parent_id: "p1", child_kind: "spawn", title: "write the tests", status: "working" });
    const thread = task("th", { parent_id: "p1", child_kind: "thread", title: "PR #1" });
    const h = setup(task("p1", { orchestration: true }), [child, thread]);
    const list = screen.getByRole("list", { name: "Sub-tasks" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    await userEvent.click(within(list).getByRole("button", { name: "write the tests" }));
    expect(h.onSelect).toHaveBeenCalledWith("c1");
    h.r.unmount();
    renderWith(h.env, <OrchestrationPanel t={child} onSelect={h.onSelect} />);
    await userEvent.click(screen.getByRole("button", { name: /Sub-task of goal of p1/ }));
    expect(h.onSelect).toHaveBeenCalledWith("p1");
    expect(screen.queryByRole("switch")).toBeNull();
  });

  it("a thread shows nothing", () => {
    const { r } = setup(task("p1"), []);
    r.unmount();
    const h = makeEnv();
    const th = task("th", { parent_id: "p1", child_kind: "thread" });
    h.load([th]);
    const { container } = renderWith(h.env, <OrchestrationPanel t={th} onSelect={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
  });
});
