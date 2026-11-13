import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { TaskView, ThreadLink } from "../lib/protocol";
import { task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { type PaneApi, PaneContext } from "./paneContext";
import { ThreadHeader, ThreadNotes } from "./ThreadHeader";

const pr: ThreadLink = { kind: "pr", ref: "acme/api#123", url: "https://github.com/acme/api/pull/123" };
const thread = (over: Partial<TaskView> = {}, link = pr) =>
  task("th", { title: "rebase", parent_id: "p", notes: "keep the API", thread: { link, notes_hash: "h1", delivered: false, summary: "Rebased; <i>tests</i> pass." }, ...over });

function header(th: TaskView, answers: Record<string, unknown> = {}) {
  const h = makeEnv(answers);
  h.load([task("p", { title: "main chat" }), th]);
  const pane: PaneApi = { taskId: "th", openFile: vi.fn(), openTarget: vi.fn() };
  const onSelect = vi.fn();
  renderWith(
    h.env,
    <PaneContext.Provider value={pane}>
      <ThreadHeader t={th} onSelect={onSelect} />
    </PaneContext.Provider>,
  );
  return { ...h, pane, onSelect };
}

describe("ThreadHeader", () => {
  it("link chip opens via openableLink; the parent and the summary (as text) show", async () => {
    const h = header(thread());
    const head = screen.getByRole("region", { name: "Thread" });
    await userEvent.click(within(head).getByRole("button", { name: "PR #123 ↗" }));
    expect(h.env.openUrl).toHaveBeenCalledWith("https://github.com/acme/api/pull/123");
    await userEvent.click(within(head).getByRole("button", { name: "↳ from main chat" }));
    expect(h.onSelect).toHaveBeenCalledWith("p");
    expect(within(head).getByText("Rebased; <i>tests</i> pass.")).toBeInTheDocument();
    expect(head.querySelector("i")).toBeNull();
    expect(within(head).getByText("The notes go out with your next message")).toBeInTheDocument();
  });

  it("a link outside the opener's rule is never opened: copy link instead", async () => {
    const h = header(thread({}, { kind: "issue", ref: "https://linear.app/acme/issue/ENG-1", url: "https://linear.app/acme/issue/ENG-1" }));
    expect(screen.queryByRole("button", { name: /↗/ })).toBeNull();
    expect(screen.getByRole("button", { name: "Copy link" })).toBeInTheDocument();
    expect(h.env.openUrl).not.toHaveBeenCalled();
  });

  it("Notes opens the side pane notes editor", async () => {
    const h = header(thread());
    await userEvent.click(screen.getByRole("button", { name: "Notes" }));
    expect(h.pane.openTarget).toHaveBeenCalledWith({ k: "notes" });
  });
});

describe("ThreadNotes", () => {
  it("saves with the expected hash; a conflict keeps the draft until the person loads the current notes", async () => {
    const saved = thread({ notes: "keep the API, add tests", thread: { link: pr, notes_hash: "h2", delivered: false } });
    let answer: unknown = rpcError(2, "changed");
    const h = makeEnv({ "task.thread": () => answer });
    h.load([thread()]);
    renderWith(h.env, <ThreadNotes taskId="th" />);
    const box = screen.getByLabelText("Thread notes");
    expect(box).toHaveValue("keep the API");
    await userEvent.type(box, ", add tests");
    await userEvent.click(screen.getByRole("button", { name: "Save notes" }));
    expect(h.calls).toContainEqual(["task.thread", { task: "th", notes: "keep the API, add tests", expect_hash: "h1" }]);
    expect(await screen.findByRole("alert")).toHaveTextContent("changed since you opened them");
    expect(box).toHaveValue("keep the API, add tests");
    await userEvent.click(screen.getByRole("button", { name: "Load the current notes" }));
    expect(box).toHaveValue("keep the API");
    answer = saved;
    await userEvent.type(box, ", add tests");
    await userEvent.click(screen.getByRole("button", { name: "Save notes" }));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Saved"));
    expect(box).toHaveValue("keep the API, add tests");
    expect(h.env.cockpit.getState().tasks.th.thread?.notes_hash).toBe("h2");
  });

  it("over 8 KiB cannot be saved; a task that is no thread has no notes", () => {
    const h = makeEnv();
    h.load([thread({ notes: "x".repeat(8 * 1024 + 1) }), task("plain")]);
    renderWith(h.env, <ThreadNotes taskId="th" />);
    expect(screen.getByText("8193 / 8192 bytes")).toHaveClass("error");
    renderWith(h.env, <ThreadNotes taskId="plain" />);
    expect(screen.getByText("This task is not a thread: it has no notes.")).toBeInTheDocument();
  });
});

describe("ThreadHeader: summaries", () => {
  it("Mirante's own summary needs no model; with summaries off there is no Claude button", async () => {
    const h = header(thread(), { "daemon.settings": { auto_resume: true, summaries: "none" }, "task.summarize": { summary: "idle · x", by: "deterministic" } });
    await waitFor(() => expect(h.calls).toContainEqual(["daemon.settings", {}]));
    expect(screen.queryByRole("button", { name: "Summarize with Claude…" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Summarize" }));
    expect(h.calls).toContainEqual(["task.summarize", { task: "th" }]);
  });

  it("Summarize with Claude shows the estimated cost first and needs a click every time", async () => {
    const h = header(thread(), { "daemon.settings": { auto_resume: true, summaries: "claude" }, "task.summarize": { summary: "Rebased.", by: "claude", cost_usd: 0.0106 } });
    const ask = await screen.findByRole("button", { name: "Summarize with Claude…" });
    await userEvent.click(ask);
    const confirm = screen.getByRole("group", { name: "Summarize with Claude" });
    expect(confirm).toHaveTextContent("≈ $0.01–0.03 of your Claude usage");
    expect(h.calls.filter(([m]) => m === "task.summarize")).toEqual([]);
    await userEvent.click(within(confirm).getByRole("button", { name: "Run" }));
    expect(h.calls.filter(([m]) => m === "task.summarize")).toEqual([["task.summarize", { task: "th", with: "claude" }]]);
    expect(await screen.findByRole("status")).toHaveTextContent("Done (it reported $0.0106)");
    // The next run asks again.
    expect(screen.queryByRole("group", { name: "Summarize with Claude" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Summarize with Claude…" }));
    expect(screen.getByRole("group", { name: "Summarize with Claude" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(h.calls.filter(([m]) => m === "task.summarize")).toHaveLength(1);
  });

  it("summary is untrusted text in the card and the header", () => {
    header(thread({ thread: { link: pr, notes_hash: "h", delivered: true, summary: "<img src=x onerror=alert(1)> done", summary_by: "claude" } }));
    expect(screen.getByText("<img src=x onerror=alert(1)> done")).toBeInTheDocument();
    expect(document.querySelector("img")).toBeNull();
  });
});
