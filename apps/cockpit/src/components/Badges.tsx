import type { TaskView } from "../lib/protocol";
import { agentModeLabel } from "../lib/labels";
import { isHibernated } from "../lib/memguard";
import { type Column, COLUMN_LABEL } from "../store/select";
import { SELF_APPROVING } from "../store/state";

export function StatusPill({ column }: { column: Column }) {
  return <span className={`pill pill-${column}`}>{COLUMN_LABEL[column]}</span>;
}

/** How to go back to manual mode (Mirante never presses keys in the agent's dialog for this). */
export const MANUAL_MODE_HINT =
  "In the task's Terminal tab, press Shift+Tab until the footer says \"manual mode on\" (Mirante does not switch it for you).";

/** Red when the live Claude session left manual mode: it approves its own actions. */
export function AutoModeBadge({ t }: { t: TaskView }) {
  const mode = t.live ? t.permission_mode : null;
  if (!mode || !SELF_APPROVING.has(mode)) return null;
  const edits = mode === "acceptEdits";
  return (
    <span
      className="tag tag-danger"
      title={`${edits ? "Claude edits files without asking you." : "Claude approves its own commands."} ${MANUAL_MODE_HINT}`}
    >
      {edits ? "accept-edits on" : "auto mode on"}
    </span>
  );
}

/** Agent, branch and the labels that say how much it may do alone. */
export function TaskBadges({ t }: { t: TaskView }) {
  const mode = agentModeLabel(t.agent);
  return (
    <span className="badges">
      <span className={`agent agent-${t.agent}`}>@{t.agent}</span>
      <code className="branch">{t.branch}</code>
      {mode && (
        <span className="tag" title={mode.title}>
          {mode.text}
        </span>
      )}
      <AutoModeBadge t={t} />
      {isHibernated(t) && <span className="tag tag-muted">hibernated</span>}
      {t.queued > 0 && <span className="tag">{t.queued} queued</span>}
      {t.detail && <span className="tag tag-muted">{t.detail}</span>}
      {t.usage && <span className="tag tag-muted">{t.usage}</span>}
    </span>
  );
}
