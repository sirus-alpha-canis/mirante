// What components need from the outside world, injected so tests can fake it.
import { createContext, useContext } from "react";
import type { Note } from "./lib/notify";
import type { Cockpit } from "./store/cockpit";
import { useCockpitState } from "./store/cockpit";
import type { State } from "./store/state";

export interface Env {
  cockpit: Cockpit;
  rpc<T>(method: string, params?: object): Promise<T>;
  /** Where the repo registry lives (localStorage in the app). */
  storage: Pick<Storage, "getItem" | "setItem">;
  openUrl(url: string): Promise<void>;
  startDaemon(): Promise<void>;
  /** Native notification (already decided: what it says, and that it is not on screen). */
  notify(n: Note): void;
}

export const EnvContext = createContext<Env | null>(null);

export function useEnv(): Env {
  const env = useContext(EnvContext);
  if (!env) throw new Error("EnvContext missing");
  return env;
}

export function useStore(): State {
  return useCockpitState(useEnv().cockpit);
}
