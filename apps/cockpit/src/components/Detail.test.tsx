import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactElement } from "react";
import { describe, expect, it, vi } from "vitest";
import type { TaskView } from "../lib/protocol";

const term = vi.hoisted(() => ({ mounts: 0, unmounts: 0 }));
vi.mock("./TerminalTab", async () => {
  const { useEffect } = await import("react");
  return {
    TerminalTab: ({ t, active }: { t: TaskView; active?: boolean }) => {
      useEffect(() => {
        term.mounts++;
        return () => {
          term.unmounts++;
        };
      }, []);
      return (
        <div aria-label={`Terminal of ${t.branch}`} data-active={String(active ?? true)}>
          terminal here
        </div>
      );
    },
  };
});

import { RpcCode, type TaskDiff } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { EnvContext, type Env } from "../env";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { Detail } from "./Detail";
import { FEED_CAP } from "../store/state";
import { onMac } from "../lib/shortcuts";

// The same element type `renderWith` renders, so a rerender updates instead of remounting.
const wrap = (env: Env, ui: ReactElement) => <EnvContext.Provider value={env}>{ui}</EnvContext.Provider>;

function setup(answers = {}, tasks = [task("a", { status: "working" }), task("b", { status: "needs_you" })], events = [ev(1, "a", "step", { type: "command", command: "npm test", exit_code: 1 })]) {
  const h = makeEnv(answers);
  h.load(tasks, events);
  const onSelect = vi.fn();
  renderWith(h.env, <Detail taskId="a" onSelect={onSelect} />);
  return { ...h, onSelect };
}

