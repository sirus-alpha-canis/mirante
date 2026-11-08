import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { REPOS_KEY } from "../lib/repos";
import { projects } from "../store/select";
import { makeEnv, renderWith } from "../test/env";
import { task } from "../test/fixtures";
import { LeftRail } from "./LeftRail";

function setup(tasks = [task("a", { status: "working" })], registry = ["/repos/shop"]) {
  const h = makeEnv({ "dna.status": { available: false } });
  h.load(tasks);
  h.env.storage.setItem(REPOS_KEY, JSON.stringify(registry));
  const props = { onSelect: vi.fn(), onReposChanged: vi.fn(), onMention: vi.fn(), onOpen: vi.fn() };
  renderWith(h.env, <LeftRail projects={projects(h.env.cockpit.getState(), registry)} selected="/repos/shop" status={null} {...props} />);
  return { ...h, ...props };
}

describe("LeftRail", () => {
  it("lists every agent the dispatch form offers, with its live sessions", () => {
    setup([task("a"), task("b", { agent: "codex", live: false })]);
    const agents = within(screen.getByRole("navigation", { name: "Projects and agents" }));
    expect(agents.getByRole("button", { name: /^@claude/ })).toHaveTextContent("1 live");
    expect(agents.getByRole("button", { name: /^@codex/ })).toHaveTextContent("0 live");
  });

  it("an agent with no live session goes to the dispatch form", async () => {
    const h = setup([]);
    await userEvent.click(screen.getByRole("button", { name: /^@codex/ }));
    expect(h.onMention).toHaveBeenCalledWith("codex");
  });

  it("an added project can be removed; a task's project cannot", async () => {
    const h = setup([task("x", { repo_path: "/repos/api" })], ["/repos/shop"]);
    expect(screen.queryByRole("button", { name: "Remove api from Projects" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Remove shop from Projects" }));
    expect(h.onReposChanged).toHaveBeenCalledWith([]);
  });

  it("a refused path says why", async () => {
    const h = setup();
    vi.mocked(h.env.pathKind).mockResolvedValue("missing");
    await userEvent.type(screen.getByLabelText("Repository path"), "/nope{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent("/nope does not exist");
    await act(async () => {});
  });
});
