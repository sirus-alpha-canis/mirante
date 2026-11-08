import { act, fireEvent, screen, within } from "@testing-library/react";
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
  it("shows the dispatch, live steps, and answers a permission card once", async () => {
    const h = setup({ "permission.answer": { status: "allowed_once" } }, task("a", { status: "working" }), [
      ev(1, "a", "step", { type: "tool", tool: "Bash", summary: "npm test", ok: true }),
      ev(2, "a", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "rm -rf dist" }),
    ]);
    const card = screen.getByRole("article", { name: "claude on feat/a" });
    expect(within(card).getByText("Bash npm test")).toBeInTheDocument();
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
    expect(screen.getByText("allowed for this session")).toBeInTheDocument();
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
    expect(await screen.findByText(/2 files changed/)).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Turn finished" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Open PR" }));
    expect(h.env.openUrl).toHaveBeenCalledWith(diff.compare_url);
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
});
