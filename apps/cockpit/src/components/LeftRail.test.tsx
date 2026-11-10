import { act, fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useCallback, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { useEnv, useSelect } from "../env";
import { M, RpcCode, type RepoScan } from "../lib/protocol";
import { GROUPS_KEY, loadRegistry, type Registry, REPOS_KEY } from "../lib/repos";
import { projects } from "../store/select";
import { same, type State } from "../store/state";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { task } from "../test/fixtures";
import { LeftRail, SessionSettings } from "./LeftRail";

type RailProps = Omit<Parameters<typeof LeftRail>[0], "projects" | "registry" | "status" | "selected">;

/** The rail as App feeds it: the projects worked out from the store and the registry. */
function Rail(props: RailProps) {
  const { storage } = useEnv();
  const [reg, setReg] = useState(() => loadRegistry(storage));
  const all = useSelect(
    useCallback((s: State) => projects(s, reg.repos, { hidden: reg.hidden, groups: reg.groups }), [reg]),
    same,
  );
  const changed = (r: Registry) => {
    setReg(r);
    props.onReposChanged(r);
  };
  return <LeftRail {...props} projects={all} registry={reg} selected="/repos/shop" status={null} onReposChanged={changed} />;
}

const REPO: RepoScan = { root: "repo", repos: [], truncated: false };

function setup(
  tasks = [task("a", { status: "working" })],
  registry = ["/repos/shop"],
  answers: Record<string, unknown> = {},
  groups: Record<string, string> = {},
  /** What `scanRepos` finds at a path (a repository elsewhere). */
  folders: Record<string, RepoScan> = {},
) {
  const h = makeEnv({ "dna.status": { available: false }, ...answers });
  vi.mocked(h.env.scanRepos).mockImplementation(async (p: string) => folders[p] ?? REPO);
  h.load(tasks);
  h.env.storage.setItem(REPOS_KEY, JSON.stringify(registry));
  h.env.storage.setItem(GROUPS_KEY, JSON.stringify(groups));
  const props = { onSelect: vi.fn(), onReposChanged: vi.fn(), onMention: vi.fn(), onOpen: vi.fn() };
  renderWith(h.env, <Rail {...props} />);
  return { ...h, ...props };
}

const projectList = () => within(screen.getByRole("list", { name: "Projects" }));
const row = (name: string) => projectList().queryByRole("button", { name: new RegExp(`^${name}`) });
const escape = () =>
  act(() => {
    document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  });

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

  it("a project with no tasks is removed at once", async () => {
    const h = setup([task("x", { repo_path: "/repos/api" })], ["/repos/shop"]);
    await userEvent.click(screen.getByRole("button", { name: "Remove shop from Projects" }));
    expect(h.onReposChanged).toHaveBeenCalledWith(expect.objectContaining({ repos: [] }));
    expect(row("shop")).toBeNull();
    expect(row("api")).not.toBeNull();
    expect(h.calls.filter(([m]) => m === M.taskArchive)).toEqual([]);
  });

  it("a refused path says why", async () => {
    const h = setup();
    vi.mocked(h.env.pathKind).mockResolvedValue("missing");
    await userEvent.type(screen.getByLabelText("Repository path"), "/nope{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent("/nope does not exist");
    await act(async () => {});
  });
});

