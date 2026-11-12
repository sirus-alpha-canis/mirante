import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { type DaemonStatus, RpcCode, type TaskDiff } from "../lib/protocol";
import { GROUPS_KEY, HIDDEN_KEY, loadRegistry, type Registry, REPOS_KEY } from "../lib/repos";
import { AGENT_KEY } from "./DispatchComposer";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";

/** Calls other than the rail's repo-memory and skills reads. */
const sent = (h: ReturnType<typeof makeEnv>) => h.calls.filter(([m]) => !m.startsWith("dna.") && !m.startsWith("skill."));
import { projects } from "../store/select";
import { Home } from "./Home";

const diff: TaskDiff = {
  base: "main",
  merge_base: "abc12345ff",
  files: [{ path: "src/a.ts", added: 10, removed: 2 }],
  untracked: ["notes.md"],
  patch: "+x",
  truncated: false,
  compare_url: "https://github.com/acme/shop/compare/main...feat/b?expand=1",
  pushed: true,
};

type Opened = [string, string | undefined][];

function Harness({
  env,
  status = null,
  opened = [],
  registry = ["/repos/shop"],
  onSkills,
}: {
  env: ReturnType<typeof makeEnv>["env"];
  status?: DaemonStatus | null;
  opened?: Opened;
  registry?: string[];
  onSkills?(): void;
}) {
  const [draft, setDraft] = useState("");
  const [reg, setReg] = useState<Registry>(() => ({ ...loadRegistry(env.storage), repos: registry }));
  return (
    <Home
      projects={projects(env.cockpit.getState(), reg.repos, { hidden: reg.hidden, groups: reg.groups })}
      registry={reg}
      project="/repos/shop"
      onProject={() => {}}
      onReposChanged={setReg}
      onOpen={(id, tab) => opened.push([id, tab])}
      onSkills={onSkills}
      status={status}
      draft={draft}
      onDraft={setDraft}
    />
  );
}

function setup(
  answers = {},
  tasks = [task("a", { status: "working" })],
  events = [] as ReturnType<typeof ev>[],
  status: DaemonStatus | null = null,
  registry = ["/repos/shop"],
) {
  const h = makeEnv(answers);
  h.load(tasks, events);
  h.env.storage.setItem(REPOS_KEY, JSON.stringify(registry));
  const opened: Opened = [];
  renderWith(h.env, <Harness env={h.env} status={status} opened={opened} registry={registry} />);
  return { ...h, opened };
}

/** Picks an agent in the form's Agent menu. */
async function chooseAgent(label: "Claude" | "Codex") {
  await userEvent.click(screen.getByRole("button", { name: /^Agent:/ }));
  await userEvent.click(within(screen.getByRole("group", { name: "Agents" })).getByRole("button", { name: new RegExp(`^${label}`) }));
}

/** The form's repository chip, and the picker's options as shown. */
const repoChip = () => screen.getByRole("button", { name: /^Repository/ });
const projectOptions = () => within(screen.getByRole("listbox", { name: "Projects" })).getAllByRole("option").map((o) => o.textContent?.replace(/^[▾▸✓]?\s*/, "").trim());

/** The agent the form's Agent menu shows. */
const agentShown = () => screen.getByRole("button", { name: /^Agent:/ }).getAttribute("aria-label")?.replace(/^Agent: /, "");

/** Unfolds the rail's Sessions settings (folded by default). */
const openSessions = () => userEvent.click(screen.getByRole("button", { name: /^Sessions/ }));

const daemonStatus = (claude_inline_answers: boolean): DaemonStatus => ({
  pid: 1,
  boot_id: "b1",
  version: "0.1.0",
  live_sessions: 1,
  claude_inline_answers,
});

