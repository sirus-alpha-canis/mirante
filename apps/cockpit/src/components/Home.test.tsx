import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { type DaemonStatus, RpcCode, type TaskDiff } from "../lib/protocol";
import { REPOS_KEY } from "../lib/repos";
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
  const [repos, setRepos] = useState(registry);
  return (
    <Home
      projects={projects(env.cockpit.getState(), repos)}
      project="/repos/shop"
      onProject={() => {}}
      onReposChanged={setRepos}
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
    await userEvent.click(screen.getByRole("button", { name: "Codex" }));
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
    expect(screen.getByRole("button", { name: "Claude" })).toHaveAttribute("aria-pressed", "true");
    await userEvent.click(screen.getByRole("button", { name: "Codex" }));
    expect(screen.getByRole("button", { name: "Codex" })).toHaveAttribute("aria-pressed", "true");
    expect(h.env.storage.getItem(AGENT_KEY)).toBe("codex");
  });

  it("starts on the remembered agent, and survives storage that throws", () => {
    const h = makeEnv();
    h.load([]);
    h.env.storage.setItem(AGENT_KEY, "codex");
    renderWith(h.env, <Harness env={h.env} />);
    expect(screen.getByRole("button", { name: "Codex" })).toHaveAttribute("aria-pressed", "true");
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
    expect(screen.getByRole("button", { name: "Claude" })).toHaveAttribute("aria-pressed", "true");
  });

  it("the repo select lists the projects, starts on the selected one, and is what is sent", async () => {
    const h = setup({ "task.create": task("new"), "session.start": { boot: "booting" } }, [], [], null, ["/repos/shop", "/repos/api"]);
    const repo = screen.getByLabelText("Repository");
    expect(repo).toHaveValue("/repos/shop");
    await userEvent.selectOptions(repo, "/repos/api");
    await userEvent.type(screen.getByLabelText("Goal"), "fix it{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(sent(h)[0]?.[1]).toMatchObject({ repo: "/repos/api", branch: "mirante/fix-it" }));
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

  it("explains a bad power-user dispatch without calling the daemon", async () => {
    const h = setup();
    await userEvent.type(screen.getByLabelText("Goal"), "@claude web/x do it{Meta>}{Enter}{/Meta}");
    expect(await screen.findByRole("alert")).toHaveTextContent('unknown repo "web"');
    expect(sent(h)).toEqual([]);
  });

  it("the board groups tasks into Needs you / Working / Ready for review", () => {
    setup({ "task.diff": diff }, [task("n", { status: "needs_you" }), task("w", { status: "working" }), task("r")], [ev(1, "r", "turn_ended")]);
    const board = screen.getByRole("complementary", { name: "Board" });
    const col = (name: string) => within(within(board).getByRole("region", { name })).getAllByRole("button").map((b) => b.textContent);
    expect(col("Needs you")).toEqual(["goal of n@claude · feat/n"]);
    expect(col("Working")).toEqual(["goal of w@claude · feat/w"]);
    expect(col("Ready for review")).toEqual(["goal of r@claude · feat/r"]);
  });

  it("an agent in the rail with no live session selects it in the form and focuses the goal", async () => {
    setup({}, []);
    await userEvent.click(screen.getByRole("button", { name: /@codex/ }));
    expect(screen.getByRole("button", { name: "Codex" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByLabelText("Goal")).toHaveFocus();
  });

  it("adding a repo takes an absolute path only", async () => {
    setup();
    await userEvent.type(screen.getByLabelText("Repository path"), "shop{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent("absolute path");
  });

  it("Claude cards follow the daemon's inline-answer setting (off when unknown)", async () => {
    const asks = [ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" })];
    const h = setup({}, [task("a", { status: "needs_you" })], asks);
    const feed = screen.getByRole("main", { name: "Project feed" });
    expect(within(feed).queryByRole("button", { name: "Allow once" })).toBeNull();
    await userEvent.click(within(feed).getByRole("button", { name: "Answer in terminal" }));
    expect(h.opened).toEqual([["a", "Terminal"]]);
  });

  it("with inline answers on, a Claude card offers Allow once / Deny", () => {
    const asks = [ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" })];
    setup({}, [task("a", { status: "needs_you" })], asks, daemonStatus(true));
    const feed = screen.getByRole("main", { name: "Project feed" });
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
    const sys = { level: "critical", total: 16 * 2 ** 30, free: 256 * 2 ** 20, swap_used: 0, swap_total: 0, paused: true };
    setup({}, [task("a", { status: "working" })], [], { ...daemonStatus(false), memory: sys });
    const rail = screen.getByRole("navigation", { name: "Projects and agents" });
    const panel = within(rail).getByRole("region", { name: "Memory guard" });
    expect(panel).toHaveTextContent("System pressure critical · 256 MB free · new sessions paused");
  });
});

describe("the rail's skills entry", () => {
  it("shows how many skill proposals wait and opens the Skills view", async () => {
    const h = makeEnv({ "skill.proposals": [{ id: "sp-1" }, { id: "sp-2" }] });
    h.load([task("a")]);
    let opened = 0;
    renderWith(h.env, <Harness env={h.env} onSkills={() => opened++} />);
    await userEvent.click(await screen.findByRole("button", { name: "Skills (2 to review)" }));
    expect(opened).toBe(1);
    expect(h.calls).toContainEqual(["skill.proposals", { status: "pending", limit: 200 }]);
  });

  it("an older daemon without skills shows a plain Skills entry", async () => {
    const h = makeEnv({ "skill.proposals": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method") });
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
