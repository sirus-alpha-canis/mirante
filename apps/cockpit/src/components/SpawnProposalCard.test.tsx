import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { SpawnProposalView } from "../lib/protocol";
import { task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { SpawnProposalCard } from "./SpawnProposalCard";

const p = (over: Partial<SpawnProposalView> = {}): SpawnProposalView => ({
  id: "sp1",
  parent_id: "p1",
  status: "pending",
  goal: "Add tests for <script>alert(1)</script> login\nsecond line",
  agent: "codex",
  title: "Login tests",
  why: "the <b>form</b> has none",
  created_at: 1,
  parent_title: "fix login",
  parent_branch: "mirante/login",
  ...over,
});

function setup(view = p(), answers: Record<string, unknown> = {}) {
  const child = task("c1", { parent_id: "p1", child_kind: "spawn", branch: "mirante/login--1-login-tests" });
  const h = makeEnv({ "task.spawn_review": { proposal: { ...view, status: "accepted", child_id: "c1" }, child: child, started: { task: "c1", boot: "booting", resumed: false, screen: "" } }, ...answers });
  h.load([task("p1", { title: "fix login", branch: "mirante/login" })]);
  const onDone = vi.fn();
  const onSelect = vi.fn();
  renderWith(h.env, <SpawnProposalCard p={view} onDone={onDone} onSelect={onSelect} />);
  return { ...h, onDone, onSelect };
}

describe("SpawnProposalCard", () => {
  it('shows goal/agent/why as text nodes with "proposed by the agent"', () => {
    setup();
    const card = screen.getByRole("article", { name: "Proposed sub-task: Login tests" });
    expect(within(card).getByText("proposed by the agent")).toBeInTheDocument();
    expect(within(card).getByText("@codex")).toBeInTheDocument();
    expect(within(card).getByText(/Add tests for <script>alert\(1\)<\/script> login/)).toBeInTheDocument();
    expect(within(card).getByText(/the <b>form<\/b> has none/)).toBeInTheDocument();
    expect(card.querySelector("script, b")).toBeNull();
  });

  it("Create child approves it as proposed, and the child is put in the store", async () => {
    const h = setup();
    await userEvent.click(screen.getByRole("button", { name: "Create child" }));
    await waitFor(() => expect(h.onDone).toHaveBeenCalled());
    expect(h.calls.filter(([m]) => m === "task.spawn_review")).toEqual([["task.spawn_review", { id: "sp1", accept: true }]]);
    expect(h.env.cockpit.getState().tasks.c1?.child_kind).toBe("spawn");
  });

  it("Edit then Create child sends only the person's changes", async () => {
    const h = setup(p({ repos: ["site", "bff", "docs"] }));
    await userEvent.click(screen.getByRole("button", { name: "Edit" }));
    const goal = screen.getByLabelText("Goal");
    await userEvent.clear(goal);
    await userEvent.type(goal, "Add login tests only");
    await userEvent.selectOptions(screen.getByLabelText("Agent"), "claude");
    expect(screen.getByRole("checkbox", { name: "site" })).toBeDisabled();
    await userEvent.click(screen.getByRole("checkbox", { name: "docs" }));
    await userEvent.click(screen.getByRole("button", { name: "Create child" }));
    await waitFor(() => expect(h.onDone).toHaveBeenCalled());
    expect(h.calls.filter(([m]) => m === "task.spawn_review")).toEqual([
      ["task.spawn_review", { id: "sp1", accept: true, edits: { goal: "Add login tests only", agent: "claude", repos: ["site", "bff"] } }],
    ]);
  });

  it("Reject", async () => {
    const h = setup(p(), { "task.spawn_review": { proposal: { ...p(), status: "rejected" } } });
    await userEvent.click(screen.getByRole("button", { name: "Reject" }));
    await waitFor(() => expect(h.onDone).toHaveBeenCalled());
    expect(h.calls).toContainEqual(["task.spawn_review", { id: "sp1", accept: false }]);
  });

  it("a child that could not start says so, and a refusal is shown", async () => {
    const h = setup(p(), {
      "task.spawn_review": { proposal: { ...p(), status: "accepted" }, child: task("c1"), start_error: "memory is tight" },
    });
    await userEvent.click(screen.getByRole("button", { name: "Create child" }));
    expect(await screen.findByText(/Created, but it did not start: memory is tight/)).toBeInTheDocument();
    expect(h.onDone).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Open the child" }));
    expect(h.onSelect).toHaveBeenCalledWith("c1");
  });

  it("a daemon refusal is shown on the card", async () => {
    setup(p(), { "task.spawn_review": rpcError(-32002, "that proposal was already decided (rejected)") });
    await userEvent.click(screen.getByRole("button", { name: "Create child" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("already decided");
  });
});
