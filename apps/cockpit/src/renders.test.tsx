// How much React work one daemon event costs: cards, tiles and the detail render only when
// what they show changed. Counted through StatusPill, which every card, tile and detail
// header renders once per render of its own.
import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

const count = vi.hoisted(() => ({ pills: 0, views: {} as Record<string, number> }));
/** Counts the renders of a component module's export (the real one renders). */
function counted<M extends Record<string, unknown>>(real: M, name: keyof M & string): M {
  const Real = real[name] as (p: object) => unknown;
  const Wrapped = (p: object) => {
    count.views[name] = (count.views[name] ?? 0) + 1;
    return Real(p);
  };
  return { ...real, [name]: Wrapped };
}
vi.mock("./components/LeftRail", async (actual) => counted(await actual<typeof import("./components/LeftRail")>(), "LeftRail"));
vi.mock("./components/DispatchComposer", async (actual) => counted(await actual<typeof import("./components/DispatchComposer")>(), "DispatchComposer"));
vi.mock("./components/MemoryPanel", async (actual) => counted(await actual<typeof import("./components/MemoryPanel")>(), "MemoryPanel"));
vi.mock("./components/TerminalTab", () => ({ TerminalTab: () => null }));
vi.mock("./components/Badges", async (actual) => {
  const real = await actual<typeof import("./components/Badges")>();
  return {
    ...real,
    StatusPill: (p: Parameters<typeof real.StatusPill>[0]) => {
      count.pills++;
      return real.StatusPill(p);
    },
  };
});

import { App } from "./App";
import { ev, task } from "./test/fixtures";
import { makeEnv } from "./test/env";

const TASKS = ["a", "b", "c", "d", "e"].map((id) => task(id, { status: "working" }));

function setup() {
  const h = makeEnv({
    "daemon.status": { pid: 1, boot_id: "b1", version: "x", live_sessions: 0 },
    "message.list_queued": [],
    "task.get": (p: Record<string, unknown>) => ({ ...h.env.cockpit.getState().tasks[p.task as string] }),
  });
  render(<App env={h.env} />);
  act(() => h.load(TASKS));
  return h;
}

/** Card renders (StatusPill renders) caused by `fn`, once React settled. */
async function rendersOf(fn: () => void): Promise<number> {
  await act(async () => {});
  count.pills = 0;
  await act(async () => fn());
  await act(async () => {});
  return count.pills;
}

let seq = 100;
const step = (id: string) => ev(++seq, id, "step", { type: "tool", tool: "Read", summary: "x.ts", ok: true });

describe("renders per daemon event", () => {
  beforeEach(() => {
    count.pills = 0;
  });

  it("Home: a step on one task renders that task's card only", async () => {
    const h = setup();
    expect(screen.getAllByRole("article")).toHaveLength(5);
    expect(await rendersOf(() => h.env.cockpit.dispatch({ type: "event", event: step("a") }))).toBe(1);
  });

  it("Home: an event about one task renders no rail, form or memory panel (no whole-state readers)", async () => {
    const h = setup();
    await act(async () => {});
    count.views = {};
    await act(async () => h.env.cockpit.dispatch({ type: "event", event: step("a") }));
    await act(async () => h.env.cockpit.dispatch({ type: "task", view: { ...h.env.cockpit.getState().tasks.a, updated_at: 99 } }));
    await act(async () => {});
    expect(count.views).toEqual({});
  });

  it("Home: a task.get that changed nothing renders nothing", async () => {
    const h = setup();
    const same = { ...h.env.cockpit.getState().tasks.b };
    expect(await rendersOf(() => h.env.cockpit.dispatch({ type: "task", view: same }))).toBe(0);
  });

  it("Home: an event, then the task.get refresh it triggers bringing the same view: one card render", async () => {
    const h = setup();
    const event = await rendersOf(() => h.env.cockpit.dispatch({ type: "event", event: step("d") }));
    const refresh = await rendersOf(() => h.env.cockpit.dispatch({ type: "task", view: { ...h.env.cockpit.getState().tasks.d } }));
    expect(event + refresh).toBe(1);
  });

  it("Grid: a step on one task renders that tile only", async () => {
    const h = setup();
    await userEvent.click(screen.getByRole("button", { name: /^Grid/ }));
    expect(within(screen.getByRole("main", { name: "Agent grid" })).getAllByRole("article")).toHaveLength(5);
    expect(await rendersOf(() => h.env.cockpit.dispatch({ type: "event", event: step("c") }))).toBe(1);
  });

  it("Detail: a step on another task renders nothing", async () => {
    const h = setup();
    await userEvent.click(screen.getByRole("button", { name: "goal of a" }));
    expect(screen.getByRole("main", { name: "Task goal of a" })).toBeInTheDocument();
    expect(await rendersOf(() => h.env.cockpit.dispatch({ type: "event", event: step("b") }))).toBe(0);
  });
});
