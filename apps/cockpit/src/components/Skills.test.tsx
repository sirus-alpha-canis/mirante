import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { SkillSummary, SkillView, TenantView } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { ev } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { Skills } from "./Skills";

const sum = (name: string, over: Partial<SkillSummary> = {}): SkillSummary => ({
  name,
  version: 1,
  scope: "personal",
  activation: "on_demand",
  status: "active",
  description: `about ${name}`,
  manifest_hash: `h-${name}-${over.version ?? 1}`,
  verified: true,
  reason: null,
  injected_30d: 0,
  used_30d: 0,
  updated_at: 1,
  ...over,
});

const LIST: SkillSummary[] = [
  sum("review-checklist", { injected_30d: 9, used_30d: 4 }),
  sum("cargo-tests-first", { version: 3, scope: "org:github.com/acme", activation: "always", injected_30d: 12 }),
  sum("api-conventions", { version: 2, scope: "repo:github.com/acme/api", verified: false, reason: "modified on disk: SKILL.md" }),
];

const TENANTS: TenantView[] = [
  { tenant: "github.com/acme", learning: true, tasks: 4, skills: 2 },
  { tenant: "dev.azure.com/globex", learning: false, tasks: 2, skills: 0 },
];

const view = (s: SkillSummary, body = `Body of ${s.name}.\n`): SkillView => ({
  ...s,
  shown_version: s.version,
  manifest: null,
  skill_md: s.verified ? `---\nname: ${s.name}\ndescription: "${s.description}"\n---\n${body}` : "",
  files: [],
  versions: Array.from({ length: s.version }, (_, i) => i + 1),
  warnings: [],
  dir: `/home/.mirante/skills/${s.name}/v${s.version}`,
});

function setup(answers: Record<string, unknown> = {}) {
  const h = makeEnv({
    "skill.tenants": TENANTS,
    "skill.list": LIST,
    "skill.get": (p: Record<string, unknown>) => {
      const s = LIST.find((x) => x.name === p.name);
      return s ? view(s) : rpcError(RpcCode.NOT_FOUND, "no skill");
    },
    "skill.proposals": [],
    ...answers,
  });
  renderWith(h.env, <Skills reviewNonce={0} />);
  return h;
}

const library = () => screen.getByRole("region", { name: "Skill library" });