describe("Home (V3 group chat)", () => {
  it("the form dispatches: agent, repo, goal, and a branch named after the goal", async () => {
    const h = setup({ "task.create": task("new", { branch: "mirante/leia" }), "session.start": { boot: "booting" } });
    await chooseAgent("Codex");
    await userEvent.type(screen.getByLabelText("Goal"), "Leia este repositório e me explique");
    expect(screen.getByText("mirante/leia-este-repositorio-e-me-explique")).toBeInTheDocument();
    await userEvent.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(sent(h).map((c) => c[0])).toEqual(["task.create", "session.start"]));
    expect(sent(h)[0][1]).toEqual({
      repo: "/repos/shop",
      agent: "codex",
      branch: "mirante/leia-este-repositorio-e-me-explique",
      goal: "Leia este repositório e me explique",
      base: "HEAD",
    });
    expect(sent(h)[1][1]).toEqual({ task: "new", prompt: "Leia este repositório e me explique" });
  });

  it("Ctrl+Enter dispatches too; plain Enter is a new line", async () => {
    const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } });
    const box = screen.getByLabelText("Goal");
    await userEvent.type(box, "first line{Enter}second");
    expect(box).toHaveValue("first line\nsecond");
    expect(sent(h)).toEqual([]);
    await userEvent.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(sent(h).map((c) => c[0])).toEqual(["task.create", "session.start"]));
  });

  it("Claude is the default agent; the choice is remembered for next time", async () => {
    const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } });
    expect(agentShown()).toBe("Claude");
    await chooseAgent("Codex");
    expect(agentShown()).toBe("Codex");
    expect(h.env.storage.getItem(AGENT_KEY)).toBe("codex");
    // The menu marks the chosen one.
    await userEvent.click(screen.getByRole("button", { name: /^Agent:/ }));
    expect(within(screen.getByRole("group", { name: "Agents" })).getByRole("button", { name: /^Codex/ })).toHaveAttribute("aria-pressed", "true");
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("group", { name: "Agents" })).toBeNull();
    expect(screen.getByRole("button", { name: /^Agent:/ })).toHaveFocus();
  });

  it("starts on the remembered agent, and survives storage that throws", () => {
    const h = makeEnv();
    h.load([]);
    h.env.storage.setItem(AGENT_KEY, "codex");
    renderWith(h.env, <Harness env={h.env} />);
    expect(agentShown()).toBe("Codex");
    cleanup();
    const broken = makeEnv();
    broken.load([]);
    broken.env.storage = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
    };
    renderWith(broken.env, <Harness env={broken.env} registry={[]} />);
    expect(agentShown()).toBe("Claude");
  });

  it("the repository chip is one button that opens the projects: search focused, ↑/↓/Enter pick", async () => {
    const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } }, [], [], null, ["/repos/shop", "/repos/api", "/repos/apex"]);
    const chip = repoChip();
    expect(chip).toHaveTextContent("Repository shop");
    expect(chip).toHaveAttribute("aria-haspopup", "listbox");
    expect(chip).toHaveAttribute("aria-expanded", "false");
    // A click anywhere on it (here on its label) opens the list, never selects text.
    await userEvent.click(within(chip).getByText("Repository"));
    expect(chip).toHaveAttribute("aria-expanded", "true");
    const list = screen.getByRole("listbox", { name: "Projects" });
    expect(chip).toHaveAttribute("aria-controls", list.id);
    expect(projectOptions()).toEqual(["apex", "api", "shop"]);
    // The chosen one is checked; the search has the keyboard, on the chosen one.
    expect(screen.getByRole("option", { name: "shop" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("option", { name: "api" })).toHaveAttribute("aria-selected", "false");
    const search = screen.getByRole("combobox", { name: "Search projects" });
    expect(search).toHaveFocus();
    expect(search).toHaveAttribute("aria-activedescendant", screen.getByRole("option", { name: "shop" }).id);
    await userEvent.type(search, "ap");
    expect(projectOptions()).toEqual(["apex", "api"]);
    await userEvent.keyboard("{ArrowDown}");
    expect(search).toHaveAttribute("aria-activedescendant", screen.getByRole("option", { name: "api" }).id);
    await userEvent.keyboard("{Enter}");
    expect(screen.queryByRole("listbox", { name: "Projects" })).toBeNull();
    expect(repoChip()).toHaveTextContent("Repository api");
    expect(repoChip()).toHaveFocus();
    await userEvent.type(screen.getByLabelText("Goal"), "fix it{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(sent(h)[0]?.[1]).toMatchObject({ repo: "/repos/api", branch: "mirante/fix-it" }));
  });

  it("the repository picker: a click picks; Escape closes and keeps the chosen project", async () => {
    setup({}, [], [], null, ["/repos/shop", "/repos/api"]);
    await userEvent.click(repoChip());
    await userEvent.keyboard("{ArrowDown}{Escape}");
    expect(screen.queryByRole("listbox", { name: "Projects" })).toBeNull();
    expect(repoChip()).toHaveTextContent("Repository shop");
    expect(repoChip()).toHaveFocus();
    await userEvent.click(repoChip());
    await userEvent.click(screen.getByRole("option", { name: "api" }));
    expect(repoChip()).toHaveTextContent("Repository api");
    // Open again: api is the checked one now.
    await userEvent.click(repoChip());
    expect(screen.getByRole("option", { name: "api" })).toHaveAttribute("aria-selected", "true");
  });

  it("the repository picker groups a folder's projects like the rail (folding), and leaves hidden ones out", async () => {
    const h = makeEnv();
    h.load([]);
    const repos = ["/w/gol/site", "/w/gol/api", "/w/gol/old", "/repos/shop"];
    h.env.storage.setItem(REPOS_KEY, JSON.stringify(repos));
    h.env.storage.setItem(HIDDEN_KEY, JSON.stringify(["/w/gol/old"]));
    h.env.storage.setItem(GROUPS_KEY, JSON.stringify({ "/w/gol/site": "/w/gol", "/w/gol/api": "/w/gol", "/w/gol/old": "/w/gol" }));
    renderWith(h.env, <Harness env={h.env} registry={repos} />);
    await userEvent.click(repoChip());
    const group = screen.getByRole("option", { name: /gol \(2\)/ });
    expect(group).toHaveAttribute("aria-expanded", "true");
    expect(projectOptions()).toEqual(["gol (2)", "api", "site", "shop"]);
    expect(screen.queryByRole("option", { name: "old" })).toBeNull();
    // Its arrow folds it; ←/→ fold and unfold from the keyboard.
    await userEvent.click(group.querySelector(".picker-fold")!);
    expect(projectOptions()).toEqual(["gol (2)", "shop"]);
    expect(screen.getByRole("listbox", { name: "Projects" })).toBeInTheDocument();
    const search = screen.getByRole("combobox", { name: "Search projects" });
    expect(search).toHaveAttribute("aria-activedescendant", screen.getByRole("option", { name: /gol/ }).id);
    await userEvent.keyboard("{ArrowRight}");
    expect(projectOptions()).toEqual(["gol (2)", "api", "site", "shop"]);
    await userEvent.keyboard("{ArrowLeft}");
    expect(projectOptions()).toEqual(["gol (2)", "shop"]);
    // A search shows every match, folded or not.
    await userEvent.type(search, "si");
    expect(projectOptions()).toEqual(["gol (1)", "site"]);
    await userEvent.keyboard("{ArrowDown}{Enter}");
    expect(repoChip()).toHaveTextContent("Repository site");
  });

  /** A rail with the folder /w/gol (Site-B2C, bff-flight, docs) and /repos/shop. */
  function gol(answers = {}, tasks: ReturnType<typeof task>[] = []) {
    const h = makeEnv(answers);
    h.load(tasks);
    const repos = ["/w/gol/Site-B2C", "/w/gol/bff-flight", "/w/gol/docs", "/repos/shop"];
    h.env.storage.setItem(REPOS_KEY, JSON.stringify(repos));
    h.env.storage.setItem(GROUPS_KEY, JSON.stringify({ "/w/gol/Site-B2C": "/w/gol", "/w/gol/bff-flight": "/w/gol", "/w/gol/docs": "/w/gol" }));
    renderWith(h.env, <Harness env={h.env} registry={repos} />);
    return h;
  }
  const checklist = () => screen.getByRole("group", { name: /Repositories of gol this task may change/ });
  const box = (name: string | RegExp) => within(checklist()).getByRole("checkbox", { name });

  it("a folder is a target: its row picks it, the goal's repos are pre-checked, the task goes to the checked ones", async () => {
    const h = gol({ "task.create": task("new", { branch: "mirante/x" }), "session.start": { boot: "booting" } }, [
      task("old", { repo_path: "/w/gol/bff-flight", branch: "mirante/here-are-site-b2c-and-bff-flight-to" }),
    ]);
    await userEvent.click(repoChip());
    await userEvent.click(screen.getByRole("option", { name: /gol \(3\)/ }));
    expect(repoChip()).toHaveTextContent("Repository gol (3)");
    // Nothing named yet: nothing checked.
    expect(within(checklist()).getAllByRole("checkbox").filter((c) => (c as HTMLInputElement).checked)).toEqual([]);
    await userEvent.type(screen.getByLabelText("Goal"), "Here are Site-B2C and bff-flight to change");
    expect(box("Site-B2C")).toBeChecked();
    expect(box("bff-flight")).toBeChecked();
    expect(box("docs")).not.toBeChecked();
    expect(box("Select all")).toHaveProperty("indeterminate", true);
    // The hint: the folder, how many repos, the branch (past the one a task of the folder uses).
    const hint = screen.getByLabelText("Where the agent will work");
    expect(hint).toHaveTextContent("→ gol · 2 repos · ⎇ mirante/here-are-site-b2c-and-bff-flight-to-2");
    await userEvent.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(sent(h).map((c) => c[0])).toEqual(["task.create", "session.start"]));
    expect(sent(h)[0][1]).toEqual({
      repo: "/w/gol",
      repos: ["/w/gol/Site-B2C", "/w/gol/bff-flight"],
      agent: "claude",
      branch: "mirante/here-are-site-b2c-and-bff-flight-to-2",
      base: "HEAD",
      goal: "Here are Site-B2C and bff-flight to change",
    });
  });

  it("the checklist: select all, untick one, and nothing checked is refused before any call", async () => {
    const h = gol({ "task.create": task("new"), "session.start": { boot: "booting" } });
    await userEvent.click(repoChip());
    await userEvent.click(screen.getByRole("option", { name: /gol \(3\)/ }));
    await userEvent.type(screen.getByLabelText("Goal"), "tidy the readmes");
    await userEvent.keyboard("{Meta>}{Enter}{/Meta}");
    expect(screen.getByRole("alert")).toHaveTextContent("Check the repositories of gol this task may change.");
    expect(sent(h)).toEqual([]);
    await userEvent.click(box("Select all"));
    expect(box("docs")).toBeChecked();
    expect(screen.getByLabelText("Where the agent will work")).toHaveTextContent("3 repos");
    await userEvent.click(box("bff-flight"));
    expect(screen.getByLabelText("Where the agent will work")).toHaveTextContent("2 repos");
    await userEvent.click(screen.getByRole("button", { name: "Dispatch" }));
    await waitFor(() => expect(sent(h)[0]?.[1]).toMatchObject({ repo: "/w/gol", repos: ["/w/gol/docs", "/w/gol/Site-B2C"] }));
  });

  it("@agent <folder> on the one-line syntax targets the folder; a project picked again leaves it", async () => {
    const h = gol({ "task.create": task("new"), "session.start": { boot: "booting" } });
    await userEvent.type(screen.getByLabelText("Goal"), "@codex gol fix docs please");
    expect(box("docs")).toBeChecked();
    expect(screen.getByLabelText("Where the agent will work")).toHaveTextContent("→ gol · 1 repo ·");
    await userEvent.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() =>
      expect(sent(h)[0]?.[1]).toMatchObject({ repo: "/w/gol", repos: ["/w/gol/docs"], agent: "codex", goal: "fix docs please" }),
    );
  });

  it("a long project name is cut in the chip, its full path in the tooltip", async () => {
    const long = "/repos/a-very-long-repository-name-that-does-not-fit-in-the-toolbar";
    setup({}, [], [], null, [long]);
    const chip = repoChip();
    expect(chip).toHaveAttribute("title", long);
    expect(chip.querySelector(".token-name")).toHaveTextContent("a-very-long-repository-name-that-does-not-fit-in-the-toolbar");
    await userEvent.click(chip);
    expect(screen.getByRole("option", { name: /a-very-long/ })).toHaveAttribute("title", long);
  });

  it("the agent is a plain-text token (its name), labelled for screen readers, that opens the agents", async () => {
    setup();
    const chip = screen.getByRole("button", { name: /^Agent:/ });
    expect(chip).toHaveTextContent(/^claude$/);
    expect(chip).toHaveAttribute("aria-label", "Agent: Claude");
    await userEvent.click(chip);
    expect(screen.getByRole("group", { name: "Agents" })).toBeInTheDocument();
  });

  it("the send arrow is quiet while the goal is empty and lights up with text; ⌘↵ is the hint", async () => {
    setup();
    const send = screen.getByRole("button", { name: "Dispatch" });
    expect(send).toBeDisabled();
    expect(send).toHaveAttribute("title", expect.stringMatching(/^Dispatch \((⌘|Ctrl\+)↵\)$/));
    // No big button: the arrow is inside the field, with the goal.
    expect(send.closest(".composer-field")).toContainElement(screen.getByLabelText("Goal"));
    expect(screen.getByLabelText("Goal")).toHaveAttribute("rows", "1");
    await userEvent.type(screen.getByLabelText("Goal"), "fix it");
    expect(send).toBeEnabled();
    expect(document.querySelector(".composer-hint")).toHaveClass("on");
  });

  describe("the @ shortcut's suggestions", () => {
    const goal = () => screen.getByLabelText("Goal");
    const options = () => within(screen.getByRole("listbox", { name: "Suggestions" })).getAllByRole("option").map((o) => o.textContent);

    it("@ suggests the agents; arrows move, Enter picks", async () => {
      setup({}, [], [], null, ["/repos/shop", "/repos/api"]);
      await userEvent.type(goal(), "@");
      expect(options()).toEqual(["@claude", "@codex"]);
      expect(goal()).toHaveAttribute("aria-autocomplete", "list");
      await userEvent.keyboard("{ArrowDown}");
      expect(goal()).toHaveAttribute("aria-activedescendant", screen.getByRole("option", { name: "@codex" }).id);
      expect(screen.getByRole("option", { name: "@codex" })).toHaveAttribute("aria-selected", "true");
      await userEvent.keyboard("{Enter}");
      expect(goal()).toHaveValue("@codex ");
    });

    it("after the agent, the projects filtered as typed; Tab inserts the project and a space", async () => {
      setup({}, [], [], null, ["/repos/shop", "/repos/api"]);
      await userEvent.type(goal(), "@claude ");
      expect(options()).toEqual(["api/repos/api", "shop/repos/shop"]);
      await userEvent.type(goal(), "a");
      expect(options()).toEqual(["api/repos/api"]);
      await userEvent.keyboard("{Tab}");
      expect(goal()).toHaveValue("@claude api ");
      expect(goal()).toHaveFocus();
      expect(screen.queryByRole("listbox", { name: "Suggestions" })).toBeNull();
    });

    it("a click picks; Escape closes without changing the text", async () => {
      setup({}, [], [], null, ["/repos/shop", "/repos/api"]);
      await userEvent.type(goal(), "@claude s");
      await userEvent.keyboard("{Escape}");
      expect(screen.queryByRole("listbox", { name: "Suggestions" })).toBeNull();
      expect(goal()).toHaveValue("@claude s");
      await userEvent.type(goal(), "h");
      await userEvent.click(screen.getByRole("option", { name: /^shop/ }));
      expect(goal()).toHaveValue("@claude shop ");
    });

    it("never inside the goal text", async () => {
      setup({}, [], [], null, ["/repos/shop"]);
      await userEvent.type(goal(), "@claude shop/feat/x fix @co");
      expect(screen.queryByRole("listbox", { name: "Suggestions" })).toBeNull();
    });

    it("Enter that confirms an IME composition does not pick", () => {
      setup({}, [], [], null, ["/repos/shop"]);
      act(() => goal().focus());
      fireEvent.change(goal(), { target: { value: "@", selectionStart: 1 } });
      expect(screen.getByRole("listbox", { name: "Suggestions" })).toBeInTheDocument();
      fireEvent.keyDown(goal(), { key: "Enter", isComposing: true });
      fireEvent.keyDown(goal(), { key: "Enter", keyCode: 229 });
      expect(goal()).toHaveValue("@");
    });

    it("repo/ with no branch gets one named from the goal", async () => {
      const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } }, [task("a", { branch: "mirante/fix-the-login" })]);
      await userEvent.type(goal(), "@codex shop/ fix the login{Meta>}{Enter}{/Meta}");
      await waitFor(() =>
        expect(sent(h)[0]?.[1]).toEqual({ repo: "/repos/shop", agent: "codex", branch: "mirante/fix-the-login-2", goal: "fix the login", base: "HEAD" }),
      );
    });
  });

  it("a branch another task of the repo has gets -2, -3…", async () => {
    const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } }, [
      task("a", { branch: "mirante/fix-it" }),
      task("b", { branch: "mirante/fix-it-2" }),
      task("c", { branch: "mirante/other", repo_path: "/elsewhere" }),
    ]);
    await userEvent.type(screen.getByLabelText("Goal"), "Fix it");
    expect(screen.getByText("mirante/fix-it-3")).toBeInTheDocument();
    await userEvent.keyboard("{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(sent(h)[0]?.[1]).toMatchObject({ branch: "mirante/fix-it-3" }));
  });

  it("when git already has the branch, the next free name is tried", async () => {
    const h = setup({
      "task.create": (p: Record<string, unknown>) =>
        p.branch === "mirante/fix-it" || p.branch === "mirante/fix-it-2"
          ? rpcError(RpcCode.INVALID_PARAMS, `invalid branch name "${p.branch}": branch already exists; a base only applies to a new branch`)
          : task("new", { branch: String(p.branch) }),
      "session.start": { boot: "booting" },
    });
    await userEvent.type(screen.getByLabelText("Goal"), "fix it{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(sent(h).map((c) => c[0])).toEqual(["task.create", "task.create", "task.create", "session.start"]));
    expect(sent(h).map((c) => c[1].branch).slice(0, 3)).toEqual(["mirante/fix-it", "mirante/fix-it-2", "mirante/fix-it-3"]);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("the branch can be edited; an edited branch is used as typed (an existing one is checked out)", async () => {
    const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } });
    await userEvent.type(screen.getByLabelText("Goal"), "fix it");
    await userEvent.click(screen.getByRole("button", { name: "Edit branch" }));
    const branch = screen.getByLabelText("Branch");
    expect(branch).toHaveValue("mirante/fix-it");
    await userEvent.clear(branch);
    await userEvent.type(branch, "feat/mine");
    await userEvent.click(screen.getByRole("button", { name: "Dispatch" }));
    await waitFor(() => expect(sent(h)[0]?.[1]).toEqual({ repo: "/repos/shop", agent: "claude", branch: "feat/mine", goal: "fix it" }));
  });

  it("an edited branch git would refuse is explained without calling the daemon", async () => {
    const h = setup();
    await userEvent.type(screen.getByLabelText("Goal"), "fix it");
    await userEvent.click(screen.getByRole("button", { name: "Edit branch" }));
    await userEvent.clear(screen.getByLabelText("Branch"));
    await userEvent.type(screen.getByLabelText("Branch"), "a..b");
    await userEvent.click(screen.getByRole("button", { name: "Dispatch" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("cannot contain '..'");
    expect(sent(h)).toEqual([]);
  });

  it("with no project yet, the form says to add one", async () => {
    const h = makeEnv();
    h.load([]);
    renderWith(h.env, <Harness env={h.env} registry={[]} />);
    await userEvent.type(screen.getByLabelText("Goal"), "fix it{Meta>}{Enter}{/Meta}");
    expect(await screen.findByRole("alert")).toHaveTextContent("Add a repository under Projects first");
    expect(sent(h)).toEqual([]);
  });

  it("power users: @agent repo/branch goal in the goal box still works", async () => {
    const h = setup({ "task.create": task("new", { branch: "feat/login" }), "session.start": { boot: "booting" } });
    await userEvent.type(screen.getByLabelText("Goal"), "@codex shop/feat/login add a login page{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(sent(h).map((c) => c[0])).toEqual(["task.create", "session.start"]));
    expect(sent(h)[0][1]).toEqual({ repo: "/repos/shop", agent: "codex", branch: "feat/login", goal: "add a login page" });
    expect(sent(h)[1][1]).toEqual({ task: "new", prompt: "add a login page" });
  });

  it("explains a bad power-user branch without calling the daemon", async () => {
    const h = setup();
    await userEvent.type(screen.getByLabelText("Goal"), "@claude shop/a..b do it{Meta>}{Enter}{/Meta}");
    expect(await screen.findByRole("alert")).toHaveTextContent("cannot contain '..'");
    expect(sent(h)).toEqual([]);
  });

  describe("the @ line never asks for a repo or a branch", () => {
    const twoRepos = ["/repos/shop", "/repos/Site-B2C"];
    const founder = "@claude Ao buscar um voo na cultura Colombia, o preço aparece em reais indevidamente";
    const founderGoal = "Ao buscar um voo na cultura Colombia, o preço aparece em reais indevidamente";
    const pickRepo = async (name: string) => {
      await userEvent.click(repoChip());
      await userEvent.click(screen.getByRole("option", { name }));
    };

    it("@claude + a goal goes to the toolbar's repo on a branch Mirante names (the founder's input)", async () => {
      const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } }, [], [], null, twoRepos);
      await pickRepo("Site-B2C");
      fireEvent.change(screen.getByLabelText("Goal"), { target: { value: founder } });
      // The chips stay usable; the hint shows where it will go.
      expect(repoChip()).toBeEnabled();
      expect(screen.getByRole("button", { name: /^Agent:/ })).toBeEnabled();
      expect(screen.queryByText(/come from the @ line/)).toBeNull();
      const hint = screen.getByLabelText("Where the agent will work");
      expect(hint).toHaveTextContent("→ Site-B2C · ⎇ mirante/ao-buscar-um-voo-na-cultura-colombia-o");
      await userEvent.click(screen.getByRole("button", { name: "Dispatch" }));
      await waitFor(() => expect(sent(h).map((c) => c[0])).toEqual(["task.create", "session.start"]));
      expect(screen.queryByRole("alert")).toBeNull();
      expect(sent(h)[0][1]).toEqual({
        repo: "/repos/Site-B2C",
        agent: "claude",
        branch: "mirante/ao-buscar-um-voo-na-cultura-colombia-o",
        goal: founderGoal,
        base: "HEAD",
      });
    });

    it("@codex + a project name + a goal: that project, auto branch (-2 when taken); the agent chip follows the line", async () => {
      const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } }, [task("a", { branch: "mirante/corrigir-o-preco", repo_path: "/repos/Site-B2C" })], [], null, twoRepos);
      await userEvent.type(screen.getByLabelText("Goal"), "@codex site-b2c corrigir o preço");
      expect(agentShown()).toBe("Codex");
      expect(screen.getByLabelText("Where the agent will work")).toHaveTextContent("→ Site-B2C · ⎇ mirante/corrigir-o-preco-2");
      await userEvent.keyboard("{Meta>}{Enter}{/Meta}");
      await waitFor(() =>
        expect(sent(h)[0]?.[1]).toEqual({ repo: "/repos/Site-B2C", agent: "codex", branch: "mirante/corrigir-o-preco-2", goal: "corrigir o preço", base: "HEAD" }),
      );
    });

    it("an unknown a/b word is part of the goal, sent to the toolbar's repo", async () => {
      const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } });
      await userEvent.type(screen.getByLabelText("Goal"), "@claude web/x do it{Meta>}{Enter}{/Meta}");
      await waitFor(() => expect(sent(h)[0]?.[1]).toEqual({ repo: "/repos/shop", agent: "claude", branch: "mirante/web-x-do-it", goal: "web/x do it", base: "HEAD" }));
    });

    it("the auto branch in the hint can be edited", async () => {
      const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } });
      await userEvent.type(screen.getByLabelText("Goal"), "@claude fix it");
      await userEvent.click(screen.getByRole("button", { name: "Edit branch" }));
      await userEvent.clear(screen.getByLabelText("Branch"));
      await userEvent.type(screen.getByLabelText("Branch"), "feat/mine");
      await userEvent.click(screen.getByRole("button", { name: "Dispatch" }));
      await waitFor(() => expect(sent(h)[0]?.[1]).toEqual({ repo: "/repos/shop", agent: "claude", branch: "feat/mine", goal: "fix it" }));
    });

    it("choosing an agent in the toolbar rewrites the @ word", async () => {
      setup();
      await userEvent.type(screen.getByLabelText("Goal"), "@claude fix it");
      await chooseAgent("Codex");
      expect(screen.getByLabelText("Goal")).toHaveValue("@codex fix it");
    });

    it("the placeholder says the @ parts are optional", () => {
      setup();
      expect(screen.getByLabelText("Goal")).toHaveAttribute(
        "placeholder",
        expect.stringMatching(/^Ask an agent… \(optional: @claude or @codex, a project, or project\/branch\)$/),
      );
    });
  });

  it("Home is an inbox across projects: needs you, ready for review, working, grouped by project", async () => {
    setup(
      { "task.diff": diff },
      [
        task("n", { status: "needs_you" }),
        task("n2", { status: "needs_you", repo_path: "/repos/api" }),
        task("w", { status: "working" }),
        task("r"),
        task("i"),
      ],
      [ev(1, "r", "turn_ended"), ev(2, "w", "step", { type: "tool", tool: "Bash", summary: "rm -rf node_modules", ok: true })],
    );
    const inbox = screen.getByRole("main", { name: "Inbox" });
    const section = (name: RegExp) => within(inbox).getByRole("region", { name });
    const needs = section(/^Needs you \(2\)$/);
    // Grouped by project, each with its chip.
    expect(within(needs).getAllByRole("group").map((g) => g.getAttribute("aria-label"))).toEqual(["api", "shop"]);
    expect(within(needs).getAllByRole("article")).toHaveLength(2);
    expect(within(section(/^Ready for review \(1\)$/)).getByRole("article", { name: "claude on feat/r" })).toBeInTheDocument();
    const working = section(/^Working \(1\)$/);
    const row = within(working).getByRole("article", { name: "claude on feat/w" });
    // One line, in plain words: never the raw command.
    expect(row).toHaveTextContent("@claude");
    expect(row).toHaveTextContent("running a command");
    expect(row).not.toHaveTextContent("rm -rf");
    // Idle tasks wait folded away.
    expect(within(inbox).getByText(/^Idle \(1\)$/)).toBeInTheDocument();
    expect(screen.queryByRole("complementary", { name: "Board" })).toBeNull();
  });

  it("a hidden project's tasks are left out of the inbox (until it is shown again)", async () => {
    const h = makeEnv({ "dna.status": { available: false } });
    h.load([task("n", { status: "needs_you" }), task("n2", { status: "needs_you", repo_path: "/repos/api" })]);
    h.env.storage.setItem(HIDDEN_KEY, JSON.stringify(["/repos/api"]));
    renderWith(h.env, <Harness env={h.env} />);
    const inbox = screen.getByRole("main", { name: "Inbox" });
    const needs = within(inbox).getByRole("region", { name: /^Needs you \(1\)$/ });
    expect(within(needs).getAllByRole("group").map((g) => g.getAttribute("aria-label"))).toEqual(["shop"]);
    await userEvent.click(screen.getByRole("button", { name: "Show hidden projects (1)" }));
    await userEvent.click(screen.getByRole("button", { name: "Show api in Projects again" }));
    expect(within(inbox).getByRole("region", { name: /^Needs you \(2\)$/ })).toBeInTheDocument();
  });

  it("says when agents have waited on you more than 10 minutes", () => {
    const long = Date.now() - 11 * 60_000;
    setup({}, [task("a", { status: "needs_you", updated_at: long }), task("b", { status: "needs_you", updated_at: long }), task("c", { status: "needs_you", updated_at: Date.now() })]);
    expect(screen.getByText("2 agents have waited on you for over 10 min")).toBeInTheDocument();
  });

  it("the branch chip shows once there is a goal; attached context goes after the goal, labeled", async () => {
    const h = setup({
      "task.create": task("new"),
      "session.start": { boot: "booting" },
      "repo.files": (p: Record<string, unknown>) => (String(p.query).includes("log") ? ["src/login.ts", "docs/login.md"] : []),
      "skill.list": [{ name: "cargo-tests-first", description: "run the tests first", version: 1, scope: "personal", activation: "on_demand", status: "active", manifest_hash: "h", verified: true, injected_30d: 0, used_30d: 0, updated_at: 0 }],
    });
    expect(screen.queryByRole("button", { name: "Edit branch" })).toBeNull();
    await userEvent.type(screen.getByLabelText("Goal"), "Fix the login");
    expect(screen.getByText("mirante/fix-the-login")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Add context" }));
    const dialog = screen.getByRole("dialog", { name: "Add context" });
    await userEvent.type(within(dialog).getByLabelText("Search Files"), "log");
    await userEvent.click(await within(dialog).findByRole("button", { name: /^src\/login\.ts/ }));
    await userEvent.click(within(dialog).getByRole("button", { name: "Skills" }));
    await userEvent.click(await within(dialog).findByRole("button", { name: /^cargo-tests-first/ }));
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Add context" })).toBeNull();
    const attached = screen.getByRole("list", { name: "Attached context" });
    expect(within(attached).getAllByRole("listitem")).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: "Dispatch" }));
    await waitFor(() => expect(sent(h).map((c) => c[0])).toContain("task.create"));
    const goal = String(sent(h).find((c) => c[0] === "task.create")?.[1].goal);
    expect(goal.split("\n")[0]).toBe("Fix the login");
    expect(goal).toContain('- file: "src/login.ts"');
    expect(goal).toContain('- skill: "cargo-tests-first"');
    expect(goal).not.toContain("run the tests first");
    await waitFor(() => expect(screen.queryByRole("list", { name: "Attached context" })).toBeNull());
  });

  it("under memory pressure the form keeps working and says so quietly", async () => {
    const hint = "macOS reports memory pressure — Mirante keeps going; consider hibernating idle sessions";
    const elevated: DaemonStatus = {
      ...daemonStatus(false),
      memory: { level: "elevated", total: 1, free: 1, swap_used: 0, swap_total: 0, paused: false, pausing: "off" },
    };
    const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } }, [], [], elevated);
    const note = screen.getByText(hint);
    expect(note).not.toHaveClass("warn");
    await userEvent.type(screen.getByLabelText("Goal"), "fix it");
    const send = screen.getByRole("button", { name: "Dispatch" });
    expect(send).toBeEnabled();
    await userEvent.click(send);
    await waitFor(() => expect(sent(h).map((c) => c[0])).toEqual(["task.create", "session.start"]));
    cleanup();
    // Even a daemon set to pause: Dispatch is never disabled for memory (the daemon says why).
    setup({}, [], [], { ...elevated, memory: { ...elevated.memory!, level: "critical", paused: true } });
    expect(screen.getByText(hint)).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Goal"), "x");
    expect(screen.getByRole("button", { name: "Dispatch" })).toBeEnabled();
    cleanup();
    setup({}, [], [], { ...elevated, memory: { ...elevated.memory!, level: "normal" } });
    expect(screen.queryByText(hint)).toBeNull();
  });

  it("files that may hold secrets are flagged in the picker", async () => {
    setup({ "repo.files": ["src/env.ts", "config/.env"] });
    await userEvent.click(screen.getByRole("button", { name: "Add context" }));
    const dialog = screen.getByRole("dialog", { name: "Add context" });
    const secret = await within(dialog).findByRole("button", { name: /^config\/\.env/ });
    expect(secret).toHaveTextContent("may hold secrets");
    expect(within(dialog).getByRole("button", { name: /^src\/env\.ts/ })).not.toHaveTextContent("may hold secrets");
  });

  it("a power dispatch to another repo refuses files and notes attached for this one", async () => {
    const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" }, "repo.files": ["src/login.ts"] }, [task("x", { repo_path: "/repos/api" })]);
    await userEvent.click(screen.getByRole("button", { name: "Add context" }));
    const dialog = screen.getByRole("dialog", { name: "Add context" });
    await userEvent.click(await within(dialog).findByRole("button", { name: /^src\/login\.ts/ }));
    await userEvent.keyboard("{Escape}");
    await userEvent.type(screen.getByLabelText("Goal"), "@claude api/feat/y do it{Meta>}{Enter}{/Meta}");
    expect(await screen.findByRole("alert")).toHaveTextContent(/attached .* shop.*api|from shop/);
    expect(sent(h).map((c) => c[0])).not.toContain("task.create");
  });

  it("files attached for one repo are refused once the form's repo changes", async () => {
    const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" }, "repo.files": ["src/login.ts"] }, [task("x", { repo_path: "/repos/api" })], [], null, [
      "/repos/shop",
      "/repos/api",
    ]);
    await userEvent.click(screen.getByRole("button", { name: "Add context" }));
    const dialog = screen.getByRole("dialog", { name: "Add context" });
    await userEvent.click(await within(dialog).findByRole("button", { name: /^src\/login\.ts/ }));
    await userEvent.keyboard("{Escape}");
    await userEvent.click(repoChip());
    await userEvent.keyboard("api{Enter}");
    await userEvent.type(screen.getByLabelText("Goal"), "do it{Meta>}{Enter}{/Meta}");
    expect(await screen.findByRole("alert")).toHaveTextContent("The attached files and notes are from shop, not api");
    expect(sent(h).map((c) => c[0])).not.toContain("task.create");
  });

  it("the context search asks the daemon once typing settles, and the skills once per opening", async () => {
    const h = setup({
      "repo.files": ["src/login.ts"],
      "skill.list": [
        { name: "cargo-tests-first", description: "run the tests first", version: 1, scope: "personal", activation: "on_demand", status: "active", manifest_hash: "h", verified: true, injected_30d: 0, used_30d: 0, updated_at: 0 },
        { name: "docs", description: "write docs", version: 1, scope: "personal", activation: "on_demand", status: "active", manifest_hash: "h", verified: true, injected_30d: 0, used_30d: 0, updated_at: 0 },
      ],
    });
    const count = (m: string) => h.calls.filter(([x]) => x === m).length;
    await userEvent.click(screen.getByRole("button", { name: "Add context" }));
    const dialog = screen.getByRole("dialog", { name: "Add context" });
    await waitFor(() => expect(count("repo.files")).toBe(1));
    await userEvent.type(within(dialog).getByLabelText("Search Files"), "login");
    await waitFor(() => expect(count("repo.files")).toBe(2));
    await new Promise((r) => setTimeout(r, 200));
    expect(count("repo.files")).toBe(2);
    expect(h.calls.filter(([x]) => x === "repo.files").at(-1)?.[1]).toMatchObject({ query: "login" });
    await userEvent.click(within(dialog).getByRole("button", { name: "Skills" }));
    await within(dialog).findByRole("button", { name: /^docs/ });
    await userEvent.type(within(dialog).getByLabelText("Search Skills"), "cargo");
    expect(await within(dialog).findByRole("button", { name: /^cargo-tests-first/ })).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: /^docs/ })).toBeNull();
    expect(count("skill.list")).toBe(1);
  });

  it("with MIRANTE_AUTO_RESUME=off the setting is shown off and cannot be changed here", async () => {
    setup({}, [], [], { ...daemonStatus(false), auto_resume: false, auto_resume_forced_off: true });
    await openSessions();
    const settings = screen.getByRole("region", { name: "Sessions" });
    const box = within(settings).getByRole("checkbox", { name: "Resume sessions after a daemon restart" });
    expect(box).not.toBeChecked();
    expect(box).toBeDisabled();
    expect(settings).toHaveTextContent("MIRANTE_AUTO_RESUME=off");
  });

  it("the rail says closing the app keeps agents running, and switches auto-resume through the daemon", async () => {
    const h = setup({ "daemon.settings": (p: Record<string, unknown>) => ({ auto_resume: p.auto_resume }) }, [], [], { ...daemonStatus(false), auto_resume: true });
    await openSessions();
    const settings = screen.getByRole("region", { name: "Sessions" });
    expect(settings).toHaveTextContent("Closing Mirante leaves your agents running");
    const box = within(settings).getByRole("checkbox", { name: "Resume sessions after a daemon restart" });
    expect(box).toBeChecked();
    await userEvent.click(box);
    expect(h.calls).toContainEqual(["daemon.settings", { auto_resume: false }]);
    await waitFor(() => expect(box).not.toBeChecked());
  });

  it("the Sessions section sets when new sessions pause under memory pressure (off by default)", async () => {
    const h = setup(
      { "daemon.settings": (p: Record<string, unknown>) => ({ auto_resume: true, pause_new_sessions_under_pressure: p.pause_new_sessions_under_pressure }) },
      [],
      [],
      { ...daemonStatus(false), auto_resume: true, pause_new_sessions_under_pressure: "off" },
    );
    await openSessions();
    const settings = screen.getByRole("region", { name: "Sessions" });
    const pick = within(settings).getByRole("combobox", { name: "Pause new sessions under memory pressure" });
    expect(pick).toHaveValue("off");
    expect(within(pick).getAllByRole("option").map((o) => o.textContent)).toEqual(["Off", "Only when critical", "When elevated"]);
    expect(settings).toHaveTextContent("Mirante keeps working under pressure");
    await userEvent.selectOptions(pick, "critical");
    expect(h.calls).toContainEqual(["daemon.settings", { pause_new_sessions_under_pressure: "critical" }]);
    await waitFor(() => expect(pick).toHaveValue("critical"));
  });

  it("the pause setting is read-only under MIRANTE_MEMGUARD_PAUSE, and absent for an older daemon", async () => {
    setup({}, [], [], { ...daemonStatus(false), auto_resume: true, pause_new_sessions_under_pressure: "elevated", pause_forced_by_env: true });
    await openSessions();
    const settings = screen.getByRole("region", { name: "Sessions" });
    const pick = within(settings).getByRole("combobox", { name: "Pause new sessions under memory pressure" });
    expect(pick).toHaveValue("elevated");
    expect(pick).toBeDisabled();
    expect(settings).toHaveTextContent("MIRANTE_MEMGUARD_PAUSE");
    cleanup();
    setup({}, [], [], { ...daemonStatus(false), auto_resume: true });
    expect(within(screen.getByRole("region", { name: "Sessions" })).queryByRole("combobox")).toBeNull();
  });

  it("an agent in the rail with no live session selects it in the form and focuses the goal", async () => {
    setup({}, []);
    await userEvent.click(screen.getByRole("button", { name: /@codex/ }));
    expect(agentShown()).toBe("Codex");
    expect(screen.getByLabelText("Goal")).toHaveFocus();
  });

  it("an agent in the rail with one live session opens it (Terminal when it needs you)", async () => {
    const h = setup({}, [task("a", { status: "needs_you" }), task("x", { agent: "codex", status: "working" }), task("z", { agent: "codex", live: false })]);
    const rail = screen.getByRole("navigation", { name: "Projects and agents" });
    await userEvent.click(within(rail).getByRole("button", { name: /^@claude/ }));
    await userEvent.click(within(rail).getByRole("button", { name: /^@codex/ }));
    expect(h.opened).toEqual([
      ["a", "Terminal"],
      ["x", undefined],
    ]);
  });

  it("an agent with several live sessions lists them to pick from; Escape closes the list", async () => {
    const h = setup({}, [
      task("a", { status: "working" }),
      task("b", { status: "needs_you", repo_path: "/repos/api" }),
      task("x", { agent: "codex" }),
    ]);
    const rail = screen.getByRole("navigation", { name: "Projects and agents" });
    await userEvent.click(within(rail).getByRole("button", { name: /^@claude/ }));
    const list = within(rail).getByRole("list", { name: "Live @claude sessions" });
    expect(within(list).getAllByRole("button").map((b) => b.textContent)).toEqual(["feat/a · shopWorking", "feat/b · apiNeeds you", "New @claude task"]);
    await userEvent.keyboard("{Escape}");
    expect(within(rail).queryByRole("list", { name: "Live @claude sessions" })).toBeNull();
    await userEvent.click(within(rail).getByRole("button", { name: /^@claude/ }));
    await userEvent.click(within(rail).getByRole("button", { name: /^feat\/b/ }));
    expect(h.opened).toEqual([["b", "Terminal"]]);
    expect(within(rail).queryByRole("list", { name: "Live @claude sessions" })).toBeNull();
  });

  it("the list of live sessions also offers a new task", async () => {
    setup({}, [task("a"), task("b")]);
    const rail = screen.getByRole("navigation", { name: "Projects and agents" });
    await userEvent.click(within(rail).getByRole("button", { name: /^@claude/ }));
    await userEvent.click(within(rail).getByRole("button", { name: "New @claude task" }));
    expect(agentShown()).toBe("Claude");
    expect(screen.getByLabelText("Goal")).toHaveFocus();
  });

  it("a session named in the memory panel opens its detail", async () => {
    const h = setup({}, [task("a", { status: "working", memory: { bytes: 1, peak: 1, budget: 2, state: "ok" } })]);
    const panel = screen.getByRole("region", { name: "Memory guard" });
    await userEvent.click(within(panel).getByRole("button", { name: /^System memory/ }));
    await userEvent.click(within(panel).getByRole("button", { name: "feat/a" }));
    expect(h.opened).toEqual([["a", undefined]]);
  });

  it("adding a repo takes an absolute path only", async () => {
    setup();
    await userEvent.type(screen.getByLabelText("Repository path"), "shop{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent("absolute path");
  });

  describe("adding a project", () => {
    const rail = () => screen.getByRole("navigation", { name: "Projects and agents" });
    const mount = (over: Partial<ReturnType<typeof makeEnv>["env"]> = {}) => {
      const h = makeEnv();
      h.load([]);
      h.env.storage.setItem(REPOS_KEY, JSON.stringify(["/repos/shop"]));
      Object.assign(h.env, over);
      renderWith(h.env, <Harness env={h.env} />);
      return h;
    };

    it("Choose… opens the native folder picker and adds the folder picked", async () => {
      const h = mount({ pickFolder: vi.fn(async () => "/Users/me/code/api") });
      await userEvent.click(within(rail()).getByRole("button", { name: "Choose…" }));
      expect(h.env.pickFolder).toHaveBeenCalledTimes(1);
      expect(await within(rail()).findByRole("button", { name: /^api/ })).toBeInTheDocument();
      expect(JSON.parse(h.env.storage.getItem(REPOS_KEY)!)).toEqual(["/repos/shop", "/Users/me/code/api"]);
    });

    it("a cancelled picker adds nothing and says nothing", async () => {
      const h = mount({ pickFolder: vi.fn(async () => null) });
      await userEvent.click(within(rail()).getByRole("button", { name: "Choose…" }));
      await waitFor(() => expect(h.env.pickFolder).toHaveBeenCalled());
      expect(within(rail()).queryByRole("alert")).toBeNull();
      expect(JSON.parse(h.env.storage.getItem(REPOS_KEY)!)).toEqual(["/repos/shop"]);
    });

    it("a pasted ~/ path is expanded with the home the app reports", async () => {
      const h = mount({ homeDir: vi.fn(async () => "/Users/me") });
      await userEvent.type(screen.getByLabelText("Repository path"), "~/code/api");
      await userEvent.click(within(rail()).getByRole("button", { name: "Add" }));
      expect(await within(rail()).findByRole("button", { name: /^api/ })).toBeInTheDocument();
      expect(h.env.pathKind).toHaveBeenCalledWith("/Users/me/code/api");
      expect(screen.getByLabelText("Repository path")).toHaveValue("");
    });

    it.each([
      ["missing", "/Users/me/nope does not exist"],
      ["file", "/Users/me/nope is a file, not a folder"],
    ] as const)("a path that is %s is refused with a plain reason", async (kind, msg) => {
      const h = mount({ pathKind: vi.fn(async () => kind) });
      await userEvent.type(screen.getByLabelText("Repository path"), "/Users/me/nope{Enter}");
      expect(await within(rail()).findByRole("alert")).toHaveTextContent(msg);
      expect(JSON.parse(h.env.storage.getItem(REPOS_KEY)!)).toEqual(["/repos/shop"]);
    });
  });

  it("Claude cards follow the daemon's inline-answer setting (off when unknown)", async () => {
    const asks = [ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" })];
    const h = setup({}, [task("a", { status: "needs_you" })], asks);
    const feed = screen.getByRole("main", { name: "Inbox" });
    expect(within(feed).queryByRole("button", { name: "Allow once" })).toBeNull();
    await userEvent.click(within(feed).getByRole("button", { name: "Answer in terminal" }));
    expect(h.opened).toEqual([["a", "Terminal"]]);
  });

  it("with inline answers on, a Claude card offers Allow once / Deny", () => {
    const asks = [ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" })];
    setup({}, [task("a", { status: "needs_you" })], asks, daemonStatus(true));
    const feed = screen.getByRole("main", { name: "Inbox" });
    expect(within(feed).getByRole("button", { name: "Allow once" })).toBeInTheDocument();
    expect(within(feed).queryByRole("button", { name: "Answer in terminal" })).toBeNull();
  });

  it("Enter pressed twice (or auto-repeat) dispatches once", async () => {
    const h = setup({ "task.create": () => new Promise(() => {}) });
    await userEvent.type(screen.getByLabelText("Goal"), "@codex shop/feat/x go{Meta>}{Enter}{Enter}{/Meta}");
    expect(h.calls.filter(([m]) => m === "task.create")).toHaveLength(1);
  });

  it("Enter while an IME is composing does not dispatch", () => {
    const h = setup();
    const box = screen.getByLabelText("Goal");
    fireEvent.change(box, { target: { value: "@codex shop/feat/x 日本" } });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true, isComposing: true });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true, keyCode: 229 });
    expect(sent(h)).toEqual([]);
  });

  it("text typed while a dispatch is in flight is kept", async () => {
    let finish: (v: unknown) => void = () => {};
    const h = setup({ "task.create": () => new Promise((r) => (finish = r)), "session.start": { boot: "booting" } });
    const box = screen.getByLabelText("Goal");
    await userEvent.type(box, "@codex shop/feat/x go{Meta>}{Enter}{/Meta}");
    await userEvent.type(box, " and more");
    await act(async () => finish(task("new", { branch: "feat/x" })));
    await waitFor(() => expect(sent(h).map((c) => c[0])).toEqual(["task.create", "session.start"]));
    expect(box).toHaveValue("@codex shop/feat/x go and more");
  });

  it("a dispatched draft is cleared once created", async () => {
    setup({ "task.create": task("new", { branch: "feat/x" }), "session.start": { boot: "booting" } });
    const box = screen.getByLabelText("Goal");
    await userEvent.type(box, "@codex shop/feat/x go{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(box).toHaveValue(""));
  });

  it("says so when the task was created but the agent did not start", async () => {
    setup({ "task.create": task("new", { branch: "feat/x" }), "session.start": rpcError(RpcCode.DRIVER, "codex not found") });
    await userEvent.type(screen.getByLabelText("Goal"), "@codex shop/feat/x go{Meta>}{Enter}{/Meta}");
    expect(await screen.findByRole("alert")).toHaveTextContent("feat/x was created but the agent did not start");
    expect(screen.getByRole("alert")).toHaveTextContent("codex not found");
  });

  it("an ambiguous repo name is refused without calling the daemon", async () => {
    const h = setup({}, [task("a", { status: "working" })], [], null, ["/repos/shop", "/other/shop"]);
    await userEvent.type(screen.getByLabelText("Goal"), "@claude shop/feat/x go{Meta>}{Enter}{/Meta}");
    expect(await screen.findByRole("alert")).toHaveTextContent('"shop" names several repos');
    expect(sent(h)).toEqual([]);
  });

  it("an added repo can be removed from Projects", async () => {
    setup({}, [], [], null, ["/repos/shop", "/repos/api"]);
    const rail = screen.getByRole("navigation", { name: "Projects and agents" });
    await userEvent.click(within(rail).getByRole("button", { name: "Remove api from Projects" }));
    expect(within(rail).queryByRole("button", { name: /^api/ })).toBeNull();
    expect(within(rail).getByRole("button", { name: /^shop/ })).toBeInTheDocument();
  });

  it("the rail's needs-you count is read as words, the badge itself hidden", () => {
    setup({}, [task("a", { status: "needs_you" })]);
    const rail = screen.getByRole("navigation", { name: "Projects and agents" });
    expect(within(rail).getByRole("button", { name: "shop, 1 needs you" })).toBeInTheDocument();
    expect(rail.querySelector(".badge")).toHaveAttribute("aria-hidden", "true");
  });

  it("one status region announces a card's change of state", () => {
    const h = setup({}, [task("a", { status: "working" })]);
    const region = screen.getByRole("status", { name: "Agent updates" });
    expect(region).toHaveTextContent("");
    act(() => h.env.cockpit.dispatch({ type: "event", event: ev(5, "a", "status", { to: "needs_you" }) }));
    expect(region).toHaveTextContent("goal of a: Needs you");
  });

  it("the rail ends with the memory guard panel, fed by daemon.status", () => {
    const sys = {
      level: "critical",
      total: 16 * 2 ** 30,
      free: 256 * 2 ** 20,
      swap_used: 0,
      swap_total: 0,
      paused: true,
      kernel_level: "critical",
      kernel_source: "macos",
    };
    setup({}, [task("a", { status: "working" })], [], { ...daemonStatus(false), memory: sys });
    const rail = screen.getByRole("navigation", { name: "Projects and agents" });
    const panel = within(rail).getByRole("region", { name: "Memory guard" });
    expect(panel).toHaveTextContent("macOS reported critical memory pressure · new sessions paused until it eases");
    expect(panel).toHaveTextContent("256 MB free of 16.0 GB");
  });
});

