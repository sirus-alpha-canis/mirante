import type { TaskView } from "../lib/protocol";
import { agentModeLabel } from "../lib/labels";
import { isHibernated } from "../lib/memguard";
import { type Column, columnLabel } from "../store/select";
import { t as tr, useT } from "../lib/i18n";
import { SELF_APPROVING } from "../store/state";

export function StatusPill({ column }: { column: Column }) {
  useT();
  return <span className={`pill pill-${column}`}>{columnLabel(column)}</span>;
}

/** How to go back to manual mode (Mirante never presses keys in the agent's dialog for this). */
export const manualModeHint = () => tr("badge.manualHint");

/** Red when the live Claude session left manual mode: it approves its own actions. */
export function AutoModeBadge({ t }: { t: TaskView }) {
  const tt = useT();
  const mode = t.live ? t.permission_mode : null;
  if (!mode || !SELF_APPROVING.has(mode)) return null;
  const edits = mode === "acceptEdits";
  return (
    <span
      className="tag tag-danger"
      title={`${tt(edits ? "badge.editsTitle" : "badge.autoTitle")} ${manualModeHint()}`}
    >
      {tt(edits ? "badge.editsOn" : "badge.autoOn")}
    </span>
  );
}

/** Agent, branch and the labels that say how much it may do alone. */
export function TaskBadges({ t }: { t: TaskView }) {
  const tt = useT();
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
      {isHibernated(t) && <span className="tag tag-muted">{tt("badge.hibernated")}</span>}
      {t.queued > 0 && <span className="tag">{tt("badge.queued", { n: t.queued })}</span>}
      {t.detail && <span className="tag tag-muted">{t.detail}</span>}
      {t.usage && <span className="tag tag-muted">{t.usage}</span>}
    </span>
  );
}
