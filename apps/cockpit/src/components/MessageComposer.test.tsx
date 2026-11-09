import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { RpcCode } from "../lib/protocol";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { task } from "../test/fixtures";
import { MessageComposer } from "./MessageComposer";

function setup(answers: Record<string, unknown> = {}, t = task("a")) {
  const h = makeEnv(answers);
  h.load([t]);
  renderWith(h.env, <MessageComposer t={t} />);
  return h;
}

const box = () => screen.getByRole("textbox", { name: "Message to the agent" });

describe("MessageComposer", () => {
  it("Steer now sends now and clears the box; Queue says where the message waits", async () => {
    const h = setup({ "message.send": (p: Record<string, unknown>) => (p.mode === "steer" ? { delivered: "steer" } : { delivered: "queue", queued: 2 }) });
    await userEvent.type(box(), "look at the tests");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    expect(h.calls).toContainEqual(["message.send", { task: "a", text: "look at the tests", mode: "steer" }]);
    expect(box()).toHaveValue("");
    expect(screen.getByRole("status")).toHaveTextContent("sent now");
    await userEvent.type(box(), "then the docs");
    await userEvent.click(screen.getByRole("button", { name: "Queue" }));
    expect(screen.getByRole("status")).toHaveTextContent("queued (#2, delivered when the turn ends)");
  });

  it("one arrow inside the field steers; ⌥-click (or the quiet queue token) queues; no big buttons", async () => {
    const h = setup({ "message.send": (p: Record<string, unknown>) => (p.mode === "steer" ? { delivered: "steer" } : { delivered: "queue", queued: 1 }) });
    const arrow = screen.getByRole("button", { name: "Steer now" });
    expect(arrow).toBeDisabled();
    expect(arrow.closest(".composer-field")).toContainElement(box());
    expect(document.querySelector(".btn-primary")).toBeNull();
    await userEvent.type(box(), "later");
    expect(arrow).toBeEnabled();
    fireEvent.click(arrow, { altKey: true });
    await waitFor(() => expect(box()).toHaveValue(""));
    expect(h.calls).toContainEqual(["message.send", { task: "a", text: "later", mode: "queue" }]);
    expect(screen.getByRole("button", { name: "Queue" })).toHaveAttribute("title", expect.stringMatching(/turn ends/));
  });

  it("an open request comes first", async () => {
    setup({ "message.send": rpcError(RpcCode.NEEDS_YOU, "needs you") });
    await userEvent.type(box(), "go on");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    expect(screen.getByRole("status")).toHaveTextContent("answer the open request first");
    expect(box()).toHaveValue("go on");
  });

  it("a stopped task has no composer", () => {
    setup({}, task("a", { live: false }));
    expect(screen.getByText(/No live session/)).toBeInTheDocument();
  });

  it("nothing is sent while the daemon is away", async () => {
    const h = setup({ "message.send": { delivered: "steer" } });
    await userEvent.type(box(), "go on");
    act(() => h.env.cockpit.dispatch({ type: "disconnected", error: "gone", retryMs: 1000 }));
    expect(screen.getByRole("button", { name: "Steer now" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Queue" })).toBeDisabled();
    await userEvent.type(box(), "{Meta>}{Enter}{/Meta}");
    expect(h.calls.filter(([m]) => m === "message.send")).toEqual([]);
  });
});
