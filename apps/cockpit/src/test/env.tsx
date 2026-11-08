// Test harness: a real store fed by hand, a recording rpc, and the Env around a component.
import { render } from "@testing-library/react";
import type { ReactElement } from "react";
import { vi } from "vitest";
import { type Env, EnvContext } from "../env";
import { DaemonError } from "../lib/daemon";
import type { BridgeError, DaemonEvent, TaskView } from "../lib/protocol";
import { createCockpit } from "../store/cockpit";
import { snapshot } from "./fixtures";

type Answer = unknown | ((params: Record<string, unknown>) => unknown);

const ERROR_KINDS = new Set(["disconnected", "rpc", "protocol", "forbidden", "timeout"]);

/** A `BridgeError` answer (notes and proposals have a `kind` too: "fact", "gotcha"…). */
function isBridgeError(v: unknown): v is BridgeError {
  return !!v && typeof v === "object" && "kind" in v && ERROR_KINDS.has(String((v as { kind: unknown }).kind));
}

export function makeEnv(answers: Record<string, Answer> = {}) {
  const calls: [string, Record<string, unknown>][] = [];
  const rpc = vi.fn(async (method: string, params: object = {}) => {
    calls.push([method, params as Record<string, unknown>]);
    const a = answers[method];
    const v = typeof a === "function" ? (a as (p: Record<string, unknown>) => unknown)(params as Record<string, unknown>) : a;
    if (isBridgeError(v)) throw new DaemonError(v);
    return v as never;
  });
  const cockpit = createCockpit({ rpc, subscribe: async () => {} });
  const mem = new Map<string, string>();
  const env: Env = {
    cockpit,
    rpc,
    storage: { getItem: (k) => mem.get(k) ?? null, setItem: (k, v) => void mem.set(k, v) },
    openUrl: vi.fn(async () => {}),
    startDaemon: vi.fn(async () => {}),
    notify: vi.fn(),
  };
  return {
    env,
    calls,
    load(tasks: TaskView[], events: DaemonEvent[] = []) {
      cockpit.dispatch({ type: "connected", snapshot: snapshot(tasks, 0) });
      for (const e of events) cockpit.dispatch({ type: "event", event: e });
    },
  };
}

export function renderWith(env: Env, ui: ReactElement) {
  return render(<EnvContext.Provider value={env}>{ui}</EnvContext.Provider>);
}

export const rpcError = (code: number, message = "refused"): BridgeError => ({ kind: "rpc", code, message });