describe("the rail's list of an agent's live sessions", () => {
  const two = () => setup([task("a", { branch: "feat/a" }), task("b", { branch: "feat/b" })]);
  const list = () => screen.queryByRole("list", { name: "Live @claude sessions" });
  const agent = () => screen.getByRole("button", { name: /^@claude/ });

  it("a press outside closes it", async () => {
    two();
    await userEvent.click(agent());
    expect(list()).not.toBeNull();
    fireEvent.pointerDown(list()!);
    expect(list()).not.toBeNull();
    fireEvent.pointerDown(document.body);
    expect(list()).toBeNull();
  });

  it("Escape closes it and the keyboard goes back to the agent", async () => {
    two();
    await userEvent.click(agent());
    act(() => {
      document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    expect(list()).toBeNull();
    expect(agent()).toHaveFocus();
  });

  it("focus leaving it, or the window losing focus, closes it", async () => {
    two();
    await userEvent.click(agent());
    act(() => screen.getByLabelText("Repository path").focus());
    expect(list()).toBeNull();
    await userEvent.click(agent());
    fireEvent.blur(window);
    expect(list()).toBeNull();
  });
});

describe("removing a project that has tasks", () => {
  const two = () => [task("a"), task("b", { live: false, status: "idle" })];
  const confirm = () => screen.queryByRole("group", { name: "Remove shop from Projects" });

  it("asks first, inline, and the keyboard goes there; Escape or a press outside closes it", async () => {
    const h = setup(two(), ["/repos/shop"]);
    const x = screen.getByRole("button", { name: "Remove shop from Projects" });
    await userEvent.click(x);
    expect(confirm()).toHaveTextContent(
      "shop has 2 tasks (1 live). Remove it from Projects? Its tasks are archived (live sessions are stopped first); worktrees are removed, branches are kept.",
    );
    expect(confirm()).toHaveFocus();
    expect(within(confirm()!).getByRole("button", { name: "Archive tasks and remove" })).toBeInTheDocument();
    escape();
    expect(confirm()).toBeNull();
    expect(x).toHaveFocus();
    await userEvent.click(x);
    fireEvent.pointerDown(document.body);
    expect(confirm()).toBeNull();
    expect(h.calls.filter(([m]) => m === M.taskArchive || m === M.sessionStop)).toEqual([]);
    expect(row("shop")).not.toBeNull();
  });

  it("archives the tasks (stopping the live one first) and the project is gone", async () => {
    const h = setup(two(), ["/repos/shop"], {
      [M.sessionStop]: (p: Record<string, unknown>) => task(String(p.task), { live: false }),
      [M.taskArchive]: (p: Record<string, unknown>) => task(String(p.task), { live: false, lifecycle: "archived" }),
    });
    await userEvent.click(screen.getByRole("button", { name: "Remove shop from Projects" }));
    await userEvent.click(screen.getByRole("button", { name: "Archive tasks and remove" }));
    expect(h.calls.filter(([m]) => m !== "dna.status").map(([m, p]) => `${m} ${p.task}`)).toEqual([`${M.sessionStop} a`, `${M.taskArchive} a`, `${M.taskArchive} b`]);
    expect(row("shop")).toBeNull();
    expect(confirm()).toBeNull();
    expect(loadRegistry(h.env.storage).repos).toEqual([]);
  });

  it("a task's project not in the registry can be removed the same way", async () => {
    setup([task("x", { repo_path: "/repos/api", live: false })], [], {
      [M.taskArchive]: (p: Record<string, unknown>) => task(String(p.task), { repo_path: "/repos/api", lifecycle: "archived", live: false }),
    });
    await userEvent.click(screen.getByRole("button", { name: "Remove api from Projects" }));
    await userEvent.click(screen.getByRole("button", { name: "Archive tasks and remove" }));
    expect(row("api")).toBeNull();
  });

  it("a dirty worktree the daemon refuses keeps the project, says why and offers the task", async () => {
    const h = setup(two(), ["/repos/shop"], {
      [M.sessionStop]: (p: Record<string, unknown>) => task(String(p.task), { live: false }),
      [M.taskArchive]: (p: Record<string, unknown>) =>
        p.task === "b" ? rpcError(RpcCode.CONFLICT, "the worktree /wt/b has uncommitted changes: ?? wip.txt") : task(String(p.task), { live: false, lifecycle: "archived" }),
    });
    await userEvent.click(screen.getByRole("button", { name: "Remove shop from Projects" }));
    await userEvent.click(screen.getByRole("button", { name: "Archive tasks and remove" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Not archived (1), so shop stays in Projects:");
    expect(alert).toHaveTextContent("goal of b (feat/b): the worktree /wt/b has uncommitted changes: ?? wip.txt");
    expect(row("shop")).not.toBeNull();
    expect(loadRegistry(h.env.storage).repos).toEqual(["/repos/shop"]);
    await userEvent.click(within(alert).getByRole("button", { name: "Open task" }));
    expect(h.onOpen).toHaveBeenCalledWith("b");
  });

  it("Hide keeps the tasks and hides the project; the hidden ones can be shown again", async () => {
    const h = setup(two(), ["/repos/shop", "/repos/api"]);
    await userEvent.click(screen.getByRole("button", { name: "Remove shop from Projects" }));
    await userEvent.click(screen.getByRole("button", { name: "Hide from Projects" }));
    expect(row("shop")).toBeNull();
    expect(loadRegistry(h.env.storage).hidden).toEqual(["/repos/shop"]);
    expect(h.calls.filter(([m]) => m === M.taskArchive)).toEqual([]);
    const toggle = screen.getByRole("button", { name: "Show hidden projects (1)" });
    await userEvent.click(toggle);
    expect(toggle).toHaveAttribute("aria-pressed", "true");
    expect(row("shop")).not.toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Show shop in Projects again" }));
    expect(loadRegistry(h.env.storage).hidden).toEqual([]);
    expect(screen.queryByRole("button", { name: /hidden projects/ })).toBeNull();
    expect(row("shop")).not.toBeNull();
  });
});

describe("adding a folder of repositories", () => {
  const gol: RepoScan = {
    root: null,
    truncated: false,
    repos: [
      { path: "/Users/me/gol/shop", name: "shop", kind: "repo", main_repo: null },
      { path: "/Users/me/gol/shop-fix", name: "shop-fix", kind: "worktree", main_repo: "/Users/me/gol/shop" },
      { path: "/Users/me/gol/api", name: "api", kind: "repo", main_repo: null },
    ],
  };

  it("a folder that is no repository lists the repositories in it: main ones picked, worktrees not", async () => {
    const h = setup([], [], {}, {}, { "/Users/me/gol": gol });
    await userEvent.type(screen.getByLabelText("Repository path"), "~/gol{Enter}");
    const picker = await screen.findByRole("region", { name: "Found 3 repositories in gol" });
    expect(h.env.scanRepos).toHaveBeenCalledWith("/Users/me/gol");
    const box = (n: RegExp) => within(picker).getByRole("checkbox", { name: n });
    expect(box(/^shop$/)).toBeChecked();
    expect(box(/^api$/)).toBeChecked();
    expect(box(/^shop-fix/)).not.toBeChecked();
    expect(within(picker).getByText("worktree of shop")).toBeInTheDocument();
    await userEvent.click(within(picker).getByRole("button", { name: "Select none" }));
    expect(within(picker).getByRole("button", { name: "Add selected (0)" })).toBeDisabled();
    await userEvent.click(within(picker).getByRole("button", { name: "Select all" }));
    await userEvent.click(box(/^shop-fix/));
    await userEvent.click(within(picker).getByRole("button", { name: "Add selected (2)" }));
    expect(screen.queryByRole("region", { name: /Found/ })).toBeNull();
    const reg = loadRegistry(h.env.storage);
    expect(reg.repos).toEqual(["/Users/me/gol/shop", "/Users/me/gol/api"]);
    expect(reg.groups).toEqual({ "/Users/me/gol/api": "/Users/me/gol", "/Users/me/gol/shop": "/Users/me/gol" });
    expect(screen.getByRole("button", { name: "gol (2)" })).toHaveAttribute("aria-expanded", "true");
  });

  it("says so when the folder holds none", async () => {
    const h = setup([], [], {}, {}, { "/Users/me/empty": { root: null, repos: [], truncated: false } });
    await userEvent.type(screen.getByLabelText("Repository path"), "/Users/me/empty{Enter}");
    expect(await screen.findByText("No git repositories in /Users/me/empty (looked 2 levels deep).")).toBeInTheDocument();
    expect(loadRegistry(h.env.storage).repos).toEqual([]);
  });

  it("a repository is added as before", async () => {
    const h = setup([], []);
    await userEvent.type(screen.getByLabelText("Repository path"), "/Users/me/shop{Enter}");
    await act(async () => {});
    expect(loadRegistry(h.env.storage).repos).toEqual(["/Users/me/shop"]);
  });
});

describe("project groups", () => {
  const groups = { "/g/api": "/g", "/g/web": "/g" };

  it("a folder's projects sit under its name, folded on request (remembered)", async () => {
    const h = setup([], ["/g/api", "/g/web", "/repos/shop"], {}, groups);
    const head = screen.getByRole("button", { name: "g (2)" });
    expect(head).toHaveAttribute("aria-expanded", "true");
    expect(row("api")).not.toBeNull();
    await userEvent.click(head);
    expect(head).toHaveAttribute("aria-expanded", "false");
    expect(row("api")).toBeNull();
    expect(row("shop")).not.toBeNull();
    expect(loadRegistry(h.env.storage).collapsed).toEqual(["/g"]);
  });

  it("the group's × removes all of it (no tasks: at once)", async () => {
    const h = setup([], ["/g/api", "/g/web", "/repos/shop"], {}, groups);
    await userEvent.click(screen.getByRole("button", { name: "Remove the g group from Projects" }));
    expect(loadRegistry(h.env.storage).repos).toEqual(["/repos/shop"]);
    expect(screen.queryByRole("button", { name: "g (2)" })).toBeNull();
  });

  it("a group with tasks asks first, with its tasks counted", async () => {
    setup([task("a", { repo_path: "/g/api" })], ["/g/api", "/g/web"], {}, groups);
    await userEvent.click(screen.getByRole("button", { name: "Remove the g group from Projects" }));
    expect(screen.getByRole("group", { name: "Remove g from Projects" })).toHaveTextContent("g has 1 task (1 live).");
  });

  it("many projects get a filter", async () => {
    const many = Array.from({ length: 10 }, (_, i) => `/repos/p${i}`);
    setup([], many);
    await userEvent.type(screen.getByRole("searchbox", { name: "Filter projects" }), "p7");
    expect(projectList().getAllByRole("button", { name: /^p\d/ }).map((b) => b.textContent)).toEqual(["p7"]);
  });
});

describe("a folder added before that is no repository", () => {
  it("says so and offers to scan it; the picked ones replace it", async () => {
    const h = setup([], ["/Users/me/gol"], {}, {}, {
      "/Users/me/gol": { root: null, truncated: false, repos: [{ path: "/Users/me/gol/shop", name: "shop", kind: "repo", main_repo: null }] },
    });
    expect(await screen.findByText("gol is a folder, not a repository")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Scan it for repositories" }));
    const picker = await screen.findByRole("region", { name: "Found 1 repository in gol" });
    await userEvent.click(within(picker).getByRole("button", { name: "Add selected (1)" }));
    expect(loadRegistry(h.env.storage).repos).toEqual(["/Users/me/gol/shop"]);
    expect(screen.queryByText("gol is a folder, not a repository")).toBeNull();
    expect(row("gol \\(1\\)")).not.toBeNull();
  });

  it("its × removes it at once", async () => {
    const h = setup([], ["/Users/me/gol"], {}, {}, { "/Users/me/gol": { root: null, truncated: false, repos: [] } });
    await screen.findByText("gol is a folder, not a repository");
    await userEvent.click(screen.getByRole("button", { name: "Remove gol from Projects" }));
    expect(loadRegistry(h.env.storage).repos).toEqual([]);
  });
});

describe("the rail's Sessions settings, folded by default", () => {
  const base = { pid: 1, boot_id: "b1", version: "x", live_sessions: 0 };
  const head = () => screen.getByRole("button", { name: /^Sessions/ });
  const mount = (status: Parameters<typeof SessionSettings>[0]["status"], answers: Record<string, unknown> = {}) => {
    const h = makeEnv(answers);
    h.load([]);
    const r = renderWith(h.env, <SessionSettings status={status} />);
    return { h, ...r };
  };

  it("one line with a summary; the settings hidden until opened", async () => {
    mount({ ...base, auto_resume: true, pause_new_sessions_under_pressure: "off" });
    expect(head()).toHaveAttribute("aria-expanded", "false");
    expect(head()).toHaveAccessibleName("Sessions Auto-resume on · Pause off");
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.queryByRole("combobox")).toBeNull();
    await userEvent.click(head());
    expect(screen.getByRole("checkbox", { name: "Resume sessions after a daemon restart" })).toBeVisible();
    expect(screen.getByRole("combobox", { name: "Pause new sessions under memory pressure" })).toBeVisible();
    expect(screen.getByText(/Mirante keeps working under pressure/)).toBeVisible();
    await userEvent.click(head());
    expect(screen.queryByRole("checkbox")).toBeNull();
  });

  it.each([
    [{ auto_resume: false, pause_new_sessions_under_pressure: "critical" as const }, "Auto-resume off · Pause when critical"],
    [{ auto_resume: true, auto_resume_forced_off: true, pause_new_sessions_under_pressure: "elevated" as const }, "Auto-resume off · Pause when elevated"],
    [{ auto_resume: true }, "Auto-resume on"],
    [{}, ""],
  ])("summarises %o as %s", (over, text) => {
    mount({ ...base, ...over });
    expect(head()).toHaveAccessibleName(`Sessions${text ? ` ${text}` : ""}`);
  });

  it("the summary follows a change made inside", async () => {
    mount(
      { ...base, auto_resume: true, pause_new_sessions_under_pressure: "off" },
      {
        "daemon.settings": (p: Record<string, unknown>) => ({
          auto_resume: p.auto_resume ?? true,
          pause_new_sessions_under_pressure: p.pause_new_sessions_under_pressure ?? "off",
        }),
      },
    );
    await userEvent.click(head());
    await userEvent.click(screen.getByRole("checkbox"));
    await userEvent.selectOptions(screen.getByRole("combobox"), "elevated");
    await userEvent.click(head());
    expect(head()).toHaveAccessibleName("Sessions Auto-resume off · Pause when elevated");
  });

  it("remembers being opened", async () => {
    const { h, unmount } = mount({ ...base, auto_resume: true });
    await userEvent.click(head());
    unmount();
    renderWith(h.env, <SessionSettings status={{ ...base, auto_resume: true }} />);
    expect(head()).toHaveAttribute("aria-expanded", "true");
  });
});
