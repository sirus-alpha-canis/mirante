import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { SubagentView } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith } from "../test/env";
import { SubagentLane, SubagentPane } from "./SubagentLane";
import { type PaneApi, PaneContext } from "./paneContext";

const now = Date.now();
const lanes: SubagentView[] = [
  {
    id: "a1",
    agent_type: "Explore",
    description: "Find the login handlers",
    status: "done",
    started_ms: now - 5 * 60_000,
    ended_ms: now - 2 * 60_000,
    tools: 7,
    last_text: "Found it: <b>src/login.rs</b>",
    permissions: [],
    pending: 0,
  },
  {
    id: "a2",
    agent_type: "general-purpose",
    description: "Create c/new.txt",
    status: "running",
    started_ms: now - 60_000,
    tools: 1,
    last_text: "I'll run those commands.",
    permissions: ["p1"],
    pending: 1,
  },
];

const events = [
  ev(1, "t1", "subagent.started", { agent_id: "a1", agent_type: "Explore" }),
  ev(2, "t1", "step", { type: "tool", tool: "Read", summary: "src/login.rs", ok: true, agent: "Explore", agent_id: "a1" }),
  ev(3, "t1", "step", { type: "tool", tool: "Grep", summary: "fn login", ok: true, agent: "Explore", agent_id: "a1" }),
  ev(4, "t1", "step", { type: "tool", tool: "Bash", summary: "cargo test", ok: true, agent: null, agent_id: null }),
  ev(5, "t1", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "touch c/new.txt" }),
  ev(6, "t1", "subagent.permission", { permission_id: "p1", agent_id: "a2", agent_type: "general-purpose" }),
];

function setup(list: SubagentView[] = lanes) {
  const h = makeEnv({ "task.subagents": list });
  h.load([task("t1", { status: "needs_you", needs_you: 1 })], events);
  const pane: PaneApi = { taskId: "t1", openFile: vi.fn(), openTarget: vi.fn() };
  renderWith(
    h.env,
    <PaneContext.Provider value={pane}>
      <SubagentLane taskId="t1" />
    </PaneContext.Provider>,
  );
  return { ...h, pane };
}

const row = (name: RegExp) => screen.getByRole("listitem", { name });

describe("SubagentLane", () => {
  it("lists running and finished subagents with type, description, tool count, last text", async () => {
    const h = setup();
    const lane = await screen.findByRole("region", { name: "Claude's own agents (read-only)" });
    expect(h.calls).toContainEqual(["task.subagents", { task: "t1" }]);
    const one = within(lane).getByRole("listitem", { name: /Agent Explore: Find the login handlers/ });
    expect(within(one).getByText("done")).toBeInTheDocument();
    expect(within(one).getByText("7 tool calls")).toBeInTheDocument();
    expect(within(one).getByText("3 min")).toBeInTheDocument();
    // The agent's text, as text (never markup).
    expect(within(one).getByText("Found it: <b>src/login.rs</b>")).toBeInTheDocument();
    expect(one.querySelector("b")).toBeNull();
    const two = row(/Agent general-purpose: Create c\/new.txt/);
    expect(within(two).getByText("running")).toBeInTheDocument();
    expect(within(two).getByText("1 tool call")).toBeInTheDocument();
  });

  it("a permission request is attributed to the subagent that asked", async () => {
    setup();
    await screen.findByRole("region", { name: "Claude's own agents (read-only)" });
    const two = row(/Agent general-purpose/);
    expect(within(two).getByText("asks for permission: Bash touch c/new.txt")).toBeInTheDocument();
  });

  it("steps tagged with an agent group under its lane", async () => {
    setup();
    await screen.findByRole("region", { name: "Claude's own agents (read-only)" });
    const one = row(/Agent Explore/);
    // Read + Grep are Explore's; the main agent's Bash is not.
    expect(within(one).getByText("read 1 file, searched once")).toBeInTheDocument();
    expect(within(one).queryByText(/cargo test/)).toBeNull();
  });

  it("opening one shows its last text in the side pane", async () => {
    const h = setup();
    await screen.findByRole("region", { name: "Claude's own agents (read-only)" });
    await userEvent.click(within(row(/Agent Explore/)).getByRole("button", { name: "Open beside" }));
    expect(h.pane.openTarget).toHaveBeenCalledWith({ k: "subagent", id: "a1" });
  });

  it("no steer or stop control is rendered", async () => {
    setup();
    const lane = await screen.findByRole("region", { name: "Claude's own agents (read-only)" });
    expect(within(lane).queryByRole("textbox")).toBeNull();
    const names = within(lane)
      .getAllByRole("button")
      .map((b) => b.textContent);
    expect(names.every((n) => n === "Open beside")).toBe(true);
  });

  it("renders nothing while Claude has no agents", async () => {
    const h = setup([]);
    await waitFor(() => expect(h.calls.some(([m]) => m === "task.subagents")).toBe(true));
    expect(screen.queryByRole("region", { name: "Claude's own agents (read-only)" })).toBeNull();
  });

  it("refetches when a subagent event arrives", async () => {
    const h = setup();
    await screen.findByRole("region", { name: "Claude's own agents (read-only)" });
    const before = h.calls.filter(([m]) => m === "task.subagents").length;
    h.env.cockpit.dispatch({ type: "event", event: ev(7, "t1", "subagent.stopped", { agent_id: "a2", agent_type: "general-purpose" }) });
    await waitFor(() => expect(h.calls.filter(([m]) => m === "task.subagents").length).toBeGreaterThan(before));
  });
});

describe("SubagentPane", () => {
  it("shows the agent's latest text and its steps", async () => {
    const h = makeEnv({ "task.subagents": lanes });
    h.load([task("t1")], events);
    renderWith(h.env, <SubagentPane taskId="t1" id="a1" />);
    expect(await screen.findByText("Found it: <b>src/login.rs</b>")).toBeInTheDocument();
    expect(screen.getByText("Read src/login.rs")).toBeInTheDocument();
    expect(screen.queryByText(/cargo test/)).toBeNull();
  });

  it("says so when the agent is gone", async () => {
    const h = makeEnv({ "task.subagents": [] });
    h.load([task("t1")]);
    renderWith(h.env, <SubagentPane taskId="t1" id="a1" />);
    expect(await screen.findByText("This agent is no longer listed (the session ended).")).toBeInTheDocument();
  });
});
