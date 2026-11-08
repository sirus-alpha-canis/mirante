import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import type { DaemonStatus, TaskDiff } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith } from "../test/env";
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
}: {
  env: ReturnType<typeof makeEnv>["env"];
  status?: DaemonStatus | null;
  opened?: Opened;
}) {
  const [draft, setDraft] = useState("");
  return (
    <Home
      projects={projects(env.cockpit.getState(), ["/repos/shop"])}
      project="/repos/shop"
      onProject={() => {}}
      onReposChanged={() => {}}
      onOpen={(id, tab) => opened.push([id, tab])}
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
) {
  const h = makeEnv(answers);
  h.load(tasks, events);
  const opened: Opened = [];
  renderWith(h.env, <Harness env={h.env} status={status} opened={opened} />);
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
  it("dispatches @agent repo/branch goal: task.create, then session.start with the goal", async () => {
    const h = setup({ "task.create": task("new", { branch: "feat/login" }), "session.start": { boot: "booting" } });
    await userEvent.type(screen.getByLabelText("Dispatch an agent"), "@codex shop/feat/login add a login page{Enter}");
    await waitFor(() => expect(h.calls.map((c) => c[0])).toEqual(["task.create", "session.start"]));
    expect(h.calls[0][1]).toEqual({ repo: "/repos/shop", agent: "codex", branch: "feat/login", goal: "add a login page" });
    expect(h.calls[1][1]).toEqual({ task: "new", prompt: "add a login page" });
  });

  it("explains a bad dispatch without calling the daemon", async () => {
    const h = setup();
    await userEvent.type(screen.getByLabelText("Dispatch an agent"), "@claude web/x do it{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent('unknown repo "web"');
    expect(h.calls).toEqual([]);
  });

  it("the board groups tasks into Needs you / Working / Ready for review", () => {
    setup({ "task.diff": diff }, [task("n", { status: "needs_you" }), task("w", { status: "working" }), task("r")], [ev(1, "r", "turn_ended")]);
    const board = screen.getByRole("complementary", { name: "Board" });
    const col = (name: string) => within(within(board).getByRole("region", { name })).getAllByRole("button").map((b) => b.textContent);
    expect(col("Needs you")).toEqual(["goal of n@claude · feat/n"]);
    expect(col("Working")).toEqual(["goal of w@claude · feat/w"]);
    expect(col("Ready for review")).toEqual(["goal of r@claude · feat/r"]);
  });

  it("an agent in the rail starts a message to it in the selected project", async () => {
    setup();
    await userEvent.click(screen.getByRole("button", { name: /@codex/ }));
    expect(screen.getByLabelText("Dispatch an agent")).toHaveValue("@codex shop/");
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

});
