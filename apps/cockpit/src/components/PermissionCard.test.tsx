import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { GRANT_ROOT_TOOL } from "../lib/labels";
import { RpcCode } from "../lib/protocol";
import type { Perm } from "../store/state";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { task } from "../test/fixtures";
import { PermissionCard } from "./PermissionCard";

const perm = (over: Partial<Perm> = {}): Perm => ({ id: "p1", task_id: "a", tool: "Bash", detail: "rm -rf build", status: "pending", created_at: 1, seq: 1, rev: 1, ...over });

function setup(answers: Record<string, unknown> = {}, p = perm(), agent: "claude" | "codex" = "codex", inlineAnswers = true) {
  const h = makeEnv(answers);
  h.load([task("a", { agent })]);
  const onTerminal = vi.fn();
  renderWith(h.env, <PermissionCard perm={p} agent={agent} inlineAnswers={inlineAnswers} onAnswerInTerminal={onTerminal} />);
  return { ...h, onTerminal };
}

describe("PermissionCard", () => {
  it("shows what is asked and answers it once", async () => {
    const h = setup({ "permission.answer": {} });
    expect(screen.getByRole("group", { name: "Permission request: Bash" })).toHaveTextContent("rm -rf build");
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    expect(h.calls).toEqual(expect.arrayContaining([["permission.answer", { id: "p1", allow: true }]]));
    expect(screen.getByRole("button", { name: "Deny" })).toBeDisabled();
  });

  it("a session-wide grant is marked as one", () => {
    setup({}, perm({ tool: GRANT_ROOT_TOOL }));
    expect(screen.getByText("session-wide grant")).toHaveClass("tag-warn");
    expect(screen.getByRole("button", { name: "Allow for this session" })).toBeInTheDocument();
  });

  it("an answer given elsewhere says so and stays off", async () => {
    setup({ "permission.answer": rpcError(RpcCode.CONFLICT) });
    await userEvent.click(screen.getByRole("button", { name: "Deny" }));
    expect(screen.getByRole("alert")).toHaveTextContent("already answered");
    expect(screen.getByRole("button", { name: "Allow once" })).toBeDisabled();
  });

  it("a Claude request with inline answers off is answered in the terminal", async () => {
    const h = setup({}, perm(), "claude", false);
    await userEvent.click(screen.getByRole("button", { name: "Answer in terminal" }));
    expect(h.onTerminal).toHaveBeenCalled();
  });

  it("is off while the daemon is away", () => {
    const h = setup();
    act(() => h.env.cockpit.dispatch({ type: "disconnected", error: "gone", retryMs: 1000 }));
    expect(screen.getByRole("button", { name: "Allow once" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Deny" })).toBeDisabled();
  });
});
