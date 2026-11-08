// Small labels that tell the user how much an agent may do on its own.
import type { AgentKind } from "./protocol";

export interface Label {
  text: string;
  title: string;
}

/** Codex runs with `approvalPolicy: on-request` in a `workspace-write` sandbox (Plan 2b). */
export function agentModeLabel(agent: AgentKind): Label | null {
  return agent === "codex"
    ? {
        text: "auto-edits",
        title: "Codex edits files inside this worktree without asking. Commands outside the sandbox, network and other folders still ask you.",
      }
    : null;
}

/** The daemon's tool name for a Codex `grantRoot` file-change approval (driver/codex.rs). */
export const GRANT_ROOT_TOOL = "file_change (session-wide grant)";

/** A `grantRoot` approval lets Codex write under that folder for the rest of the session. */
export function permissionScope(tool: string): Label {
  return tool === GRANT_ROOT_TOOL
    ? { text: "session-wide grant", title: "Allowing lets the agent write under this folder until the session ends, not just once." }
    : { text: "once", title: "Allowing applies to this request only." };
}
