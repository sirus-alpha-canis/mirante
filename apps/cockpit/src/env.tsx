// What components need from the outside world, injected so tests can fake it.
import { createContext, useContext } from "react";
import type { Note } from "./lib/notify";
import type { EditorId } from "./lib/daemon";
import type { DaemonBinary, RepoScan } from "./lib/protocol";
import type { Cockpit } from "./store/cockpit";
import { useCockpitSelect } from "./store/cockpit";
import type { State } from "./store/state";

export interface Env {
  cockpit: Cockpit;
  rpc<T>(method: string, params?: object): Promise<T>;
  /** Where the repo registry lives (localStorage in the app). */
  storage: Pick<Storage, "getItem" | "setItem">;
  openUrl(url: string): Promise<void>;
  startDaemon(): Promise<void>;
  /** What the daemon binary the app would start says it is; null when it cannot tell. */
  daemonBinaryBuild(): Promise<DaemonBinary | null>;
  /** Stops the running daemon and starts the one on disk. Rejects with a message. */
  restartDaemon(): Promise<void>;
  /** Native notification (already decided: what it says, and that it is not on screen). */
  notify(n: Note): void;
  /** The native folder picker (one folder); null when cancelled. */
  pickFolder(): Promise<string | null>;
  /** The user's home directory, as the app process sees it (for "~/…"). */
  homeDir(): Promise<string>;
  /** What is at an absolute path. */
  pathKind(path: string): Promise<PathKind>;
  /** The git checkouts in a folder (or that the folder is one). */
  scanRepos(path: string): Promise<RepoScan>;
  /** The editors installed here ("default" is the system's text editor). */
  editors(): Promise<EditorId[]>;
  /** Opens a task's worktree file (relative path) in an editor; rejects with a message. */
  openInEditor(task: string, path: string, line: number | null, editor: EditorId): Promise<void>;
  /** Calls `fn` with the task of a notification the person clicked; returns the unsubscribe. */
  onNoteClick?(fn: (taskId: string) => void): () => void;
}

/** `store` with every call guarded: a read that throws is nothing stored, a write that
 * throws is not remembered (a private window, blocked site data). */
export function safeStorage(store: Pick<Storage, "getItem" | "setItem"> | null): Pick<Storage, "getItem" | "setItem"> {
  return {
    getItem(k) {
      try {
        return store ? store.getItem(k) : null;
      } catch {
        return null;
      }
    },
    setItem(k, v) {
      try {
        store?.setItem(k, v);
      } catch {
        // Not remembered.
      }
    },
  };
}

export type PathKind = "dir" | "file" | "missing";

export const EnvContext = createContext<Env | null>(null);

export function useEnv(): Env {
  const env = useContext(EnvContext);
  if (!env) throw new Error("EnvContext missing");
  return env;
}

/** Arrays element by element, plain objects key by key (`Object.is` each). */
export function shallow<T>(a: T, b: T): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a);
  if (ka.length !== Object.keys(b).length) return false;
  const ob = b as Record<string, unknown>;
  return ka.every((k) => Object.hasOwn(ob, k) && Object.is((a as Record<string, unknown>)[k], ob[k]));
}

/** `useCockpitSelect` on the env's store: the slice `select` picks, re-rendering only when
 * it changes by `equal`. */
export function useSelect<T>(select: (s: State) => T, equal: (a: T, b: T) => boolean = Object.is): T {
  return useCockpitSelect(useEnv().cockpit, select, equal);
}

const selectBoot = (s: State) => s.bootId;

/** The daemon's boot id (null before the first snapshot): views that fetch on their own
 * fetch again when it changes (a restarted daemon may answer differently). */
export function useBoot(): string | null {
  return useSelect(selectBoot);
}

const selectLive = (s: State) => s.connection.state === "live";

/** Whether the daemon answers now: actions that need it are off while it does not. */
export function useLive(): boolean {
  return useSelect(selectLive);
}
