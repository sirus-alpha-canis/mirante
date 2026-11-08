import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { type QueuedMessage, RpcCode, type TaskView } from "../lib/protocol";
import { task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { QueueList } from "./QueueList";

const q = (id: string, text = `msg ${id}`): QueuedMessage => ({ id, text });

function setup(answers: Record<string, unknown> = {}, list: QueuedMessage[] | null = [q("a"), q("b"), q("c")], t: TaskView = task("t1", { status: "working", queued: 3 })) {
  const h = makeEnv(answers);
  h.load([t]);
  if (list) h.env.cockpit.dispatch({ type: "queue", taskId: t.id, list });
  renderWith(h.env, <QueueList t={h.env.cockpit.getState().tasks[t.id]} />);
  const region = () => screen.getByRole("region", { name: "Queued messages" });
  const rows = () => within(region()).getAllByRole("listitem").map((li) => li.querySelector(".text")?.textContent);
  return { ...h, region, rows };
}

/** The daemon's reply to a queue change: the queue after it. */
const after = (list: QueuedMessage[]) => () => list;

describe("QueueList (editable, reorderable queue)", () => {
  it("lists the queued messages, next first", () => {
    const h = setup();
    expect(h.rows()).toEqual(["msg a", "msg b", "msg c"]);
    expect(within(h.region()).getByText(/delivered when the turn ends/)).toBeInTheDocument();
  });

  it("shows nothing when nothing is queued", () => {
    setup({}, [], task("t1", { queued: 0 }));
    expect(screen.queryByRole("region", { name: "Queued messages" })).toBeNull();
  });

  it("loads the queue when the task says messages are queued", async () => {
    const h = setup({ "message.list_queued": [q("x", "from daemon")] }, null, task("t1", { queued: 1 }));
    await waitFor(() => expect(h.rows()).toEqual(["from daemon"]));
    expect(h.calls).toContainEqual(["message.list_queued", { task_id: "t1" }]);
  });

  it("edits a message inline and saves it", async () => {
    const h = setup({ "message.edit": after([q("a"), q("b", "run all tests"), q("c")]) });
    await userEvent.click(screen.getByRole("button", { name: "Edit queued message 2" }));
    const box = screen.getByLabelText("Edit queued message 2");
    expect(box).toHaveValue("msg b");
    await userEvent.clear(box);
    await userEvent.type(box, "run all tests{Enter}");
    expect(h.calls).toContainEqual(["message.edit", { id: "b", text: "run all tests" }]);
    expect(h.rows()).toEqual(["msg a", "run all tests", "msg c"]);
    expect(screen.getByRole("button", { name: "Edit queued message 2" })).toHaveFocus();
  });

  it("Escape cancels an edit without calling the daemon", async () => {
    const h = setup();
    await userEvent.click(screen.getByRole("button", { name: "Edit queued message 1" }));
    await userEvent.type(screen.getByLabelText("Edit queued message 1"), " more{Escape}");
    expect(screen.queryByRole("textbox", { name: "Edit queued message 1" })).toBeNull();
    expect(screen.getByRole("button", { name: "Edit queued message 1" })).toHaveFocus();
    expect(h.rows()).toEqual(["msg a", "msg b", "msg c"]);
    expect(h.calls).toEqual([]);
  });

  it("removes a message", async () => {
    const h = setup({ "message.remove": after([q("a"), q("c")]) });
    await userEvent.click(screen.getByRole("button", { name: "Remove queued message 2" }));
    expect(h.calls).toEqual([["message.remove", { id: "b" }]]);
    expect(h.rows()).toEqual(["msg a", "msg c"]);
  });

  it("moves a message up and down with the keyboard, keeping focus on it", async () => {
    const h = setup({ "message.reorder": (p: Record<string, unknown>) => (p.ids as string[]).map((id) => q(id)) });
    expect(screen.getByRole("button", { name: "Move queued message 1 up" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Move queued message 3 down" })).toBeDisabled();
    screen.getByRole("button", { name: "Move queued message 3 up" }).focus();
    await userEvent.keyboard("{Enter}");
    expect(h.calls).toEqual([["message.reorder", { task_id: "t1", ids: ["a", "c", "b"] }]]);
    expect(h.rows()).toEqual(["msg a", "msg c", "msg b"]);
    expect(screen.getByRole("button", { name: "Move queued message 2 up" })).toHaveFocus();
    await userEvent.keyboard("{Enter}");
    expect(h.rows()).toEqual(["msg c", "msg a", "msg b"]);
    // At the top its Up is off: focus moves to its Down.
    expect(screen.getByRole("button", { name: "Move queued message 1 down" })).toHaveFocus();
    await userEvent.keyboard(" ");
    expect(h.calls.at(-1)).toEqual(["message.reorder", { task_id: "t1", ids: ["a", "c", "b"] }]);
  });

  it("a message that went out meanwhile cannot be changed: says so and reloads", async () => {
    const h = setup({ "message.remove": rpcError(RpcCode.CONFLICT, "message b is no longer queued"), "message.list_queued": [q("c")] });
    await userEvent.click(screen.getByRole("button", { name: "Remove queued message 2" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("already being delivered");
    await waitFor(() => expect(h.rows()).toEqual(["msg c"]));
    expect(h.calls.map((c) => c[0])).toEqual(["message.remove", "message.list_queued"]);
  });

  it("one change at a time: a second click while one is in flight is ignored", async () => {
    let finish: (v: unknown) => void = () => {};
    const h = setup({ "message.remove": () => new Promise((r) => (finish = r)) });
    await userEvent.click(screen.getByRole("button", { name: "Remove queued message 1" }));
    await userEvent.click(screen.getByRole("button", { name: "Remove queued message 2" }));
    expect(h.calls).toEqual([["message.remove", { id: "a" }]]);
    await act(async () => finish([q("b"), q("c")]));
    expect(h.rows()).toEqual(["msg b", "msg c"]);
  });
});
