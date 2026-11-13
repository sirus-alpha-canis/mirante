// "New thread from this": a person's message or an agent's answer becomes its own thread
// (its own task, context and notes), bound to a pasted PR / issue / branch URL or to the
// message itself. A message still waiting in the queue moves (this conversation never gets
// it); anything delivered is copied, quoted into the thread's notes. The thread starts on a
// new branch from this task's work, or on an existing branch (a PR's head) checked out as is.
import { useId, useMemo, useRef, useState } from "react";
import { useEnv, useLive, useSelect } from "../env";
import { useT } from "../lib/i18n";
import type { AgentKind, TaskView } from "../lib/protocol";
import { linkLabel, parseLink, planFor, startThread, type Started } from "../lib/threads";
import type { FeedItem } from "../store/state";

export function NewThreadDialog({
  parent,
  item,
  onClose,
  onStarted,
}: {
  parent: TaskView;
  item: FeedItem;
  onClose(): void;
  onStarted(s: Started): void;
}) {
  const { rpc, cockpit } = useEnv();
  const live = useLive();
  const t = useT();
  const id = useId();
  const queue = useSelect((s) => s.queues[parent.id]);
  const plan = useMemo(() => planFor(item, queue), [item, queue]);
  const [goal, setGoal] = useState(item.kind === "message" ? item.text : "");
  const [linkText, setLinkText] = useState("");
  const [agent, setAgent] = useState<AgentKind>(parent.agent);
  const [existing, setExisting] = useState(false);
  const [branch, setBranch] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);
  const parsed = parseLink(linkText);
  const ready = !!goal.trim() && (!existing || !!branch.trim()) && live && !busy;
  const submit = async () => {
    if (!ready || inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      const s = await startThread(rpc, { parent, item, goal, agent, linkText, branch: existing ? branch : "" }, queue);
      // Known before its event arrives: its Detail opens on it.
      cockpit.dispatch({ type: "task", view: s.task });
      onStarted(s);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return (
    <div className="overlay" onMouseDown={onClose}>
      <form
        className="palette thread-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${id}-title`}
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            onClose();
          }
        }}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <h2 id={`${id}-title`}>{t("thread.dialogTitle")}</h2>
        <p className="small muted">{t(plan.mode === "moved" ? "thread.moveNote" : "thread.copyNote")}</p>
        <label htmlFor={`${id}-goal`}>{t("thread.goal")}</label>
        <textarea id={`${id}-goal`} rows={3} value={goal} placeholder={t("thread.goalPlaceholder")} onChange={(e) => setGoal(e.target.value)} autoFocus />
        <label htmlFor={`${id}-link`}>{t("thread.link")}</label>
        <input id={`${id}-link`} value={linkText} placeholder={t("thread.linkPlaceholder")} onChange={(e) => setLinkText(e.target.value)} />
        <p className="small muted" role="status">
          {parsed ? t("thread.linkParsed", { label: linkLabel(parsed) }) : linkText.trim() ? t("thread.linkUnknown") : t("thread.linkMessage")}
        </p>
        <label htmlFor={`${id}-agent`}>{t("thread.agent")}</label>
        <select id={`${id}-agent`} value={agent} onChange={(e) => setAgent(e.target.value as AgentKind)}>
          <option value="claude">claude</option>
          <option value="codex">codex</option>
        </select>
        <fieldset>
          <legend>{t("thread.branchLegend")}</legend>
          <label>
            <input type="radio" name={`${id}-branch`} checked={!existing} onChange={() => setExisting(false)} /> {t("thread.branchFrom", { branch: parent.branch })}
          </label>
          <label>
            <input type="radio" name={`${id}-branch`} checked={existing} onChange={() => setExisting(true)} /> {t("thread.branchExisting")}
          </label>
          {existing && (
            <input aria-label={t("thread.branchName")} value={branch} placeholder={parsed?.kind === "branch" ? parsed.ref : "fix/login"} onChange={(e) => setBranch(e.target.value)} />
          )}
        </fieldset>
        {error && (
          <p className="error small" role="alert">
            {error}
          </p>
        )}
        <div className="thread-dialog-actions">
          <button type="button" className="btn btn-small btn-ghost" onClick={onClose}>
            {t("thread.cancel")}
          </button>
          <button type="submit" className="btn btn-small" disabled={!ready}>
            {busy ? t("thread.starting") : t("thread.start")}
          </button>
        </div>
      </form>
    </div>
  );
}