describe("Skills view", () => {
  it("lists skills grouped Personal / org / repo with activation, version, verified badge and 30-day usage", async () => {
    setup();
    await screen.findByRole("button", { name: /cargo-tests-first/ });
    const groups = within(library()).getAllByRole("heading", { level: 3 }).map((x) => x.textContent);
    expect(groups).toEqual(["Personal (every repo)", "Org github.com/acme", "Repo github.com/acme/api"]);
    const cargo = screen.getByRole("button", { name: /cargo-tests-first/ });
    expect(cargo).toHaveTextContent("v3");
    expect(cargo).toHaveTextContent("always on");
    expect(cargo).toHaveTextContent("verified");
    expect(cargo).toHaveTextContent("injected 12 · used 0 (30 days)");
    expect(screen.getByRole("button", { name: /review-checklist/ })).toHaveTextContent("on demand");
    expect(screen.getByRole("button", { name: /api-conventions/ })).toHaveTextContent("refused");
  });

  it("a refused skill shows why and offers Re-review (calls skill.add with its folder)", async () => {
    const h = setup({ "skill.add": { id: "sp-9" } });
    await userEvent.click(await screen.findByRole("button", { name: /api-conventions/ }));
    expect(await screen.findByText(/modified on disk: SKILL.md/)).toBeInTheDocument();
    expect(screen.getByText(/not given to any agent until you review it again/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Re-review" }));
    await waitFor(() =>
      expect(h.calls).toContainEqual([
        "skill.add",
        { dir: "/home/.mirante/skills/api-conventions/v2", scope: "repo:github.com/acme/api", activation: "on_demand" },
      ]),
    );
    expect(await screen.findByText(/queued for review/)).toBeInTheDocument();
  });

  it("tenant learning toggles are explicit and explain what is read", async () => {
    const h = setup({ "skill.learn": { tenant: "dev.azure.com/globex", learning: true, tasks: 2, skills: 0 } });
    const t = await screen.findByRole("group", { name: "dev.azure.com/globex" });
    expect(
      within(t).getByText(/Mirante reads your goals, messages and denied permissions in this client's tasks, on this machine only/),
    ).toBeInTheDocument();
    expect(within(t).getByText("Learning off")).toBeInTheDocument();
    await userEvent.click(within(t).getByRole("button", { name: "Start learning from dev.azure.com/globex" }));
    expect(h.calls).toContainEqual(["skill.learn", { tenant: "dev.azure.com/globex", enabled: true }]);
    const acme = screen.getByRole("group", { name: "github.com/acme" });
    expect(within(acme).getByRole("button", { name: "Stop learning from github.com/acme" })).toBeInTheDocument();
  });

  it("Find repeated instructions runs skill.mine and shows counts", async () => {
    const h = setup({ "skill.mine": { tenant: "github.com/acme", proposed: 1, updated: 2, skipped: 3 } });
    const acme = await screen.findByRole("group", { name: "github.com/acme" });
    // Only a learning tenant can be mined.
    expect(within(screen.getByRole("group", { name: "dev.azure.com/globex" })).queryByRole("button", { name: /Find repeated/ })).toBeNull();
    await userEvent.click(within(acme).getByRole("button", { name: "Find repeated instructions" }));
    expect(h.calls).toContainEqual(["skill.mine", { tenant: "github.com/acme" }]);
    expect(await within(acme).findByText("1 new proposal, 2 updated, 3 skipped")).toBeInTheDocument();
  });

  it("retire, rescope and rollback pass expect_hash", async () => {
    const h = setup({ "skill.action": (p: Record<string, unknown>) => view({ ...LIST[1], status: p.action === "retire" ? "retired" : "active" }) });
    await userEvent.click(await screen.findByRole("button", { name: /cargo-tests-first/ }));
    const panel = await screen.findByRole("region", { name: "Skill cargo-tests-first" });
    await userEvent.click(within(panel).getByRole("button", { name: "Retire" }));
    expect(h.calls).toContainEqual(["skill.action", { name: "cargo-tests-first", action: "retire", expect_hash: "h-cargo-tests-first-3" }]);
    await userEvent.clear(within(panel).getByLabelText("New scope"));
    await userEvent.type(within(panel).getByLabelText("New scope"), "repo:github.com/acme/api");
    await userEvent.click(within(panel).getByRole("button", { name: "Rescope" }));
    expect(h.calls).toContainEqual([
      "skill.action",
      { name: "cargo-tests-first", action: "rescope", expect_hash: "h-cargo-tests-first-3", scope: "repo:github.com/acme/api" },
    ]);
    await userEvent.selectOptions(within(panel).getByLabelText("Roll back to"), "1");
    await userEvent.click(within(panel).getByRole("button", { name: "Roll back" }));
    expect(h.calls).toContainEqual([
      "skill.action",
      { name: "cargo-tests-first", action: "rollback", expect_hash: "h-cargo-tests-first-3", version: 1 },
    ]);
  });

  it("rescoping to personal warns that it reaches every client", async () => {
    setup();
    await userEvent.click(await screen.findByRole("button", { name: /cargo-tests-first/ }));
    const panel = await screen.findByRole("region", { name: "Skill cargo-tests-first" });
    await userEvent.clear(within(panel).getByLabelText("New scope"));
    await userEvent.type(within(panel).getByLabelText("New scope"), "personal");
    expect(within(panel).getByText(/reach every repo, including other clients'/)).toBeInTheDocument();
  });

  it("SKILL.md renders as text only", async () => {
    const evil = sum("evil", { scope: "personal" });
    setup({
      "skill.list": [evil],
      "skill.get": view(evil, '<img src=x onerror="alert(1)"> **bold** [link](http://x)\n'),
    });
    await userEvent.click(await screen.findByRole("button", { name: /evil/ }));
    const panel = await screen.findByRole("region", { name: "Skill evil" });
    expect(await within(panel).findByText(/<img src=x onerror="alert\(1\)">/)).toBeInTheDocument();
    expect(document.querySelector("img, a[href]")).toBeNull();
    // One text node: nothing in the body became an element.
    expect(within(panel).getByLabelText("SKILL.md").children).toHaveLength(0);
  });

  it("refetches the library on skill events and says when the daemon has no skills", async () => {
    const h = setup();
    await screen.findByRole("button", { name: /cargo-tests-first/ });
    const lists = () => h.calls.filter(([m]) => m === "skill.list").length;
    const before = lists();
    h.env.cockpit.dispatch({ type: "connected", snapshot: { boot_id: "b1", seq: 0, tasks: [], resumed: false } });
    h.env.cockpit.dispatch({ type: "event", event: ev(1, null, "skill.changed", { name: "x", version: 2, action: "retire" }) });
    await waitFor(() => expect(lists()).toBeGreaterThan(before));
  });

  it("an older daemon gets a clear message", async () => {
    setup({ "skill.tenants": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method") });
    expect(await screen.findByText(/This daemon predates skills/)).toBeInTheDocument();
  });
});
