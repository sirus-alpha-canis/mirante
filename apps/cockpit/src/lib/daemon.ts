// The only module that talks to Tauri. Everything else gets these functions injected
// (store/cockpit.ts) or imports this module (and tests mock it).
import { Channel, invoke } from "@tauri-apps/api/core";
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
}

function describe(e: BridgeError): string {
  switch (e.kind) {
    case "disconnected":
      return `daemon not reachable: ${e.message}`;
    case "rpc":
      return e.message;
    case "protocol":
      return `daemon protocol: ${e.message}`;
    case "forbidden":
      return `${e.method} is not allowed from the cockpit`;
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

export type AttachMsg = { type: "data"; b64: string } | { type: "end" };

export interface Attached {
  id: number;
  input: boolean;
}

function fromBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export async function attachOpen(
  task: string,
  rows: number,
  cols: number,
  onData: (bytes: Uint8Array) => void,
  onEnd: () => void,
): Promise<Attached> {
  const ch = new Channel<AttachMsg>();
  ch.onmessage = (m) => (m.type === "data" ? onData(fromBase64(m.b64)) : onEnd());
  try {
    return await invoke<Attached>("attach_open", { task, rows, cols, onMsg: ch });
  } catch (e) {
    throw toDaemonError(e);
  }
}

export async function attachWrite(id: number, text: string): Promise<void> {
  const data = Array.from(new TextEncoder().encode(text));
  await invoke("attach_write", { id, data }).catch((e) => {
    throw toDaemonError(e);
  });
}

export async function attachClose(id: number): Promise<void> {
  await invoke("attach_close", { id });
}

/** Starts `mirante-daemon` if nothing answers on the socket. Rejects with a message. */
export async function daemonStart(): Promise<void> {
  await invoke("daemon_start");
}
