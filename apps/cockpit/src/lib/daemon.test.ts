import { beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  Channel: class {
    onmessage: (m: unknown) => void = () => {};
  },
}));

import { attachAck, attachClose, attachOpen, attachWrite, DaemonError, daemonBinaryBuild, daemonRestart, daemonStart, rpc } from "./daemon";
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

  it("attach hands raw output (ArrayBuffer) through as bytes and reports the end", async () => {
    let channel: { onmessage: (m: unknown) => void } | undefined;
    invoke.mockImplementation(async (_cmd: string, args: { onMsg: typeof channel }) => {
      channel = args.onMsg;
      return { id: 7, input: true };
    });
    const chunks: number[][] = [];
    let ended = false;
    const a = await attachOpen("t1", 24, 80, (b) => chunks.push([...b]), () => (ended = true));
    channel!.onmessage(new Uint8Array([104, 0xff, 0]).buffer);
    channel!.onmessage({ type: "end" });
    expect([a, chunks, ended]).toEqual([{ id: 7, input: true }, [[104, 0xff, 0]], true]);
  });

  it("acks name the attachment and the byte count; a failed ack is not thrown", async () => {
    invoke.mockImplementation(async () => undefined);
    await attachAck(7, 65536);
    expect(invoke).toHaveBeenCalledWith("attach_ack", { id: 7, bytes: 65536 });
    invoke.mockImplementation(async () => {
      throw { kind: "protocol", message: "gone" };
    });
    await expect(attachAck(7, 1)).resolves.toBeUndefined();
  });

  it("ANSWER_IN_TERMINAL (code 8) is surfaced as such with the daemon's reason", async () => {
    invoke.mockImplementation(async () => {
      throw { kind: "rpc", code: 8, message: "answer feat/a in its terminal: the dialog changed" };
    });
    const e = (await rpc("permission.answer", { id: "p1", allow: true }).catch((x: unknown) => x)) as DaemonError;
    expect(e).toBeInstanceOf(DaemonError);
    expect([e.code, e.answerInTerminal, e.unsupported, e.message]).toEqual([
      RpcCode.ANSWER_IN_TERMINAL,
      true,
      false,
      "answer feat/a in its terminal: the dialog changed",
    ]);
  });

  it("typed bytes travel as base64, not a JSON array of numbers", async () => {
    invoke.mockImplementation(async () => undefined);
    await attachWrite(3, new Uint8Array([0x6c, 0x73, 0x0d, 0xff]));
    expect(invoke).toHaveBeenCalledWith("attach_write", { id: 3, b64: "bHMN/w==" });
  });

  it("a write timeout is a typed DaemonError", async () => {
    invoke.mockImplementation(async () => {
      throw { kind: "timeout", message: "the daemon did not take the input in 5s" };
    });
    const e = (await attachWrite(1, new Uint8Array([120])).catch((x: unknown) => x)) as DaemonError;
    expect([e instanceof DaemonError, e.info.kind, e.timedOut]).toEqual([true, "timeout", true]);
  });

  it("attachClose and daemonStart reject with DaemonError too", async () => {
    invoke.mockImplementation(async () => {
      throw "mirante-daemon not found";
    });
    const errs = await Promise.all([attachClose(1).catch((x: unknown) => x), daemonStart().catch((x: unknown) => x)]);
    for (const e of errs) expect(e).toBeInstanceOf(DaemonError);
    expect((errs[1] as DaemonError).message).toContain("mirante-daemon not found");
  });

  it("daemonBinaryBuild asks the shell and never rejects; daemonRestart says why it failed", async () => {
    invoke.mockImplementation(async () => ({ version: "0.1.0", build: "a.1" }));
    expect(await daemonBinaryBuild()).toEqual({ version: "0.1.0", build: "a.1" });
    expect(invoke).toHaveBeenCalledWith("daemon_binary_build");
    invoke.mockImplementation(async () => {
      throw "boom";
    });
    expect(await daemonBinaryBuild()).toBeNull();
    invoke.mockImplementation(async () => {
      throw "the old daemon is still running";
    });
    await expect(daemonRestart()).rejects.toThrow("the old daemon is still running");
    expect(invoke).toHaveBeenCalledWith("daemon_restart");
  });
});
