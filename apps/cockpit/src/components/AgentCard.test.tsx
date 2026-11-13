import { act, cleanup as cleanupAll, fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { GRANT_ROOT_TOOL } from "../lib/labels";
import type { TaskDiff, TaskView } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { AgentCard } from "./AgentCard";

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

function setup(
  answers: Record<string, unknown>,
  t: TaskView,
  events: ReturnType<typeof ev>[] = [],
  inlineAnswers = true,
) {
  const h = makeEnv(answers);
  h.load([t], events);
  const opened: [string, string | undefined][] = [];
  renderWith(
    h.env,
    <AgentCard
      t={h.env.cockpit.getState().tasks[t.id]}
      inlineAnswers={inlineAnswers}
      onOpen={(id, tab) => opened.push([id, tab])}
    />,
  );
  return { ...h, opened };
}

describe("AgentCard", () => {
  it("shows the project and the agent, never raw steps, and answers a permission card once", async () => {
    const h = setup({ "permission.answer": { status: "allowed_once" } }, task("a", { status: "working" }), [
      ev(1, "a", "step", { type: "tool", tool: "Bash", summary: "npm test", ok: true }),
      ev(2, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "rm -rf dist" }),
    ]);
    const card = screen.getByRole("article", { name: "claude on feat/a" });
    expect(within(card).queryByText("Bash npm test")).toBeNull();
    expect(within(card).getByText("shop")).toBeInTheDocument();
    const perm = within(card).getByRole("group", { name: "Permission request: Bash" });
    expect(within(perm).getByText("once")).toBeInTheDocument();
    await userEvent.click(within(perm).getByRole("button", { name: "Allow once" }));
    expect(h.calls).toContainEqual(["permission.answer", { id: "p1", allow: true }]);
  });

  it("an already-answered card says so", async () => {
    setup({ "permission.answer": rpcError(RpcCode.CONFLICT) }, task("a"), [
      ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" }),
    ]);
    await userEvent.click(screen.getByRole("button", { name: "Deny" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("already answered");
    // Nothing left to answer: both buttons stay off until the closing event removes the card.
    expect(screen.getByRole("button", { name: "Deny" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Allow once" })).toBeDisabled();
  });

  it("after a sent answer the buttons stay off (sent…) until the card closes", async () => {
    const h = setup({ "permission.answer": { status: "allowed_once" } }, task("a", { status: "needs_you" }), [
      ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" }),
    ]);
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    expect(await screen.findByText("sent…")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Allow once" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Deny" })).toBeDisabled();
    expect(h.calls.filter(([m]) => m === "permission.answer")).toHaveLength(1);
  });

  it("a session-wide grant's accept button says it grants for the session", () => {
    setup({}, task("c", { agent: "codex", status: "needs_you" }), [
      ev(1, "c", "permission_requested", { permission_id: "p6", tool: GRANT_ROOT_TOOL, detail: "write under /wt/c" }),
    ]);
    expect(screen.getByRole("button", { name: "Allow for this session" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Allow once" })).toBeNull();
  });

  it("needs you with no request loaded yet offers Open terminal", async () => {
    const h = setup({}, task("a", { status: "needs_you" }));
    await userEvent.click(screen.getByRole("button", { name: "Open terminal" }));
    expect(h.opened).toEqual([["a", "Terminal"]]);
  });

  it("sub-tasks its agent proposed say so on the Needs you card, with Review", async () => {
    const h = setup({}, task("a", { spawn_pending: 2 }));
    expect(screen.getByText("Its agent proposed 2 sub-tasks")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Review" }));
    expect(h.opened).toEqual([["a", undefined]]);
  });

  it("the card's lines are not a live region (status changes are announced once, elsewhere)", () => {
    setup({}, task("a", { status: "working" }), [ev(1, "a", "step", { type: "tool", tool: "Bash", summary: "ls", ok: true })]);
    expect(document.querySelector("[aria-live]")).toBeNull();
  });

  it("a Claude card with inline answers off offers Answer in terminal, never Allow/Deny", async () => {
    const h = setup({}, task("a", { status: "needs_you" }), [
      ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "rm -rf dist" }),
    ], false);
    const perm = screen.getByRole("group", { name: "Permission request: Bash" });
    expect(within(perm).queryByRole("button", { name: "Allow once" })).toBeNull();
    expect(within(perm).queryByRole("button", { name: "Deny" })).toBeNull();
    await userEvent.click(within(perm).getByRole("button", { name: "Answer in terminal" }));
    expect(h.opened).toEqual([["a", "Terminal"]]);
    expect(h.calls.filter(([m]) => m === "permission.answer")).toEqual([]);
  });

  it("a refused inline answer (ANSWER_IN_TERMINAL) turns the card into Answer in terminal", async () => {
    const h = setup(
      { "permission.answer": rpcError(RpcCode.ANSWER_IN_TERMINAL, "answer this dialog in the terminal (mirante attach feat/a): a newer tool call") },
      task("a", { status: "needs_you" }),
      [ev(1, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "ls" })],
    );
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("a newer tool call");
    expect(screen.queryByRole("button", { name: "Allow once" })).toBeNull();
    expect(screen.getByRole("button", { name: "Answer in terminal" })).toHaveFocus();
    await userEvent.click(screen.getByRole("button", { name: "Answer in terminal" }));
    expect(h.opened).toEqual([["a", "Terminal"]]);
  });

  it("Codex cards keep Allow/Deny whatever the Claude setting", () => {
    setup({}, task("c", { agent: "codex", status: "needs_you" }), [
      ev(1, "c", "permission_requested", { permission_id: "p3", tool: "command", detail: "curl x" }),
    ], false);
    expect(screen.getByRole("button", { name: "Allow once" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Answer in terminal" })).toBeNull();
  });

  it("Codex Deny always sends a decline (allow: false)", async () => {
    const h = setup({ "permission.answer": { status: "denied" } }, task("c", { agent: "codex", status: "needs_you" }), [
      ev(1, "c", "permission_requested", { permission_id: "p4", tool: "command", detail: "curl x" }),
    ], false);
    await userEvent.click(screen.getByRole("button", { name: "Deny" }));
    expect(h.calls).toContainEqual(["permission.answer", { id: "p4", allow: false }]);
  });

  it("a session-wide grant, once allowed, reads as allowed for this session", () => {
    setup({}, task("c", { agent: "codex", status: "working" }), [
      ev(1, "c", "permission_requested", { permission_id: "p5", tool: GRANT_ROOT_TOOL, detail: "write under /wt/c" }),
      ev(2, "c", "permission_answered", { permission_id: "p5", answer: "allowed_session" }),
    ]);
    // Answered: the card has nothing left to ask (the answer is a line in the task's steps).
    expect(screen.queryByRole("group", { name: `Permission request: ${GRANT_ROOT_TOOL}` })).toBeNull();
  });

  it("labels Codex auto-edits and session-wide grants", () => {
    setup({}, task("c", { agent: "codex", status: "needs_you" }), [
      ev(1, "c", "permission_requested", { permission_id: "p2", tool: GRANT_ROOT_TOOL, detail: "write access under /wt/c for the rest of the session" }),
    ]);
    expect(screen.getByText("auto-edits")).toBeInTheDocument();
    expect(screen.getByText("session-wide grant")).toBeInTheDocument();
  });

  it("a finished turn shows the diff summary and opens the PR page", async () => {
    const h = setup({ "task.diff": diff }, task("b", { status: "idle" }), [ev(1, "b", "turn_started"), ev(2, "b", "turn_ended")]);
    const done = await screen.findByRole("group", { name: "Turn finished" });
    await within(done).findByText(/1 file changed/);
    expect(done).toHaveTextContent("1 file changed, +10 −2 vs main · 1 new untracked file");
    await userEvent.click(screen.getByRole("button", { name: "Open PR" }));
    expect(h.env.openUrl).toHaveBeenCalledWith(diff.compare_url);
  });

  it("only untracked files: counted as such, with no +0 −0", async () => {
    const only = { ...diff, files: [], untracked: [".serena/project.yml", ".serena/cache/x"] };
    setup({ "task.diff": only }, task("b", { status: "idle" }), [ev(1, "b", "turn_ended")]);
    const done = await screen.findByRole("group", { name: "Turn finished" });
    expect(done).toHaveTextContent("2 new untracked files");
    expect(done).not.toHaveTextContent(/files? changed|\+0/);
  });

  it("no change at all says so", async () => {
    setup({ "task.diff": { ...diff, files: [], untracked: [] } }, task("b", { status: "idle" }), [ev(1, "b", "turn_ended")]);
    expect(await screen.findByRole("group", { name: "Turn finished" })).toHaveTextContent("No changes vs main");
  });

  it("an unpushed branch cannot open a PR yet", async () => {
    setup({ "task.diff": { ...diff, pushed: false } }, task("b"), [ev(1, "b", "turn_ended")]);
    expect(await screen.findByRole("button", { name: "Open PR" })).toBeDisabled();
  });

  it("trust prompts are answered by the user from the card", async () => {
    const h = setup({ "session.trust": {} }, task("t", { boot: "trust_prompt" }));
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Trust folder" })));
    expect(h.calls).toContainEqual(["session.trust", { task: "t", accept: true }]);
  });

  it("a trust answer is sent once; both buttons stay off after it", async () => {
    const h = setup({ "session.trust": {} }, task("t", { boot: "trust_prompt" }));
    await userEvent.dblClick(screen.getByRole("button", { name: "Trust folder" }));
    expect(h.calls.filter(([m]) => m === "session.trust")).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Trust folder" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Decline" })).toBeDisabled();
  });

  it("declining the trust prompt sends accept: false", async () => {
    const h = setup({ "session.trust": {} }, task("t", { boot: "trust_prompt" }));
    await userEvent.click(screen.getByRole("button", { name: "Decline" }));
    expect(h.calls).toEqual([["session.trust", { task: "t", accept: false }]]);
  });

  it("a trust prompt answered elsewhere (CONFLICT) says so and stays off", async () => {
    setup({ "session.trust": rpcError(RpcCode.CONFLICT) }, task("t", { boot: "trust_prompt" }));
    await userEvent.click(screen.getByRole("button", { name: "Trust folder" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("already answered");
    expect(screen.getByRole("button", { name: "Trust folder" })).toBeDisabled();
  });

  it("a failed trust answer can be retried", async () => {
    setup({ "session.trust": rpcError(RpcCode.DRIVER, "pty gone") }, task("t", { boot: "trust_prompt" }));
    await userEvent.click(screen.getByRole("button", { name: "Trust folder" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("pty gone");
    expect(screen.getByRole("button", { name: "Trust folder" })).toBeEnabled();
  });

  it("shows the agent's last answer in two lines, expandable", async () => {
    const long = `Done. ${"I changed the login flow and the tests. ".repeat(8)}`;
    setup({ "task.diff": diff }, task("b", { status: "idle" }), [
      ev(1, "b", "turn_started"),
      ev(2, "b", "step", { type: "agent_message", text: "Looking around first." }),
      ev(3, "b", "step", { type: "agent_message", text: long, final: true }),
      ev(4, "b", "turn_ended"),
    ]);
    const card = screen.getByRole("article", { name: "claude on feat/b" });
    expect(within(card).getByText(long.trim(), { exact: false })).toBeInTheDocument();
    expect(within(card).queryByText(/Looking around first/)).toBeNull();
    const more = within(card).getByRole("button", { name: "more" });
    expect(more).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(more);
    expect(within(card).getByRole("button", { name: "less" })).toHaveAttribute("aria-expanded", "true");
  });

  it("a Claude session out of manual mode shows a red auto-mode badge that says how to go back", () => {
    setup({}, task("a", { status: "working", permission_mode: "auto" }));
    const badge = screen.getByText("auto mode on");
    expect(badge).toHaveClass("tag-danger");
    expect(badge.getAttribute("title")).toMatch(/approves its own commands.*Shift\+Tab/);
    cleanupAll();
    setup({}, task("a", { status: "working", permission_mode: "default" }));
    expect(screen.queryByText("auto mode on")).toBeNull();
  });

  it("shows how many skills the session was given, from its events", () => {
    setup({}, task("s"), [ev(1, "s", "skill.injected", { session_id: "x", names: ["a", "b"] })]);
    expect(screen.getByText("Skills 2")).toBeInTheDocument();
  });
});
