import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { handleDragDrop } from "../lib/drop";
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

describe("MessageComposer attachments", () => {
  const view = (id: string, name: string, extra: Record<string, unknown> = {}) => ({ id, path: `/m/sessions/a/attachments/1-${name}`, name, kind: "file", bytes: 2048, redacted: 0, ...extra });
  const png = () => new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], "shot.png", { type: "image/png" });

  it("a pasted screenshot is uploaded at once, shows as a chip with its thumbnail, and goes with an empty message", async () => {
    const h = setup({ "attachment.put": view("id1", "shot.png", { kind: "image", bytes: 4 }), "message.send": { delivered: "steer" } });
    fireEvent.paste(box(), { clipboardData: { files: [png()], getData: () => "" } });
    const tray = await screen.findByRole("list", { name: "Attachments" });
    await waitFor(() => expect(h.calls).toContainEqual(["attachment.put", { task: "a", name: "shot.png", b64: "iVBORw==" }]));
    expect(tray.querySelector("img")).toHaveAttribute("src", "data:image/png;base64,iVBORw==");
    await waitFor(() => expect(screen.getByRole("button", { name: "Steer now" })).toBeEnabled());
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    expect(h.calls).toContainEqual(["message.send", { task: "a", text: "", mode: "steer", attachments: ["id1"] }]);
    await waitFor(() => expect(screen.queryByRole("list", { name: "Attachments" })).toBeNull());
  });

  it("a text paste over 32 KB becomes pasted-text.txt; a short one stays inline", async () => {
    const h = setup({ "attachment.put": view("id1", "pasted-text.txt") });
    const long = "x".repeat(33 * 1024);
    fireEvent.paste(box(), { clipboardData: { files: [], getData: () => long } });
    await waitFor(() => expect(h.calls.find(([m]) => m === "attachment.put")?.[1]).toMatchObject({ name: "pasted-text.txt" }));
    expect(box()).toHaveValue("");
    fireEvent.paste(box(), { clipboardData: { files: [], getData: () => "short" } });
    expect(h.calls.filter(([m]) => m === "attachment.put")).toHaveLength(1);
  });

  it("the paperclip imports what the picker chose; a redacted HAR says how many secrets went", async () => {
    const h = setup({ "attachment.import": (p: Record<string, unknown>) => view(String(p.path).endsWith(".har") ? "h" : "l", String(p.path).split("/").pop()!, String(p.path).endsWith(".har") ? { redacted: 3 } : {}) });
    (h.env.pickFiles as ReturnType<typeof vi.fn>).mockResolvedValue(["/Users/me/trace.har", "/Users/me/app.log"]);
    await userEvent.click(screen.getByRole("button", { name: "Attach files" }));
    await screen.findByText("3 secrets removed");
    expect(h.calls).toContainEqual(["attachment.import", { task: "a", path: "/Users/me/trace.har" }]);
    expect(h.calls).toContainEqual(["attachment.import", { task: "a", path: "/Users/me/app.log" }]);
    expect(screen.getAllByText("2 KB")).toHaveLength(2);
  });

  it("× removes an attachment from the daemon and the tray", async () => {
    const h = setup({ "attachment.import": view("id9", "a.txt"), "attachment.remove": {} });
    (h.env.pickFiles as ReturnType<typeof vi.fn>).mockResolvedValue(["/Users/me/a.txt"]);
    await userEvent.click(screen.getByRole("button", { name: "Attach files" }));
    await userEvent.click(await screen.findByRole("button", { name: "Remove a.txt" }));
    expect(h.calls).toContainEqual(["attachment.remove", { task: "a", id: "id9" }]);
    expect(screen.queryByRole("list", { name: "Attachments" })).toBeNull();
  });

  it("a refused upload says why and leaves no chip", async () => {
    const h = setup({ "attachment.import": rpcError(RpcCode.INVALID_PARAMS, "files inside Mirante's own folder are refused") });
    (h.env.pickFiles as ReturnType<typeof vi.fn>).mockResolvedValue(["/Users/me/.mirante/x"]);
    await userEvent.click(screen.getByRole("button", { name: "Attach files" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Could not attach x: files inside Mirante's own folder are refused");
    expect(screen.queryByRole("list", { name: "Attachments" })).toBeNull();
  });

  it("after a daemon restart a send with old ids clears the tray and asks to attach again", async () => {
    const h = setup({
      "attachment.import": view("old", "a.txt"),
      "message.send": rpcError(RpcCode.INVALID_PARAMS, "unknown attachment old: attach the file again"),
    });
    (h.env.pickFiles as ReturnType<typeof vi.fn>).mockResolvedValue(["/Users/me/a.txt"]);
    await userEvent.click(screen.getByRole("button", { name: "Attach files" }));
    await screen.findByText("a.txt");
    await userEvent.type(box(), "see this");
    await userEvent.click(screen.getByRole("button", { name: "Steer now" }));
    expect(await screen.findByRole("status")).toHaveTextContent("The daemon restarted: attach the files again");
    expect(screen.queryByRole("list", { name: "Attachments" })).toBeNull();
    expect(box()).toHaveValue("see this");
  });

  it("a drop from Finder lands in the composer and is imported", async () => {
    const h = setup({ "attachment.import": view("d", "x.png", { kind: "image" }) });
    document.elementFromPoint = () => box();
    act(() => handleDragDrop({ type: "enter", paths: ["/Users/me/x.png"], position: { x: 1, y: 1 } }));
    expect(document.querySelector(".composer-msg")).toHaveClass("drop-on");
    act(() => handleDragDrop({ type: "drop", paths: ["/Users/me/x.png"], position: { x: 1, y: 1 } }));
    await waitFor(() => expect(h.calls).toContainEqual(["attachment.import", { task: "a", path: "/Users/me/x.png" }]));
    expect(document.querySelector(".composer-msg")).not.toHaveClass("drop-on");
  });
});
