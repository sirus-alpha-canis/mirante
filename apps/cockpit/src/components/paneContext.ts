// The Detail's side pane, as anything inside a task's Detail may ask for it (open a file,
// open another target). Its own module so lines and terminals need nothing else of the pane.
import { createContext, useContext } from "react";
import type { PaneTarget } from "../lib/sidepane";

export interface PaneApi {
  taskId: string;
  openFile(path: string, line?: number): void;
  openTarget(target: PaneTarget): void;
}

export const PaneContext = createContext<PaneApi | null>(null);

/** The side pane of the task being shown, or null outside a task's Detail. */
export function usePane(): PaneApi | null {
  return useContext(PaneContext);
}
