import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { TaskView } from "../lib/protocol";
import { task } from "../test/fixtures";
import { makeEnv, renderWith } from "../test/env";
import { type ThreadApi, ThreadContext } from "./threadContext";
import { ThreadRefCard } from "./ThreadRefCard";

const link = { kind: "pr" as const, ref: "acme/api#123", url: "https://github.com/acme/api/pull/123" };
const thread = (over: Partial<TaskView> = {}) =>
  task("th", { title: "rebase the PR", parent_id: "p", thread: { link, notes_hash: "h", delivered: true }, ...over });

function setup(th: TaskView, answers: Record<string, unknown> = {}) {
  const h = makeEnv(answers);
  h.load([task("p"), th]);
  const api: ThreadApi = { newThreadFrom: vi.fn(), open: vi.fn() };
  renderWith(
    h.env,
    <ThreadContext.Provider value={api}>
      <ThreadRefCard id="th" title="old title" link={link} />
    </ThreadContext.Provider>,
  );
  return { ...h, api };
}

describe("ThreadRefCard", () => {
  it("Continue focuses the thread's composer (resumes when ended)", async () => {
    const ended = setup(thread({ live: false, status: "ended" }), { "session.resume": { task: "th", boot: "ready", resumed: true, screen: "" } });
    const card = screen.getByRole("article", { name: "Thread: PR #123" });
    expect(within(card).getByText("rebase the PR")).toBeInTheDocument();
    await userEvent.click(within(card).getByRole("button", { name: "Continue" }));
    expect(ended.calls).toEqual([["session.resume", { task: "th" }]]);
    await waitFor(() => expect(ended.api.open).toHaveBeenCalledWith("th", true));
  });

  it("a live thread is continued without a resume; Open opens it; a failed resume says why and stays", async () => {
    const h = setup(thread());
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(h.calls).toEqual([]);
    expect(h.api.open).toHaveBeenCalledWith("th", true);
    await userEvent.click(screen.getByRole("button", { name: "Open" }));
    expect(h.api.open).toHaveBeenCalledWith("th");
  });

  it("a resume that fails is shown and does not navigate", async () => {
    const h = setup(thread({ live: false, status: "ended" }), { "session.resume": { kind: "rpc", code: 2, message: "memory is tight" } });
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("memory is tight");
    expect(h.api.open).not.toHaveBeenCalled();
  });
});
