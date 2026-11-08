import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith } from "../test/env";
import { Grid } from "./Grid";

const status = { pid: 1, boot_id: "b1", version: "x", live_sessions: 2 };

describe("Grid (V2)", () => {
  it("shows live tiles with last lines, inline allow and a mini input", async () => {
    const h = makeEnv({ "permission.answer": {}, "message.send": { delivered: "queue", queued: 1 } });
    h.load([task("a", { status: "working" }), task("b", { status: "needs_you", agent: "codex" }), task("idle", { live: false, boot: null })], [
      ev(1, "a", "step", { type: "agent_message", text: "refactoring the cart" }),
      ev(2, "b", "permission_requested", { permission_id: "p9", tool: "command", detail: "npm publish" }),
    ]);
    renderWith(h.env, <Grid onOpen={() => {}} status={null} />);
    const tiles = screen.getAllByRole("article");
    expect(tiles.map((t) => t.getAttribute("aria-label"))).toEqual(["codex on feat/b", "claude on feat/a"]);
    expect(within(tiles[1]).getByText("refactoring the cart")).toBeInTheDocument();
    await userEvent.click(within(tiles[0]).getByRole("button", { name: "Allow once" }));
    expect(h.calls).toContainEqual(["permission.answer", { id: "p9", allow: true }]);
    await userEvent.type(within(tiles[1]).getByRole("textbox"), "add tests{Alt>}{Enter}{/Alt}");
    expect(h.calls).toContainEqual(["message.send", { task: "a", text: "add tests", mode: "queue" }]);
    expect(within(tiles[1]).getByRole("textbox")).toHaveValue("");
  });

  it("a Claude request is answered in the terminal unless the daemon types answers", async () => {
    const h = makeEnv({ "permission.answer": {} });
    h.load([task("b", { status: "needs_you" })], [ev(1, "b", "permission_requested", { permission_id: "p1", tool: "Bash", detail: "rm -rf dist" })]);
    const onOpen = vi.fn();
    const { unmount } = renderWith(h.env, <Grid onOpen={onOpen} status={status} />);
    expect(screen.queryByRole("button", { name: "Allow once" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Answer in terminal" }));
    expect(onOpen).toHaveBeenCalledWith("b", "Terminal");
    unmount();
    renderWith(h.env, <Grid onOpen={onOpen} status={{ ...status, claude_inline_answers: true }} />);
    expect(screen.getByRole("button", { name: "Allow once" })).toBeInTheDocument();
  });

  it("Enter steers; the trust card shows on its tile; empty when nothing is active", async () => {
    const h = makeEnv({ "message.send": { delivered: "steer", queued: 0 } });
    h.load([task("a", { status: "working" }), task("t", { boot: "trust_prompt" })]);
    renderWith(h.env, <Grid onOpen={() => {}} status={null} />);
    const [trust, work] = screen.getAllByRole("article");
    expect(within(trust).getByRole("group", { name: "Trust this folder" })).toBeInTheDocument();
    await userEvent.type(within(work).getByRole("textbox"), "go{Enter}");
    expect(h.calls).toContainEqual(["message.send", { task: "a", text: "go", mode: "steer" }]);
    expect(within(work).getByRole("status")).toHaveTextContent("sent");
  });

  it("says so when there are no active agents", () => {
    const h = makeEnv();
    h.load([task("idle", { live: false, boot: null })]);
    renderWith(h.env, <Grid onOpen={() => {}} status={null} />);
    expect(screen.getByText(/No active agents/)).toBeInTheDocument();
  });

  it("a task waiting with no modelled request points at its terminal", async () => {
    const h = makeEnv();
    h.load([task("w", { status: "needs_you" })]);
    const onOpen = vi.fn();
    renderWith(h.env, <Grid onOpen={onOpen} status={null} />);
    await userEvent.click(screen.getByRole("button", { name: "Open terminal" }));
    expect(onOpen).toHaveBeenCalledWith("w", "Terminal");
  });
});
