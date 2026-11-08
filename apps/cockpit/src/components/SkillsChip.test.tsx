import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { TaskSkills } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { SkillsChip, SkillsCount } from "./SkillsChip";

const TS: TaskSkills = {
  tenant: "github.com/acme",
  origin: "github.com/acme/app",
  learning: true,
  confirmed: true,
  active: [
    { name: "cargo-tests-first", version: 3, activation: "always", scope: "org:github.com/acme" },
    { name: "review-checklist", version: 1, activation: "on_demand", scope: "personal" },
    { name: "api-style", version: 2, activation: "on_demand", scope: "repo:github.com/acme/app" },
  ],
  refused: [],
};

function setup(answer: unknown, events = [] as ReturnType<typeof ev>[]) {
  const h = makeEnv({ "skill.task_skills": answer, "skill.confirm_origin": { origin: "github.com/acme/app", tenant: "github.com/acme", cleared: 1 } });
  h.load([task("a")], events);
  renderWith(h.env, <SkillsChip task="a" />);
  return h;
}

describe("SkillsChip", () => {
  it('reads "Skills 3", or "Skills 3 · 1 refused" in the warning colour', async () => {
    setup(TS);
    expect(await screen.findByRole("button", { name: "Skills 3" })).not.toHaveClass("skill-chip-warn");
  });

  it("a refused skill is counted and coloured", async () => {
    setup({ ...TS, refused: [{ name: "x", reason: "modified on disk: SKILL.md" }] });
    expect(await screen.findByRole("button", { name: "Skills 3 · 1 refused" })).toHaveClass("skill-chip-warn");
  });

  it("the popover lists name, version, activation, used/injected, and the tenant with its learning state", async () => {
    setup({ ...TS, refused: [{ name: "x", reason: "over budget" }] }, [
      ev(1, "a", "skill.injected", { session_id: "s1", names: ["cargo-tests-first", "review-checklist"] }),
      ev(2, "a", "skill.used", { session_id: "s1", name: "review-checklist", version: 1 }),
    ]);
    await userEvent.click(await screen.findByRole("button", { name: /Skills 3/ }));
    const pop = screen.getByRole("group", { name: "Skills of this task" });
    expect(pop).toHaveTextContent("Tenant: github.com/acme · learning on");
    const rows = within(within(pop).getByRole("list", { name: "Active skills" })).getAllByRole("listitem").map((li) => li.textContent);
    expect(rows).toEqual([
      "cargo-tests-first v3 · always on · injected",
      "review-checklist v1 · on demand · injected, used",
      "api-style v2 · on demand · next session",
    ]);
    expect(within(pop).getByRole("list", { name: "Refused skills" })).toHaveTextContent("x: over budget");
    // Escape closes it (and is not the app's "leave the detail").
    const esc = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => {
      pop.dispatchEvent(esc);
    });
    expect(esc.defaultPrevented).toBe(true);
    expect(screen.queryByRole("group", { name: "Skills of this task" })).toBeNull();
  });

  it("refetches on skill.injected", async () => {
    const h = setup(TS);
    await screen.findByRole("button", { name: "Skills 3" });
    const n = h.calls.filter(([m]) => m === "skill.task_skills").length;
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(5, "a", "skill.injected", { session_id: "s2", names: [] }) }));
    await waitFor(() => expect(h.calls.filter(([m]) => m === "skill.task_skills").length).toBe(n + 1));
  });

  it("an unconfirmed origin says skills are off and offers to confirm either origin", async () => {
    const h = setup({
      ...TS,
      confirmed: false,
      active: [],
      observed: "github.com/evil/app",
      unavailable: "the checkout's origin is github.com/evil/app, not the confirmed github.com/acme/app: confirm it in the cockpit before skills apply",
    });
    const chip = await screen.findByRole("button", { name: "Skills off: confirm origin" });
    expect(chip).toHaveClass("skill-chip-warn");
    await userEvent.click(chip);
    const pop = screen.getByRole("group", { name: "Skills of this task" });
    expect(pop).toHaveTextContent("github.com/evil/app");
    expect(within(pop).getByRole("button", { name: "Use github.com/evil/app for new tasks" })).toBeInTheDocument();
    await userEvent.click(within(pop).getByRole("button", { name: "Keep github.com/acme/app" }));
    expect(h.calls).toContainEqual(["skill.confirm_origin", { task: "a", origin: "github.com/acme/app" }]);
    expect(await within(pop).findByText(/Confirmed github.com\/acme\/app: 1 task gets skills again/)).toBeInTheDocument();
  });

  it("shows the confirm warning the daemon returns", async () => {
    const h = makeEnv({
      "skill.task_skills": { ...TS, confirmed: false, active: [], observed: "github.com/evil/app" },
      "skill.confirm_origin": { origin: "github.com/acme/app", tenant: "github.com/acme", cleared: 1, warning: "remote.origin.url still points to github.com/evil/app" },
    });
    h.load([task("a")]);
    renderWith(h.env, <SkillsChip task="a" />);
    fireEvent.click(await screen.findByRole("button", { name: "Skills off: confirm origin" }));
    await userEvent.click(screen.getByRole("button", { name: "Keep github.com/acme/app" }));
    expect(await screen.findByText(/remote.origin.url still points to github.com\/evil\/app/)).toBeInTheDocument();
  });

  it("an older daemon shows nothing", async () => {
    const h = setup(rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method"));
    await waitFor(() => expect(h.calls.length).toBeGreaterThan(0));
    expect(screen.queryByRole("button", { name: /Skills/ })).toBeNull();
  });
});

describe("SkillsCount (agent card)", () => {
  it("counts what the session was given from its events, without a call", () => {
    const h = makeEnv();
    h.load(
      [task("a")],
      [
        ev(1, "a", "skill.injected", { session_id: "s1", names: ["x", "y"] }),
        ev(2, "a", "skill.refused", { session_id: "s1", name: "z", reason: "over budget" }),
      ],
    );
    renderWith(h.env, <SkillsCount task="a" />);
    expect(screen.getByText("Skills 2 · 1 refused")).toHaveClass("skill-chip-warn");
    expect(h.calls).toEqual([]);
  });

  it("is absent before any session got skills", () => {
    const h = makeEnv();
    h.load([task("a")]);
    const { container } = renderWith(h.env, <SkillsCount task="a" />);
    expect(container).toBeEmptyDOMElement();
  });
});
