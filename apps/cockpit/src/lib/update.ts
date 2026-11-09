// A running daemon older than the installed Mirante: the build `daemon.status` reports
// against what the daemon binary on disk says (`mirante-daemon --version`, asked by the
// shell), and the one action that fixes it (restart the daemon).
import { createContext, useContext } from "react";
import type { DaemonBinary, DaemonStatus } from "./protocol";

/** The daemon binary on disk names a build and the running daemon another one (or none:
 * it predates build ids). Unknown on either side is never "outdated". */
export function daemonOutdated(status: DaemonStatus | null, disk: DaemonBinary | null): boolean {
  if (!status || !disk?.build) return false;
  return status.build !== disk.build;
}

export interface Update {
  /** The running daemon is older than the binary on disk. */
  outdated: boolean;
  /** Sessions the restart ends come back on their own (the daemon's auto-resume). */
  autoResume: boolean;
  restarting: boolean;
  /** Why the last restart failed. */
  error: string | null;
  restart(): void;
}

export const NO_UPDATE: Update = { outdated: false, autoResume: true, restarting: false, error: null, restart: () => {} };

export const UpdateContext = createContext<Update>(NO_UPDATE);

export function useUpdate(): Update {
  return useContext(UpdateContext);
}
