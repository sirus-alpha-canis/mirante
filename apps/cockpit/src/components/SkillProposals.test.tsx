import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { Finding, SkillProposalView, SkillView, TaskSkills } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { SkillProposals } from "./SkillProposals";

const prop = (id: string, over: Partial<SkillProposalView> = {}): SkillProposalView => ({
  id,
  origin: "miner",
  task_id: null,
  tenant: "github.com/acme",
  kind: "new",
  name: `skill-${id}`,
  base_hash: null,
  scope: "org:github.com/acme",
  activation: "always",
  description: `Description of ${id}`,
  body: `Body of ${id}.\n`,
  files: [],
  evidence: [],
  fingerprint: null,
  lint: [],
  redactions: 0,
  status: "pending",
  version: null,
  decision_note: "",
  created_at: 1,
  decided_at: null,
  warnings: [],
  ...over,
});

const CURRENT: SkillView = {
  name: "cargo-tests-first",
  version: 3,
  scope: "org:github.com/acme",
  activation: "always",
  status: "active",
  description: "Run tests",
  manifest_hash: "h3",
  verified: true,
  reason: null,
  injected_30d: 0,
  used_30d: 0,
  updated_at: 1,
  shown_version: 3,
  manifest: null,
  skill_md: '---\nname: cargo-tests-first\ndescription: "Run tests"\n---\nRun cargo test.\nRun clippy.\n',
  files: [],
  versions: [1, 2, 3],
  warnings: [],
};

const lintError: Finding = { rule: "forbidden", severity: "error", file: "SKILL.md", line: 1, message: "grants permissions" };

function setup(answers: Record<string, unknown>, tasks = [task("t1", { branch: "feat/a" })]) {
  const h = makeEnv(answers);
  h.load(tasks);
  renderWith(h.env, <SkillProposals />);
  return h;
}

const card = (name: string) => screen.findByRole("article", { name: `Proposal ${name}` });

