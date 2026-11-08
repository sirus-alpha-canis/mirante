// The only module that talks to Tauri. Everything else gets these functions injected
// (store/cockpit.ts) or imports this module (and tests mock it).
import { Channel, invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import type { BridgeError, StreamMsg } from "./protocol";
import { RpcCode } from "./protocol";

export class DaemonError extends Error {
  readonly info: BridgeError;
  constructor(info: BridgeError) {
    super(describe(info));
    this.info = info;
  }
  /** The daemon's error code, or null when the call never got an answer. */
  get code(): number | null {
    return this.info.kind === "rpc" ? this.info.code : null;
  }
  get unsupported(): boolean {
    return this.code === RpcCode.UNSUPPORTED || this.code === RpcCode.METHOD_NOT_FOUND;
  }
  /** Mirante typed nothing: the person answers the dialog in the agent's terminal. */
  get answerInTerminal(): boolean {
    return this.code === RpcCode.ANSWER_IN_TERMINAL;
  }
  /** The daemon did not take a write in time (it may be stalled). */
  get timedOut(): boolean {
    return this.info.kind === "timeout";
  }
}

function describe(e: BridgeError): string {
  switch (e.kind) {
    case "disconnected":
      return t("daemonErr.disconnected", { message: e.message });
    case "rpc":
      return e.message;
    case "protocol":
      return t("daemonErr.protocol", { message: e.message });
    case "forbidden":
      return t("daemonErr.forbidden", { method: e.method });
    case "timeout":
      return t("daemonErr.timeout", { message: e.message });
  }
}

export function toDaemonError(e: unknown): DaemonError {
  if (e instanceof DaemonError) return e;
  if (e && typeof e === "object" && "kind" in e) return new DaemonError(e as BridgeError);
  return new DaemonError({ kind: "protocol", message: String(e) });
}

export async function rpc<T>(method: string, params: object = {}): Promise<T> {
  try {
    return await invoke<T>("rpc", { method, params });
  } catch (e) {
    throw toDaemonError(e);
  }
}

/** (Re)starts the bridge's event stream; messages arrive in order on `onMsg`. */
export async function subscribe(onMsg: (m: StreamMsg) => void): Promise<void> {
  const ch = new Channel<StreamMsg>();
  ch.onmessage = onMsg;
  await invoke("subscribe", { onMsg: ch });
}

/** Output arrives raw (an `ArrayBuffer`); the end of the stream is `{ type: "end" }`. */
export type AttachMsg = ArrayBuffer | { type: "end" };

export interface Attached {
  id: number;
  input: boolean;
}

export async function attachOpen(
  task: string,
  rows: number,
  cols: number,
  onData: (bytes: Uint8Array) => void,
  onEnd: () => void,
): Promise<Attached> {
  const ch = new Channel<AttachMsg>();
  ch.onmessage = (m) => {
    if (m instanceof ArrayBuffer) onData(new Uint8Array(m));
    else if (ArrayBuffer.isView(m)) onData(new Uint8Array(m.buffer, m.byteOffset, m.byteLength));
    else if (m && m.type === "end") onEnd();
  };
  try {
    return await invoke<Attached>("attach_open", { task, rows, cols, onMsg: ch });
  } catch (e) {
    throw toDaemonError(e);
  }
}

/** The terminal drew `bytes` more of the attachment's output (the bridge's flow-control
 * window: with too much unacked it stops reading). Never rejects: an ack for a stream that
 * just ended has nothing to do. */
export async function attachAck(id: number, bytes: number): Promise<void> {
  try {
    await invoke("attach_ack", { id, bytes });
  } catch {
    // The attachment is gone; its window with it.
  }
}

function toBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

/** Typed bytes, as base64 (a JSON array of numbers would be about four times larger). */
export async function attachWrite(id: number, bytes: Uint8Array): Promise<void> {
  try {
    await invoke("attach_write", { id, b64: toBase64(bytes) });
  } catch (e) {
    throw toDaemonError(e);
  }
}

export async function attachClose(id: number): Promise<void> {
  try {
    await invoke("attach_close", { id });
  } catch (e) {
    throw toDaemonError(e);
  }
}

/** Starts `mirante-daemon` if nothing answers on the socket. Rejects with a message. */
export async function daemonStart(): Promise<void> {
  try {
    await invoke("daemon_start");
  } catch (e) {
    // The shell rejects with a plain message: the daemon could not be found or started.
    throw typeof e === "string" ? new DaemonError({ kind: "disconnected", message: e }) : toDaemonError(e);
  }
}

/** The user's home directory as the app process sees it (Tauri's path resolver). */
export async function homeDir(): Promise<string> {
  return await invoke<string>("home_dir");
}

/** What is at `path` (absolute): "dir", "file" or "missing". */
export async function pathKind(path: string): Promise<"dir" | "file" | "missing"> {
  return await invoke<"dir" | "file" | "missing">("path_kind", { path });
}
