import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { TaskView } from "../lib/protocol";

vi.mock("./TerminalTab", () => ({
  TerminalTab: ({ t }: { t: TaskView }) => <div aria-label={`Terminal of ${t.branch}`}>terminal here</div>,
}));

import { RpcCode } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { Detail } from "./Detail";

function setup(answers = {}, tasks = [task("a", { status: "working" }), task("b", { status: "needs_you" })], events = [ev(1, "a", "step", { type: "command", command: "npm test", exit_code: 1 })]) {
  const h = makeEnv(answers);
  h.load(tasks, events);
  const onSelect = vi.fn();
  renderWith(h.env, <Detail taskId="a" onSelect={onSelect} />);
  return { ...h, onSelect };
}

describe("Detail (V1 classic)", () => {
  it("lists tasks grouped by state and selects one", async () => {
    const h = setup();
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    expect(within(nav).getAllByRole("heading").map((x) => x.textContent)).toEqual(["Needs you", "Working"]);
    await userEvent.click(within(nav).getByRole("button", { name: /goal of b/ }));
    expect(h.onSelect).toHaveBeenCalledWith("b");
  });

  it("shows steps, switches tabs with arrows, and has a memory placeholder", async () => {
    setup();
    expect(screen.getByText("$ npm test")).toBeInTheDocument();
    screen.getByRole("tab", { name: "Steps" }).focus();
    await userEvent.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("terminal here")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("tab", { name: "Memory" }));
    expect(screen.getByRole("tabpanel")).toHaveTextContent("Repo memory arrives with the memory layer");
  });

  it("Steer now and Queue send with their mode", async () => {
    const h = setup({ "message.send": (p: Record<string, unknown>) => ({ delivered: p.mode, queued: p.mode === "queue" ? 1 : 0 }) });
    const box = screen.getByLabelText("Message to the agent");
    await userEvent.type(box, "use the v2 API");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    expect(h.calls).toContainEqual(["message.send", { task: "a", text: "use the v2 API", mode: "steer" }]);
    expect(await screen.findByText("sent now")).toBeInTheDocument();
    await userEvent.type(box, "then update docs");
    await userEvent.click(screen.getByRole("button", { name: "Queue" }));
    expect(await screen.findByText("queued (#1, delivered when the turn ends)")).toBeInTheDocument();
  });

  it("an agent that cannot be steered offers interrupt-and-send", async () => {
    const h = setup({ "message.send": (p: Record<string, unknown>) => (p.mode === "steer" ? rpcError(RpcCode.UNSUPPORTED, "cannot steer") : { delivered: "queue", queued: 1 }), "session.interrupt": {} });
    await userEvent.type(screen.getByLabelText("Message to the agent"), "stop");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    await userEvent.click(await screen.findByRole("button", { name: "Interrupt and send" }));
    await waitFor(() => expect(h.calls.map((c) => c[0])).toEqual(["message.send", "session.interrupt", "message.send"]));
    expect(h.calls[2][1]).toMatchObject({ mode: "queue", text: "stop" });
  });

  it("a stopped task offers Resume instead of the composer", async () => {
    const h = setup({ "session.resume": {} }, [task("a", { live: false, boot: null, status: "ended" })], []);
    expect(screen.getByText(/No live session/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Resume" }));
    expect(h.calls).toContainEqual(["session.resume", { task: "a" }]);
  });

  it("a Claude permission with inline answers off is answered in the Terminal tab", async () => {
    setup({}, [task("a", { status: "needs_you" })], [ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" })]);
    expect(screen.queryByRole("button", { name: "Allow once" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Answer in terminal" }));
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByLabelText("Terminal of feat/a")).toBeInTheDocument();
  });

  it("shows the task's queue above the composer", () => {
    const h = makeEnv();
    h.load([task("a", { status: "working", queued: 1 })]);
    h.env.cockpit.dispatch({ type: "queue", taskId: "a", list: [{ id: "q1", text: "then update docs" }] });
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
    const queue = screen.getByRole("region", { name: "Queued messages" });
    expect(within(queue).getByText("then update docs")).toBeInTheDocument();
  });

  it("opens on the requested tab", () => {
    const h = makeEnv();
    h.load([task("a", { status: "working" })]);
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} initialTab="Terminal" />);
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByLabelText("Terminal of feat/a")).toBeInTheDocument();
  });
});
