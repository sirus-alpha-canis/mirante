import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage: (m: unknown) => void = () => {};
  },
}));

import { attachOpen, DaemonError, rpc } from "./daemon";
import { RpcCode } from "./protocol";

describe("daemon transport", () => {
  // Braces matter: a function returned from beforeEach is run as a cleanup hook.
  beforeEach(() => {
    invoke.mockClear();
  });

  it("rpc passes method and params to the bridge command", async () => {
    invoke.mockImplementation(async () => [1]);
    await expect(rpc("task.list")).resolves.toEqual([1]);
    expect(invoke).toHaveBeenCalledWith("rpc", { method: "task.list", params: {} });
  });

  it("bridge errors become DaemonError with the daemon code", async () => {
    invoke.mockImplementation(async () => {
      throw { kind: "rpc", code: RpcCode.UNSUPPORTED, message: "codex cannot resize" };
    });
    const e = (await rpc("session.resize").catch((x: unknown) => x)) as DaemonError;
    expect(e).toBeInstanceOf(DaemonError);
    expect([e.code, e.unsupported, e.message]).toEqual([3, true, "codex cannot resize"]);
  });

  it("attach decodes base64 chunks and reports the end", async () => {
    let channel: { onmessage: (m: unknown) => void } | undefined;
    invoke.mockImplementation(async (_cmd: string, args: { onMsg: typeof channel }) => {
      channel = args.onMsg;
      return { id: 7, input: true };
    });
    const chunks: string[] = [];
    let ended = false;
    const a = await attachOpen("t1", 24, 80, (b) => chunks.push(new TextDecoder().decode(b)), () => (ended = true));
    channel!.onmessage({ type: "data", b64: btoa("héllo".replace("é", "e")) });
    channel!.onmessage({ type: "end" });
    expect([a, chunks, ended]).toEqual([{ id: 7, input: true }, ["hello"], true]);
  });
});
