// Small labels that tell the user how much an agent may do on its own.
import { t } from "./i18n";
import type { AgentKind } from "./protocol";

export interface Label {
  text: string;
  title: string;
}

export interface Scope extends Label {
  /** The accept button's words: what allowing grants. */
  allow: string;
}

/** Codex runs with `approvalPolicy: on-request` in a `workspace-write` sandbox (Plan 2b). */
export function agentModeLabel(agent: AgentKind): Label | null {
  return agent === "codex"
    ? { text: t("label.autoEdits"), title: t("label.autoEditsTitle") }
    : null;
}

/** The daemon's tool name for a Codex `grantRoot` file-change approval (driver/codex.rs). */
export const GRANT_ROOT_TOOL = "file_change (session-wide grant)";

/** A `grantRoot` approval lets Codex write under that folder for the rest of the session. */
export function permissionScope(tool: string): Scope {
  return tool === GRANT_ROOT_TOOL
    ? { text: t("label.sessionGrant"), title: t("label.sessionGrantTitle"), allow: t("label.allowSession") }
    : { text: t("label.once"), title: t("label.onceTitle"), allow: t("label.allowOnce") };
}
