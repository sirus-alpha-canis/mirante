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

const ORIGINS = { live: "github.com/evil/app", live_tenant: "github.com/evil", pinned: "github.com/acme/app", pinned_tenant: "github.com/acme" };

function setup(answer: unknown, events = [] as ReturnType<typeof ev>[]) {
  const h = makeEnv({
    "skill.task_skills": answer,
    "skill.confirm_origin": { origin: "github.com/acme/app", tenant: "github.com/acme", cleared: 1 },
    "skill.repo_origin": ORIGINS,
  });
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

  it("the popover lists name, version, activation, used/injected, and the client with its learning state", async () => {
    setup({ ...TS, refused: [{ name: "x", reason: "over budget" }] }, [
      ev(1, "a", "skill.injected", { session_id: "s1", names: ["cargo-tests-first", "review-checklist"] }),
      ev(2, "a", "skill.used", { session_id: "s1", name: "review-checklist", version: 1 }),
    ]);
    await userEvent.click(await screen.findByRole("button", { name: /Skills 3/ }));
    const pop = screen.getByRole("dialog", { name: "Skills of this task" });
    expect(pop).toHaveTextContent("Client: github.com/acme · learning on");
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
    expect(screen.queryByRole("dialog", { name: "Skills of this task" })).toBeNull();
  });

  it("is a dialog: focus goes in on open, Escape anywhere closes it and focus goes back to the chip", async () => {
    setup(TS);
    const chip = await screen.findByRole("button", { name: "Skills 3" });
    await userEvent.click(chip);
    const pop = screen.getByRole("dialog", { name: "Skills of this task" });
    expect(pop.contains(document.activeElement)).toBe(true);
    const esc = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    act(() => {
      document.body.dispatchEvent(esc);
    });
    expect(esc.defaultPrevented).toBe(true);
    expect(screen.queryByRole("dialog", { name: "Skills of this task" })).toBeNull();
    expect(document.activeElement).toBe(chip);
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
    const pop = screen.getByRole("dialog", { name: "Skills of this task" });
    expect(pop).toHaveTextContent("github.com/evil/app");
    expect(within(pop).getByRole("button", { name: "Use github.com/evil/app for new tasks…" })).toBeInTheDocument();
    await userEvent.click(within(pop).getByRole("button", { name: "Keep github.com/acme/app" }));
    expect(h.calls).toContainEqual(["skill.confirm_origin", { task: "a", origin: "github.com/acme/app" }]);
    expect(await within(pop).findByText(/Confirmed github.com\/acme\/app: 1 task gets skills again/)).toBeInTheDocument();
  });

  it("moving the checkout to the live origin is two steps that say which client new tasks move to", async () => {
    const h = setup({ ...TS, confirmed: false, active: [], observed: "github.com/evil/app" });
    await userEvent.click(await screen.findByRole("button", { name: "Skills off: confirm origin" }));
    const pop = screen.getByRole("dialog", { name: "Skills of this task" });
    // Two rows: the pinned origin and the remote now, each with its client.
    const rows = within(await within(pop).findByRole("list", { name: "Origins of this checkout" }))
      .getAllByRole("listitem")
      .map((li) => li.textContent);
    expect(rows).toEqual(["Pinned: github.com/acme/app (client github.com/acme)", "Remote now: github.com/evil/app (client github.com/evil)"]);
    expect(h.calls).toContainEqual(["skill.repo_origin", { repo: "/repos/shop" }]);
    await userEvent.click(within(pop).getByRole("button", { name: "Use github.com/evil/app for new tasks…" }));
    expect(h.calls.some(([m]) => m === "skill.confirm_origin")).toBe(false);
    expect(pop).toHaveTextContent(
      "This moves new tasks of this checkout from client github.com/acme to github.com/evil: they get github.com/evil's skills and learning. This task stays on github.com/acme, without skills.",
    );
    await userEvent.click(within(pop).getByRole("button", { name: "Move new tasks to github.com/evil" }));
    expect(h.calls).toContainEqual(["skill.confirm_origin", { task: "a", origin: "github.com/evil/app" }]);
  });

  it("what a confirmation said stays after the chip refetches", async () => {
    let confirmed = false;
    const h = makeEnv({
      "skill.task_skills": () => ({ ...TS, confirmed, active: confirmed ? TS.active : [], observed: "github.com/evil/app" }),
      "skill.confirm_origin": () => {
        confirmed = true;
        return { origin: "github.com/acme/app", tenant: "github.com/acme", cleared: 1, warning: "remote.origin.url still points to github.com/evil/app" };
      },
      "skill.repo_origin": ORIGINS,
    });
    h.load([task("a")]);
    renderWith(h.env, <SkillsChip task="a" />);
    await userEvent.click(await screen.findByRole("button", { name: "Skills off: confirm origin" }));
    await userEvent.click(screen.getByRole("button", { name: "Keep github.com/acme/app" }));
    expect(await screen.findByRole("button", { name: "Skills 3" })).toBeInTheDocument();
    expect(screen.getByText(/1 task gets skills again/)).toBeInTheDocument();
    expect(screen.getByText(/remote.origin.url still points to github.com\/evil\/app/)).toBeInTheDocument();
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