describe("the rail's skills entry", () => {
  it("shows how many skill proposals wait and opens the Skills view", async () => {
    const h = makeEnv({ "skill.pending": { pending: 2 } });
    h.load([task("a")]);
    let opened = 0;
    renderWith(h.env, <Harness env={h.env} onSkills={() => opened++} />);
    await userEvent.click(await screen.findByRole("button", { name: "Skills (2 to review)" }));
    expect(opened).toBe(1);
    // A count, not the queue: the rail never loads 200 proposals to show a number.
    expect(h.calls).toContainEqual(["skill.pending", {}]);
    expect(h.calls.some(([m]) => m === "skill.proposals")).toBe(false);
  });

  it("an older daemon without skills shows a plain Skills entry", async () => {
    const h = makeEnv({ "skill.pending": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method") });
    h.load([task("a")]);
    renderWith(h.env, <Harness env={h.env} onSkills={() => {}} />);
    expect(await screen.findByRole("button", { name: "Skills" })).toBeInTheDocument();
  });
});

describe("dispatch and the task's origin", () => {
  const created = (origin: unknown) => ({ ...task("n", { repo_path: "/repos/shop", branch: "feat/n" }), origin });
  const go = async (origin: unknown) => {
    const h = setup({ "task.create": created(origin), "session.start": { task: "n", boot: "ready", resumed: false, screen: "" } }, []);
    await userEvent.type(screen.getByLabelText("Goal"), "@claude shop/feat/n do it{Meta>}{Enter}{/Meta}");
    return h;
  };

  it("says which client's skills a new task follows", async () => {
    await go({ origin: "github.com/acme/shop", tenant: "github.com/acme", pinned: true });
    expect(await screen.findByText(/feat\/n follows github.com\/acme\/shop \(client github.com\/acme\), pinned for this checkout now/)).toBeInTheDocument();
  });

  it("says when the origin waits for a confirmation", async () => {
    await go({ origin: "github.com/acme/shop", tenant: "github.com/acme", observed: "github.com/evil/shop", pinned: false });
    expect(await screen.findByText(/no skills until you confirm its origin/)).toBeInTheDocument();
    expect(screen.getByText(/github.com\/evil\/shop/)).toBeInTheDocument();
  });
});

/** A press outside the form: plain text that takes no focus (as on macOS's WebKit). */
const pressOutside = () => fireEvent.pointerDown(document.body);

describe("the form's menus close like menus", () => {
  const menus = [
    { name: "the Agent menu", trigger: () => screen.getByRole("button", { name: /^Agent:/ }), shown: () => screen.queryByRole("dialog", { name: "Agent" }) },
    { name: "the + context picker", trigger: () => screen.getByRole("button", { name: "Add context" }), shown: () => screen.queryByRole("dialog", { name: "Add context" }) },
  ];
  for (const m of menus) {
    it(`${m.name}: a press outside closes it (and a press on its button toggles it)`, async () => {
      setup({ "repo.files": [] });
      await userEvent.click(m.trigger());
      expect(m.shown()).not.toBeNull();
      fireEvent.pointerDown(m.shown()!);
      expect(m.shown()).not.toBeNull();
      pressOutside();
      expect(m.shown()).toBeNull();
      await userEvent.click(m.trigger());
      await userEvent.click(m.trigger());
      expect(m.shown()).toBeNull();
      await act(async () => {});
    });

    it(`${m.name}: Escape closes it and the keyboard goes back to its button`, async () => {
      setup({ "repo.files": [] });
      await userEvent.click(m.trigger());
      act(() => {
        document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
      });
      expect(m.shown()).toBeNull();
      expect(m.trigger()).toHaveFocus();
      await act(async () => {});
    });

    it(`${m.name}: focus leaving it (or the window losing focus) closes it`, async () => {
      setup({ "repo.files": [] });
      await userEvent.click(m.trigger());
      act(() => screen.getByLabelText("Goal").focus());
      expect(m.shown()).toBeNull();
      await userEvent.click(m.trigger());
      fireEvent.blur(window);
      expect(m.shown()).toBeNull();
      await act(async () => {});
    });
  }

  it("the repository picker: a press outside, Escape, focus leaving or the window losing focus closes it", async () => {
    setup({}, [], [], null, ["/repos/shop", "/repos/api"]);
    const list = () => screen.queryByRole("listbox", { name: "Projects" });
    const reopen = async () => {
      await userEvent.click(repoChip());
      expect(list()).not.toBeNull();
    };
    await reopen();
    fireEvent.pointerDown(list()!);
    expect(list()).not.toBeNull();
    pressOutside();
    expect(list()).toBeNull();
    expect(repoChip()).toHaveTextContent("Repository shop");
    // A press on the chip toggles it.
    await reopen();
    await userEvent.click(repoChip());
    expect(list()).toBeNull();
    await reopen();
    act(() => {
      document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    expect(list()).toBeNull();
    await reopen();
    act(() => screen.getByLabelText("Goal").focus());
    expect(list()).toBeNull();
    await reopen();
    fireEvent.blur(window);
    expect(list()).toBeNull();
  });
});