describe("SkillProposals", () => {
  it("miner card shows N tasks and up to 5 redacted examples", async () => {
    const evidence = ["t1", "t2", "t3", "t3", "t4", "t5"].map((t, i) => ({ task: t, text: `example ${i} TOKEN=«redacted»` }));
    setup({ "skill.proposals": [prop("a", { evidence })] });
    const c = await card("skill-a");
    expect(c).toHaveTextContent("Found in your messages: repeated in 5 tasks");
    const ex = within(c).getByRole("list", { name: "Examples" });
    expect(within(ex).getAllByRole("listitem")).toHaveLength(5);
    expect(ex).toHaveTextContent("example 0 TOKEN=«redacted»");
    expect(ex).not.toHaveTextContent("example 5");
  });

  it("shows everything a person approves: the whole body, every bundled file, activation, scope and warnings", async () => {
    const long = "x".repeat(400);
    const body = [...Array.from({ length: 70 }, (_, i) => `line ${i + 1}`), long, "last line"].join("\n") + "\n";
    setup({
      "skill.proposals": [
        prop("a", {
          body,
          files: [{ path: "reference/notes.md", text: "Bundled first line\nBundled second line\n" }],
          warnings: ["learned in github.com/acme; personal skills reach every repo, including other clients'"],
        }),
      ],
    });
    const c = await card("skill-a");
    const shown = within(c).getByLabelText("Proposed SKILL.md body");
    expect(shown.textContent).toBe(body);
    // Long lines wrap (white-space: pre-wrap); nothing is cut.
    expect(shown.textContent).toContain(long);
    expect(shown).toHaveClass("skill-body");
    expect(within(c).getByLabelText("File reference/notes.md").textContent).toBe("Bundled first line\nBundled second line\n");
    expect(c).toHaveTextContent("Always on: added to the agent's system prompt");
    expect(c).toHaveTextContent("Org github.com/acme");
    expect(c).toHaveTextContent("Client github.com/acme");
    expect(within(c).getByRole("alert")).toHaveTextContent("personal skills reach every repo");
    // Long content can be collapsed to scan the page, but then nothing can be accepted.
    const accept = within(c).getByRole("button", { name: "Accept" });
    expect(accept).toBeEnabled();
    await userEvent.click(within(c).getByRole("button", { name: "Collapse" }));
    expect(within(c).queryByLabelText("Proposed SKILL.md body")).toBeNull();
    expect(within(c).queryByLabelText("File reference/notes.md")).toBeNull();
    expect(within(c).getByRole("button", { name: "Accept" })).toBeDisabled();
    expect(c).toHaveTextContent("Expand to read everything before accepting");
    await userEvent.click(within(c).getByRole("button", { name: "Expand" }));
    expect(within(c).getByLabelText("Proposed SKILL.md body").textContent).toBe(body);
    expect(within(c).getByRole("button", { name: "Accept" })).toBeEnabled();
  });

  it("edit proposal shows a line diff against the current version", async () => {
    const h = setup({
      "skill.proposals": [
        prop("e", { kind: "edit", origin: "agent", task_id: "t1", name: "cargo-tests-first", base_hash: "h3", body: "Run cargo test.\nRun cargo fmt.\n" }),
      ],
      "skill.get": CURRENT,
    });
    const c = await card("cargo-tests-first");
    expect(c).toHaveTextContent("from task feat/a");
    const diff = await within(c).findByRole("list", { name: "Changes from v3" });
    const rows = within(diff).getAllByRole("listitem").map((li) => li.textContent);
    expect(rows).toEqual(["  Run cargo test.", "- Run clippy.", "+ Run cargo fmt."]);
    expect(h.calls).toContainEqual(["skill.get", { name: "cargo-tests-first" }]);
  });

  it("lint errors disable Accept; editing re-lints (debounced skill.lint) and re-enables", async () => {
    const h = setup({
      "skill.proposals": [prop("l", { body: "Use --dangerously-skip-permissions.\n", lint: [lintError] })],
      "skill.lint": (p: Record<string, unknown>) => (String(p.body).includes("dangerously") ? [lintError] : []),
    });
    const c = await card("skill-l");
    expect(within(c).getByRole("button", { name: "Accept" })).toBeDisabled();
    expect(c).toHaveTextContent("forbidden (error, SKILL.md line 1): grants permissions");
    await userEvent.click(within(c).getByRole("button", { name: "Edit" }));
    const box = within(c).getByLabelText("Body");
    await userEvent.clear(box);
    await userEvent.type(box, "Ask before running anything.");
    await waitFor(() => expect(within(c).getByRole("button", { name: "Accept" })).toBeEnabled());
    const lints = h.calls.filter(([m]) => m === "skill.lint");
    expect(lints.length).toBeLessThan(5);
    expect(lints.at(-1)?.[1]).toEqual({ name: "skill-l", description: "Description of l", body: "Ask before running anything.", scope: "org:github.com/acme" });
  });

  it("choosing personal shows the cross-client warning", async () => {
    setup({ "skill.proposals": [prop("p")], "skill.lint": [] });
    const c = await card("skill-p");
    expect(within(c).queryByRole("alert")).toBeNull();
    await userEvent.selectOptions(within(c).getByLabelText("Scope"), "personal");
    expect(within(c).getByRole("alert")).toHaveTextContent("Learned in github.com/acme; personal skills reach every repo, including other clients'");
  });

  it("accept sends edits once; CONFLICT (moved base) reloads the diff; FORBIDDEN explains", async () => {
    let gets = 0;
    let reviews = 0;
    const h = setup({
      "skill.proposals": [prop("e", { kind: "edit", name: "cargo-tests-first", base_hash: "h3", body: "Run cargo test.\n" })],
      "skill.get": () => (++gets === 1 ? CURRENT : { ...CURRENT, version: 4, manifest_hash: "h4", skill_md: "---\nname: x\n---\nMoved on.\n" }),
      "skill.lint": [],
      "skill.review": () => (++reviews === 1 ? rpcError(RpcCode.CONFLICT, "moved on") : rpcError(RpcCode.FORBIDDEN, "no")),
    });
    const c = await card("cargo-tests-first");
    await within(c).findByRole("list", { name: "Changes from v3" });
    await userEvent.selectOptions(within(c).getByLabelText("Activation"), "on_demand");
    await userEvent.dblClick(within(c).getByRole("button", { name: "Accept" }));
    expect(h.calls.filter(([m]) => m === "skill.review")).toEqual([
      ["skill.review", { id: "e", accept: true, edits: { activation: "on_demand" } }],
    ]);
    expect(await within(c).findByRole("list", { name: "Changes from v4" })).toBeInTheDocument();
    expect(c).toHaveTextContent("The skill changed since this proposal");
    await userEvent.click(within(c).getByRole("button", { name: "Accept" }));
    expect(await within(c).findByText(/person's decision/)).toBeInTheDocument();
  });

  it("Refine with agent only offers tasks of the same tenant and queues an \"Operator request:\" message", async () => {
    const skills = (tenant: string, over: Partial<TaskSkills> = {}): TaskSkills => ({
      tenant,
      origin: `${tenant}/app`,
      learning: true,
      confirmed: true,
      active: [],
      refused: [],
      ...over,
    });
    const h = setup(
      {
        "skill.proposals": [prop("r")],
        "skill.task_skills": (p: Record<string, unknown>) =>
          ({ ta: skills("github.com/acme"), tb: skills("dev.azure.com/globex"), tc: skills("github.com/acme", { confirmed: false }) })[
            String(p.task)
          ],
        "message.send": { delivered: "queue", queued: 1 },
      },
      [task("ta", { branch: "feat/acme" }), task("tb", { branch: "feat/globex" }), task("tc", { branch: "feat/unconfirmed" })],
    );
    const c = await card("skill-r");
    await userEvent.click(within(c).getByRole("button", { name: "Refine with agent" }));
    const pick = await within(c).findByLabelText("Task to ask");
    const options = within(pick).getAllByRole("option").map((o) => o.textContent);
    expect(options).toEqual(["feat/acme (@claude)"]);
    await userEvent.click(within(c).getByRole("button", { name: "Queue the request" }));
    const sent = h.calls.filter(([m]) => m === "message.send");
    expect(sent).toHaveLength(1);
    expect(sent[0][1]).toMatchObject({ task: "ta", mode: "queue" });
    expect(String(sent[0][1].text)).toMatch(/^Operator request: /);
    expect(String(sent[0][1].text)).toContain("skill_propose");
    expect(String(sent[0][1].text)).toContain("Body of r.");
    // Nothing is accepted: the proposal stays pending.
    expect(h.calls.some(([m]) => m === "skill.review")).toBe(false);
  });

  it("reject sends the note", async () => {
    const h = setup({ "skill.proposals": [prop("j")], "skill.review": { ...prop("j"), status: "rejected" } });
    const c = await card("skill-j");
    await userEvent.click(within(c).getByRole("button", { name: "Reject" }));
    await userEvent.type(within(c).getByLabelText("Why (optional)"), "too vague");
    await userEvent.click(within(c).getByRole("button", { name: "Reject proposal" }));
    expect(h.calls).toContainEqual(["skill.review", { id: "j", accept: false, note: "too vague" }]);
    expect(await screen.findByText("Rejected.")).toBeInTheDocument();
  });
});
