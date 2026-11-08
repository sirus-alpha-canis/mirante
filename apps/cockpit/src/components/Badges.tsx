import type { TaskView } from "../lib/protocol";
import { agentModeLabel } from "../lib/labels";
import { isHibernated } from "../lib/memguard";
import { type Column, COLUMN_LABEL } from "../store/select";

export function StatusPill({ column }: { column: Column }) {
  return <span className={`pill pill-${column}`}>{COLUMN_LABEL[column]}</span>;
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
      {isHibernated(t) && <span className="tag tag-muted">hibernated</span>}
      {t.queued > 0 && <span className="tag">{t.queued} queued</span>}
      {t.detail && <span className="tag tag-muted">{t.detail}</span>}
      {t.usage && <span className="tag tag-muted">{t.usage}</span>}
    </span>
  );
}