describe("Detail (V1 classic)", () => {
  it("a task across a folder's repos names them in the header", () => {
    const repos = ["Site-B2C", "bff-flight"].map((name) => ({ name, repo_path: `/w/gol/${name}`, worktree_path: `/m/_workspaces/x/${name}`, branch: "mirante/x" }));
    const h = makeEnv();
    h.load([task("a", { group_path: "/w/gol", workspace_path: "/m/_workspaces/x", repos })]);
    renderWith(h.env, <Detail taskId="a" onSelect={vi.fn()} />);
    const head = screen.getByLabelText("Repositories");
    expect(head).toHaveTextContent("gol: Site-B2C, bff-flight");
    expect(head).toHaveAttribute("title", "/m/_workspaces/x");
  });

  it("Rules this agent received opens the Rules view for this task", async () => {
    const h = makeEnv();
    h.load([task("a", { status: "working" })]);
    const onRules = vi.fn();
    renderWith(h.env, <Detail taskId="a" onSelect={vi.fn()} onRules={onRules} />);
    await userEvent.click(screen.getByRole("button", { name: "Rules this agent received" }));
    expect(onRules).toHaveBeenCalledWith("a");
    // One place for rules: nothing inline, nothing fetched here.
    expect(h.calls.filter((c) => c[0] === "rules.layers")).toEqual([]);
  });

  it("lists tasks by project, then by state, and selects one", async () => {
    const h = setup();
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    expect(within(nav).getAllByRole("heading").map((x) => x.textContent)).toEqual(["shop", "Needs you", "Working"]);
    await userEvent.click(within(nav).getByRole("button", { name: /goal of b/ }));
    expect(h.onSelect).toHaveBeenCalledWith("b");
  });

  it("the list filters by state with chips and by text with a search", async () => {
    setup({}, [
      task("a", { status: "working" }),
      task("b", { status: "needs_you", title: "fix the cart" }),
      task("c", { status: "working", repo_path: "/repos/api", title: "api docs" }),
    ]);
    const nav = screen.getByRole("navigation", { name: "Tasks" });
    const names = () => within(nav).queryAllByRole("button", { name: /@claude/ }).map((b) => b.querySelector(".chip-title")?.textContent);
    const chips = within(nav).getByRole("group", { name: "Filter tasks" });
    expect(within(chips).getAllByRole("button").map((b) => b.textContent)).toEqual([
      "Needs you (1)",
      "Working (2)",
      "Ready (0)",
      "All (3)",
    ]);
    expect(within(chips).getByRole("button", { name: "All (3)" })).toHaveAttribute("aria-pressed", "true");
    await userEvent.click(within(chips).getByRole("button", { name: "Working (2)" }));
    expect(names()).toEqual(["api docs", "goal of a"]);
    expect(within(nav).getAllByRole("region").map((r) => r.getAttribute("aria-label"))).toEqual(["api", "shop"]);
    await userEvent.click(within(chips).getByRole("button", { name: "All (3)" }));
    await userEvent.type(within(nav).getByRole("searchbox", { name: "Search tasks" }), "CART");
    expect(names()).toEqual(["fix the cart"]);
    await userEvent.type(within(nav).getByRole("searchbox", { name: "Search tasks" }), "zzz");
    expect(within(nav).getByText("No task matches.")).toBeInTheDocument();
  });

  it("Steps sums up runs of tool steps in plain words, expandable; repeated lines fold with ×N", async () => {
    const reads = Array.from({ length: 3 }, (_, i) => ev(10 + i, "a", "step", { type: "tool", tool: "Read", summary: `src/f${i}.ts`, ok: true }));
    const hib = (seq: number) => ev(seq, "a", "memory.offer_hibernate", { reason: "memory pressure elevated" });
    setup({}, [task("a", { status: "working" })], [
      ...reads,
      ev(20, "a", "step", { type: "command", command: "npm test", exit_code: 0 }),
      hib(21),
      hib(22),
      hib(23),
    ]);
    const panel = screen.getByRole("tabpanel");
    const run = within(panel).getByRole("button", { name: /read 3 files, ran 1 command/ });
    expect(run).toHaveAttribute("aria-expanded", "false");
    expect(within(panel).queryByText("$ npm test")).toBeNull();
    await userEvent.click(run);
    expect(within(panel).getByText("$ npm test")).toBeInTheDocument();
    expect(within(panel).getByLabelText("3 times")).toHaveTextContent("×3");
  });

  it("a history that failed to load says so on Steps, with Retry", async () => {
    let fail = true;
    const h = makeEnv({ "task.events": () => (fail ? rpcError(1, "store locked") : [ev(3, "a", "step", { type: "command", command: "ls", exit_code: 0 })]) });
    h.load([task("a")]);
    act(() => h.env.cockpit.dispatch({ type: "historyFailed", taskId: "a", error: "store locked" }));
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
    const panel = screen.getByRole("tabpanel");
    expect(within(panel).getByRole("alert")).toHaveTextContent("Could not load this task's history: store locked");
    expect(within(panel).queryByText("No steps yet.")).toBeNull();
    fail = false;
    await userEvent.click(within(panel).getByRole("button", { name: "Retry" }));
    expect(await within(panel).findByText("$ ls")).toBeInTheDocument();
    expect(within(panel).queryByRole("alert")).toBeNull();
  });

  it("an open run of steps stays open when the oldest lines fall off the feed", async () => {
    const read = (seq: number) => ev(seq, "a", "step", { type: "tool", tool: "Read", summary: `src/f${seq}.ts`, ok: true });
    const h = setup({}, [task("a", { status: "working" })], Array.from({ length: FEED_CAP }, (_, i) => read(i + 1)));
    const panel = screen.getByRole("tabpanel");
    await userEvent.click(within(panel).getByRole("button", { expanded: false }));
    expect(within(panel).getByText("Read src/f1.ts")).toBeInTheDocument();
    // One more line: the run's first one falls out of the store's window, the run goes on.
    act(() => h.env.cockpit.dispatch({ type: "event", event: read(FEED_CAP + 1) }));
    expect(within(panel).queryByText("Read src/f1.ts")).toBeNull();
    expect(within(panel).getByText(`Read src/f${FEED_CAP + 1}.ts`)).toBeInTheDocument();
  });

  it("Steps follows new lines while scrolled to the bottom, and stays put once scrolled up", async () => {
    const h = setup({}, [task("a", { status: "working" })], [ev(3, "a", "step", { type: "command", command: "ls", exit_code: 0 })]);
    const panel = screen.getByRole("tabpanel");
    let top = 0;
    Object.defineProperty(panel, "scrollHeight", { configurable: true, get: () => 1000 });
    Object.defineProperty(panel, "clientHeight", { configurable: true, get: () => 100 });
    Object.defineProperty(panel, "scrollTop", { configurable: true, get: () => top, set: (v: number) => void (top = v) });
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(4, "a", "step", { type: "command", command: "pwd", exit_code: 0 }) }));
    expect(top).toBe(1000);
    top = 200;
    fireEvent.scroll(panel);
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(5, "a", "step", { type: "command", command: "id", exit_code: 0 }) }));
    expect(top).toBe(200);
  });

  it("the task's title takes focus when the detail opens", () => {
    setup();
    expect(screen.getByRole("heading", { level: 1, name: "goal of a" })).toHaveFocus();
  });

  it("tells the app which tab it shows; before the first snapshot it says loading, not gone", async () => {
    const h = makeEnv();
    const onTab = vi.fn();
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} onTab={onTab} initialTab="Diff" />);
    expect(screen.getByText("Loading…")).toBeInTheDocument();
    expect(onTab).toHaveBeenLastCalledWith("Diff");
    act(() => h.load([task("b")]));
    expect(screen.getByText("This task is gone.")).toBeInTheDocument();
  });

  it("shows steps, switches tabs with arrows, and has a Memory tab", async () => {
    setup();
    expect(screen.getByText("$ npm test")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /ran \d+ command/ })).toBeNull();
    screen.getByRole("tab", { name: "Steps" }).focus();
    await userEvent.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("terminal here")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("tab", { name: "Memory" }));
    expect(screen.getByRole("tabpanel")).not.toHaveTextContent("arrives with the memory layer");
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
    const sent = () => h.calls.filter((c) => !c[0].startsWith("dna.") && !c[0].startsWith("skill.") && c[0] !== "task.subagents");
    await waitFor(() => expect(sent().map((c) => c[0])).toEqual(["message.send", "session.interrupt", "message.send"]));
    expect(sent()[2][1]).toMatchObject({ mode: "queue", text: "stop" });
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

  it("keeps one terminal per task across tab switches, hidden off its tab", async () => {
    const h = makeEnv();
    h.load([task("a", { status: "working" })]);
    term.mounts = term.unmounts = 0;
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} initialTab="Terminal" />);
    const view = screen.getByLabelText("Terminal of feat/a");
    expect(view.closest("[hidden]")).toBeNull();
    await userEvent.click(screen.getByRole("tab", { name: "Steps" }));
    expect(screen.getByLabelText("Terminal of feat/a")).toHaveAttribute("data-active", "false");
    expect(screen.getByLabelText("Terminal of feat/a").closest("[hidden]")).not.toBeNull();
    await userEvent.click(screen.getByRole("tab", { name: "Terminal" }));
    expect(screen.getByLabelText("Terminal of feat/a")).toBe(view);
    expect([term.mounts, term.unmounts]).toEqual([1, 0]);
  });

  it("on the Terminal tab, requests and the queue float over the terminal instead of resizing it", async () => {
    const h = makeEnv();
    h.load([task("a", { status: "needs_you", agent: "codex", queued: 1 })], [ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" })]);
    h.env.cockpit.dispatch({ type: "queue", taskId: "a", list: [{ id: "q1", text: "then update docs" }] });
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
    const card = () => screen.getByRole("button", { name: "Allow once" });
    const queue = () => screen.getByRole("region", { name: "Queued messages" });
    expect(card().closest(".term-overlay")).toBeNull();
    expect(queue().closest(".term-overlay")).toBeNull();
    await userEvent.click(screen.getByRole("tab", { name: "Terminal" }));
    expect(card().closest(".term-overlay")).not.toBeNull();
    expect(queue().closest(".term-overlay")).not.toBeNull();
  });

  it("opens on the requested tab", () => {
    const h = makeEnv();
    h.load([task("a", { status: "working" })]);
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} initialTab="Terminal" />);
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByLabelText("Terminal of feat/a")).toBeInTheDocument();
  });

  it("Home and End jump between tabs, arrows move focus with the selection, all tabs control one panel", async () => {
    setup();
    screen.getByRole("tab", { name: "Steps" }).focus();
    await userEvent.keyboard("{ArrowRight}");
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveFocus();
    await userEvent.keyboard("{End}");
    expect(screen.getByRole("tab", { name: "Memory" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tab", { name: "Memory" })).toHaveFocus();
    await userEvent.keyboard("{Home}");
    expect(screen.getByRole("tab", { name: "Steps" })).toHaveFocus();
    const panel = screen.getByRole("tabpanel");
    for (const tab of screen.getAllByRole("tab")) expect(tab).toHaveAttribute("aria-controls", panel.id);
  });

  it("a new focusTerminalNonce selects the Terminal tab again", async () => {
    const h = makeEnv();
    h.load([task("a", { status: "needs_you" })]);
    const r = renderWith(h.env, <Detail taskId="a" onSelect={() => {}} focusTerminalNonce={1} />);
    // Mounting with a nonce is not a request: initialTab says where to open.
    expect(screen.getByRole("tab", { name: "Steps" })).toHaveAttribute("aria-selected", "true");
    r.rerender(wrap(h.env, <Detail taskId="a" onSelect={() => {}} focusTerminalNonce={2} />));
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
    await userEvent.click(screen.getByRole("tab", { name: "Steps" }));
    r.rerender(wrap(h.env, <Detail taskId="a" onSelect={() => {}} focusTerminalNonce={3} />));
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
  });

  it("Stop asks first; Cancel stops nothing", async () => {
    const h = setup({ "session.stop": {} });
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(h.calls.map((c) => c[0])).not.toContain("session.stop");
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("button", { name: "Stop the session" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    await userEvent.click(screen.getByRole("button", { name: "Stop the session" }));
    expect(h.calls).toContainEqual(["session.stop", { task: "a" }]);
  });

  it("Archive asks first; a task without children is archived alone", async () => {
    const h = setup({ "task.archive": (p: Record<string, unknown>) => task(String(p.task), { lifecycle: "archived", live: false }) });
    await userEvent.click(screen.getByRole("button", { name: "Archive" }));
    const box = screen.getByRole("group", { name: "Confirm archive" });
    expect(box).toHaveTextContent("Archive this task? Its worktree is removed (a live session is stopped first); the branch is kept.");
    expect(h.calls.map((c) => c[0])).not.toContain("task.archive");
    await userEvent.click(within(box).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group", { name: "Confirm archive" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Archive" }));
    await userEvent.click(screen.getByRole("button", { name: "Archive the task" }));
    expect(h.calls).toContainEqual(["task.archive", { task: "a", stop: true }]);
  });

  it("archiving a parent lists its open threads and sub-tasks, then archives them all (or nothing on Cancel)", async () => {
    const h = makeEnv({ "task.archive": (p: Record<string, unknown>) => task(String(p.task), { lifecycle: "archived", live: false }) });
    h.load([
      task("a", { status: "working" }),
      task("t", { title: "review PR 12", branch: "feat/a--1-review", parent_id: "a", child_kind: "thread", live: true }),
      task("s", { title: "split the tests", branch: "feat/a--2-tests", parent_id: "a", child_kind: "spawn", live: false }),
      task("x", { title: "old one", parent_id: "a", child_kind: "thread", lifecycle: "archived", live: false }),
    ]);
    renderWith(h.env, <Detail taskId="a" onSelect={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Archive" }));
    const box = screen.getByRole("group", { name: "Confirm archive" });
    expect(box).toHaveTextContent("This task has 2 open threads or sub-tasks; they are archived with it, first (live sessions are stopped):");
    const items = within(within(box).getByRole("list", { name: "Its open threads and sub-tasks" })).getAllByRole("listitem");
    expect(items.map((i) => i.textContent)).toEqual(["Thread: review PR 12 (feat/a--1-review) · live", "Sub-task: split the tests (feat/a--2-tests)"]);
    await userEvent.click(within(box).getByRole("button", { name: "Cancel" }));
    expect(h.calls.map((c) => c[0])).not.toContain("task.archive");
    await userEvent.click(screen.getByRole("button", { name: "Archive" }));
    await userEvent.click(screen.getByRole("button", { name: "Archive all (3)" }));
    expect(h.calls.filter((c) => c[0] === "task.archive")).toEqual([["task.archive", { task: "a", cascade: true, stop: true }]]);
  });

  it("a cascade the daemon refuses (a dirty child) says why and archives nothing", async () => {
    const h = makeEnv({ "task.archive": rpcError(RpcCode.CONFLICT, "these tasks hold uncommitted work (nothing was archived; commit or discard it first): feat/a--1-review") });
    h.load([task("a", { status: "working" }), task("t", { branch: "feat/a--1-review", parent_id: "a", child_kind: "thread" })]);
    renderWith(h.env, <Detail taskId="a" onSelect={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Archive" }));
    await userEvent.click(screen.getByRole("button", { name: "Archive all (2)" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("nothing was archived; commit or discard it first): feat/a--1-review");
  });

  it("a session action runs once while in flight, and a later success clears the error", async () => {
    let n = 0;
    let release!: () => void;
    const h = setup({
      "session.interrupt": () => {
        n++;
        if (n === 1) return rpcError(RpcCode.DRIVER, "agent busy");
        return new Promise<object>((res) => (release = () => res({})));
      },
    });
    const btn = screen.getByRole("button", { name: "Interrupt" });
    await userEvent.click(btn);
    expect(await screen.findByRole("alert")).toHaveTextContent("agent busy");
    await userEvent.dblClick(btn);
    expect(h.calls.filter((c) => c[0] === "session.interrupt")).toHaveLength(2);
    expect(screen.queryByRole("alert")).toBeNull();
    await act(async () => release());
  });

  it("Steer now sends once on a double click", async () => {
    const h = setup({ "message.send": () => new Promise(() => {}) });
    await userEvent.type(screen.getByLabelText("Message to the agent"), "hi");
    await userEvent.dblClick(screen.getByRole("button", { name: "Steer now" }));
    fireEvent.keyDown(screen.getByLabelText("Message to the agent"), { key: "Enter", metaKey: true });
    expect(h.calls.filter((c) => c[0] === "message.send")).toHaveLength(1);
  });

  it("Enter that ends an IME composition sends nothing", () => {
    const h = setup({ "message.send": { delivered: "steer", queued: 0 } });
    const box = screen.getByLabelText("Message to the agent");
    fireEvent.change(box, { target: { value: "日本" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true, isComposing: true });
    expect(h.calls.filter((c) => c[0] === "message.send")).toHaveLength(0);
  });

  it("an unknown method (older daemon) also offers interrupt-and-send", async () => {
    setup({ "message.send": rpcError(RpcCode.METHOD_NOT_FOUND, "no such method") });
    await userEvent.type(screen.getByLabelText("Message to the agent"), "stop");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    expect(await screen.findByRole("button", { name: "Interrupt and send" })).toBeInTheDocument();
  });

  it("Interrupt and send runs once, and not at all once the text is emptied", async () => {
    let release!: () => void;
    const h = setup({
      "message.send": (p: Record<string, unknown>) => (p.mode === "steer" ? rpcError(RpcCode.UNSUPPORTED, "cannot steer") : { delivered: "queue", queued: 1 }),
      "session.interrupt": () => new Promise<object>((res) => (release = () => res({}))),
    });
    const box = screen.getByLabelText("Message to the agent");
    await userEvent.type(box, "stop");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    const fallback = await screen.findByRole("button", { name: "Interrupt and send" });
    await userEvent.dblClick(fallback);
    expect(h.calls.filter((c) => c[0] === "session.interrupt")).toHaveLength(1);
    await act(async () => release());
    await waitFor(() => expect(h.calls.at(-1)).toEqual(["message.send", { task: "a", text: "stop", mode: "queue" }]));

    // Again, but the box is emptied before choosing the fallback: nothing is interrupted.
    await userEvent.type(box, "again");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    const again = await screen.findByRole("button", { name: "Interrupt and send" });
    await userEvent.clear(box);
    await userEvent.click(again);
    expect(h.calls.filter((c) => c[0] === "session.interrupt")).toHaveLength(1);
  });

  it("switching task clears the draft", async () => {
    const h = makeEnv();
    h.load([task("a", { status: "working" }), task("b", { status: "working" })]);
    const r = renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
    await userEvent.type(screen.getByLabelText("Message to the agent"), "for a only");
    r.rerender(wrap(h.env, <Detail taskId="b" onSelect={() => {}} />));
    expect(screen.getByLabelText("Message to the agent")).toHaveValue("");
  });

  it("switching task shows the new task's diff loading, never the old one's", async () => {
    const diff = (base: string): TaskDiff => ({ base, merge_base: "abcdef123456", files: [{ path: `${base}.ts`, added: 1, removed: 0 }], untracked: [], patch: "+x", truncated: false, compare_url: null, pushed: false });
    const h = makeEnv({ "task.diff": (p: Record<string, unknown>) => (p.task === "a" ? diff("main-a") : new Promise(() => {})) });
    h.load([task("a", { status: "working" }), task("b", { status: "working" })]);
    const r = renderWith(h.env, <Detail taskId="a" onSelect={() => {}} initialTab="Diff" />);
    expect(await screen.findByText("main-a.ts")).toBeInTheDocument();
    r.rerender(wrap(h.env, <Detail taskId="b" onSelect={() => {}} initialTab="Diff" />));
    expect(screen.queryByText("main-a.ts")).toBeNull();
    expect(screen.getByText("Reading the diff…")).toBeInTheDocument();
  });

  it("the Diff tab counts untracked files apart, with no +0 −0 for them", async () => {
    const h = makeEnv({ "task.diff": { base: "main", merge_base: "abcdef123456", files: [], untracked: [".serena/a", ".serena/b"], patch: "", truncated: false, compare_url: null, pushed: false } });
    h.load([task("a", { status: "working" })]);
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} initialTab="Diff" />);
    const summary = (await screen.findByText(/2 new untracked files/)).closest("p")!;
    expect(summary).toHaveTextContent("No changes against main (abcdef12) · 2 new untracked files");
    expect(summary).not.toHaveTextContent("+0");
  });

  it("Open PR that fails to open the browser says so", async () => {
    const h = makeEnv({ "task.diff": { base: "main", merge_base: "abcdef123456", files: [], untracked: [], patch: "", truncated: false, compare_url: "https://github.com/acme/shop/compare/main...feat/a", pushed: true } });
    h.load([task("a", { status: "working" })]);
    (h.env.openUrl as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error("denied"));
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} initialTab="Diff" />);
    await userEvent.click(await screen.findByRole("button", { name: "Open PR" }));
    expect(await screen.findByText(/could not open the browser/)).toBeInTheDocument();
  });

  it("a live task can be hibernated from the header; a hibernated one offers Wake, not Resume", async () => {
    const h = setup({ "session.hibernate": {}, "session.wake": {} }, [task("a", { status: "working" })], []);
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    expect(screen.getByRole("group", { name: "Confirm hibernate" })).toHaveTextContent("The agent is mid-turn; hibernating ends this turn.");
    await userEvent.click(screen.getByRole("button", { name: "Hibernate now" }));
    expect(h.calls).toContainEqual(["session.hibernate", { task: "a" }]);
    act(() => h.load([task("a", { live: false, boot: null, status: "ended", hibernated: true })]));
    expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Wake" }));
    expect(h.calls).toContainEqual(["session.wake", { task: "a" }]);
  });

  it("the header shows the task's skills chip", async () => {
    setup({
      "skill.task_skills": {
        tenant: "github.com/acme",
        origin: "github.com/acme/shop",
        learning: false,
        confirmed: true,
        active: [{ name: "x", version: 1, activation: "always", scope: "personal" }],
        refused: [],
      },
    });
    const head = screen.getByRole("main", { name: "Task goal of a" }).querySelector("header") as HTMLElement;
    expect(await within(head).findByRole("button", { name: "Skills 1" })).toBeInTheDocument();
  });

  describe("side pane", () => {
    const DIFF = { base: "trunk", merge_base: "abc1234", files: [], untracked: [], patch: "", truncated: false, compare_url: null, pushed: false };
    const mod = () => (onMac() ? { metaKey: true } : { ctrlKey: true });
    const togglePane = () => fireEvent.keyDown(window, { key: "\\", code: "Backslash", ...mod() });
    const pane = () => screen.queryByRole("complementary", { name: "Side pane" });

    it("⌘\\ toggles the pane", () => {
      setup();
      expect(pane()).toBeNull();
      act(() => togglePane());
      expect(pane()).not.toBeNull();
      expect(pane()).toHaveTextContent(/Open a file with/);
      act(() => togglePane());
      expect(pane()).toBeNull();
    });

    it("the separator is keyboard-resizable (←/→ by 2%) with aria-valuenow", async () => {
      setup();
      act(() => togglePane());
      const sep = screen.getByRole("separator", { name: "Resize the side pane" });
      expect(sep).toHaveAttribute("aria-valuenow", "40");
      sep.focus();
      await userEvent.keyboard("{ArrowLeft}");
      expect(sep).toHaveAttribute("aria-valuenow", "42");
      await userEvent.keyboard("{ArrowRight}{ArrowRight}");
      expect(sep).toHaveAttribute("aria-valuenow", "38");
      expect(sep).toHaveAttribute("aria-valuemin", "25");
      expect(sep).toHaveAttribute("aria-valuemax", "70");
    });

    it("remembers the pane per task", () => {
      const h = makeEnv({ "task.diff": DIFF });
      h.load([task("a", { status: "working" }), task("b", { status: "working" })]);
      const r = renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
      act(() => togglePane());
      r.rerender(wrap(h.env, <Detail taskId="b" onSelect={() => {}} />));
      expect(pane()).toBeNull();
      r.rerender(wrap(h.env, <Detail taskId="a" onSelect={() => {}} />));
      expect(pane()).not.toBeNull();
    });

    it("Review layout puts Terminal left and Diff right", async () => {
      setup({ "task.diff": DIFF });
      await userEvent.click(screen.getByRole("button", { name: "Review layout" }));
      expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute("aria-selected", "true");
      expect(within(pane()!).getByText("Diff", { selector: ".side-pane-title" })).toBeInTheDocument();
    });

    it("Claude's own agents show in their lane; one opens beside, with its latest text", async () => {
      const lane = { id: "a1", agent_type: "Explore", description: "Find the handlers", status: "running", started_ms: Date.now(), tools: 2, last_text: "Looking in src/", permissions: [], pending: 0 };
      const h = makeEnv({ "task.subagents": [lane] });
      h.load([task("a", { status: "working" })]);
      renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
      const region = await screen.findByRole("region", { name: "Claude's own agents (read-only)" });
      await userEvent.click(within(region).getByRole("button", { name: "Open beside" }));
      expect(within(pane()!).getByText("Agent a1", { selector: ".side-pane-title" })).toBeInTheDocument();
      expect(await within(pane()!).findByText("Looking in src/", { selector: ".subagent-text" })).toBeInTheDocument();
    });

    it("a Codex task asks for no Claude agents", () => {
      const h = makeEnv();
      h.load([task("a", { agent: "codex" })]);
      renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
      expect(h.calls.some(([m]) => m === "task.subagents")).toBe(false);
    });

    it("the same task's terminal is never opened in the pane (Terminal stays main)", () => {
      const h = makeEnv();
      h.load([task("a", { status: "working" })]);
      h.env.storage.setItem("mirante.pane.a", JSON.stringify({ open: true, width: 40, target: { k: "terminal", task: "a" } }));
      renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
      expect(pane()).toBeNull();
      expect(screen.getAllByLabelText("Terminal of feat/a")).toHaveLength(1);
    });

    it("⌘P quick-opens a file of the task into the pane; ⌘↵ opens it in the editor", async () => {
      const h = setup({
        "task.files": ["src/login.ts"],
        "task.file": { path: "src/login.ts", abs: "/w/src/login.ts", size: 9, kind: "text", text: "const a = 1;", lang: "ts" },
      });
      act(() => void fireEvent.keyDown(window, { key: "p", code: "KeyP", ...mod() }));
      const input = screen.getByRole("combobox", { name: "File name" });
      expect(await screen.findByRole("option", { name: "src/login.ts" })).toBeInTheDocument();
      fireEvent.keyDown(input, { key: "Enter" });
      expect(screen.queryByRole("dialog", { name: "Open a file of this task" })).toBeNull();
      expect(await within(pane()!).findByText("const")).toHaveClass("tk-kw");
      expect(h.calls).toContainEqual(["task.file", { task: "a", path: "src/login.ts" }]);
      act(() => void fireEvent.keyDown(window, { key: "p", code: "KeyP", ...mod() }));
      await screen.findByRole("option", { name: "src/login.ts" });
      fireEvent.keyDown(screen.getByRole("combobox", { name: "File name" }), { key: "Enter", ...mod() });
      await waitFor(() => expect(h.env.openInEditor).toHaveBeenCalledWith("a", "src/login.ts", null, "vscode"));
    });

    it("a Read/Edit step's path is a button that opens the file at the pane", async () => {
      const wt = "/Users/me/.mirante/worktrees/shop/feat-a";
      const h = makeEnv({ "task.file": { path: "src/a.ts", abs: `${wt}/src/a.ts`, size: 3, kind: "text", text: "let x", lang: "ts" } });
      h.load(
        [task("a", { status: "working", worktree_path: wt })],
        [
          ev(1, "a", "step", { type: "tool", tool: "Read", summary: `${wt}/src/a.ts`, ok: true }),
          ev(2, "a", "step", { type: "tool", tool: "Edit", summary: "/etc/hosts", ok: true }),
          ev(3, "a", "step", { type: "tool", tool: "Grep", summary: "src/a.ts", ok: true }),
        ],
      );
      renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
      await userEvent.click(screen.getByRole("button", { name: /read 1 file/i }));
      const buttons = screen.getAllByRole("button", { name: /in the side pane/ });
      expect(buttons.map((b) => b.getAttribute("aria-label"))).toEqual(["Open src/a.ts in the side pane"]);
      await userEvent.click(buttons[0]);
      expect(await within(pane()!).findByText("let")).toHaveClass("tk-kw");
    });

    it("the main terminal stays mounted and attached when the pane opens (one resize, no re-attach)", () => {
      const h = makeEnv();
      h.load([task("a", { status: "working" })]);
      term.mounts = term.unmounts = 0;
      renderWith(h.env, <Detail taskId="a" onSelect={() => {}} initialTab="Terminal" />);
      const view = screen.getByLabelText("Terminal of feat/a");
      act(() => togglePane());
      act(() => togglePane());
      act(() => togglePane());
      expect(screen.getByLabelText("Terminal of feat/a")).toBe(view);
      expect([term.mounts, term.unmounts]).toEqual([1, 0]);
    });
  });
});

describe("Detail: a fan-out group's member", () => {
  it("shows the group's panel above the tabs; Diff beside opens a sibling's Diff in the side pane", async () => {
    const g = { id: "g1", kind: "compare" as const, title: "fix login", goal: "fix login", branch: "mirante/fix-login", created_at: 1 };
    const a = task("a", { status: "working", group_id: "g1", group_role: "child", group: g, branch: "mirante/fix-login--1-claude" });
    const b = task("b", { status: "idle", group_id: "g1", group_role: "child", group: g, agent: "codex", branch: "mirante/fix-login--2-codex", title: "the codex run" });
    const h = makeEnv({
      "group.get": { group: g, members: [{ view: a }, { view: b }] },
      "task.diff": (p: Record<string, unknown>) => ({ base: `main-${String(p.task)}`, merge_base: "abcdef123456", files: [], untracked: [], patch: "", truncated: false, compare_url: null, pushed: false }),
    });
    h.load([a, b]);
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
    const panel = screen.getByRole("region", { name: "Fan-out fix login" });
    await userEvent.click(within(panel).getByRole("button", { name: "Diff beside" }));
    const pane = screen.getByRole("complementary", { name: "Side pane" });
    expect(within(pane).getByText("Diff: the codex run")).toBeInTheDocument();
    await waitFor(() => expect(h.calls).toContainEqual(["task.diff", { task: "b" }]));
  });
});


describe("Detail: threads", () => {
  const link = { kind: "pr" as const, ref: "acme/api#123", url: "https://github.com/acme/api/pull/123" };
  const events = [
    ev(1, "a", "message_sent", { mode: "steer", text: "look at the PR", by: "person" }),
    ev(2, "a", "step", { type: "agent_message", text: "thinking aloud", final: false }),
    ev(3, "a", "step", { type: "agent_message", text: "The PR needs a rebase.", final: true }),
    ev(4, "a", "step", { type: "command", command: "git status", exit_code: 0 }),
  ];

  it("each person message and final answer has \"New thread from this\"", () => {
    const h = makeEnv();
    h.load([task("a")], events);
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
    const buttons = screen.getAllByRole("button", { name: "New thread from this" });
    expect(buttons).toHaveLength(2);
    expect(buttons[0].closest("li")).toHaveTextContent("look at the PR");
    expect(buttons[1].closest("li")).toHaveTextContent("The PR needs a rebase.");
  });

  it("the parent feed shows \"continued in thread X\" (message_moved), one card per thread", () => {
    const th = task("th", { title: "rebase the PR", parent_id: "a", thread: { link, notes_hash: "h", delivered: true } });
    const h = makeEnv();
    h.load([task("a"), th], [...events, ev(5, "a", "message_moved", { thread: "th", title: "rebase the PR", link, mode: "copied", from_seq: 3 }), ev(6, "a", "thread_started", { thread: "th2", title: "other", link: { kind: "issue", ref: "acme/api#9" } })]);
    renderWith(h.env, <Detail taskId="a" onSelect={() => {}} />);
    const card = screen.getByRole("article", { name: "Thread: PR #123" });
    expect(card).toHaveTextContent("Continued in thread");
    expect(card).toHaveTextContent("rebase the PR");
    expect(screen.getByRole("article", { name: "Thread: Issue #9" })).toBeInTheDocument();
  });

  it("New thread from this creates the thread from the answer (copied), starts it, and opens it with its composer focused", async () => {
    const th = task("th", { title: "rebase the PR", parent_id: "a", thread: { link, notes_hash: "h", delivered: false } });
    const h = makeEnv({ "task.create": th, "session.start": { task: "th", boot: "ready", resumed: false, screen: "" } });
    h.load([task("a")], events);
    const onSelect = vi.fn();
    const r = renderWith(h.env, <Detail taskId="a" onSelect={onSelect} />);
    await userEvent.click(screen.getAllByRole("button", { name: "New thread from this" })[1]);
    const dialog = screen.getByRole("dialog", { name: "New thread" });
    expect(within(dialog).getByText(/This conversation keeps it/)).toBeInTheDocument();
    await userEvent.type(within(dialog).getByLabelText("What the thread works on"), "rebase the PR");
    await userEvent.type(within(dialog).getByLabelText("Bound to (optional)"), "https://github.com/acme/api/pull/123");
    expect(within(dialog).getByText("Bound to PR #123")).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole("button", { name: "Start thread" }));
    await waitFor(() => expect(onSelect).toHaveBeenCalledWith("th"));
    expect(h.calls.find(([m]) => m === "task.create")?.[1]).toMatchObject({ parent: "a", goal: "rebase the PR", link, moved: { mode: "copied", seq: 3 } });
    expect(h.calls).toContainEqual(["session.start", { task: "th", prompt: "rebase the PR" }]);
    expect(screen.queryByRole("dialog")).toBeNull();
    r.rerender(wrap(h.env, <Detail taskId="th" onSelect={onSelect} />));
    await waitFor(() => expect(document.activeElement).toBe(document.getElementById("msg-th")));
  });
});
