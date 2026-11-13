// A thread's own Detail says what it is: the conversation it came from, what it is bound to
// (a link chip: GitHub and local dev pages open, anything else is copied), its notes (the
// side pane's editor) and whether its agent has them yet, and its summary (as text).
import { useCallback, useEffect, useState } from "react";
import { useEnv, useSelect } from "../env";
import { DaemonError } from "../lib/daemon";
import { useT } from "../lib/i18n";
import { openableLink } from "../lib/links";
import { M, RpcCode, type TaskView, type ThreadLink } from "../lib/protocol";
import { linkLabel } from "../lib/threads";
import type { State } from "../store/state";
import { usePane } from "./paneContext";

/** What the thread is bound to: opens through the app's opener rule, else "copy link". */
export function LinkChip({ link }: { link: ThreadLink }) {
  const { openUrl } = useEnv();
  const t = useT();
  const [note, setNote] = useState<string | null>(null);
  const label = linkLabel(link);
  let url: URL | null = null;
  try {
    url = link.url ? new URL(link.url) : null;
  } catch {
    url = null;
  }
  if (url && openableLink(url)) {
    const href = url.href;
    return (
      <button type="button" className="token link-chip" title={href} onClick={() => void openUrl(href).catch(() => setNote(t("thread.openFailed")))}>
        {label} ↗{note && <span className="small error"> {note}</span>}
      </button>
    );
  }
  const text = link.url ?? link.ref;
  return (
    <span className="token link-chip" title={text}>
      {label}
      {link.kind !== "message" && (
        <button
          type="button"
          className="btn btn-small btn-ghost"
          onClick={() =>
            void navigator.clipboard
              ?.writeText(text)
              .then(() => setNote(t("thread.copied")))
              .catch(() => setNote(text))
          }
        >
          {t("thread.copyLink")}
        </button>
      )}
      {note && <span className="small muted"> {note}</span>}
    </span>
  );
}

export function ThreadHeader({ t: task, onSelect }: { t: TaskView; onSelect(id: string): void }) {
  const t = useT();
  const pane = usePane();
  const parent = useSelect(useCallback((s: State) => (task.parent_id ? s.tasks[task.parent_id] : undefined), [task.parent_id]));
  const view = task.thread;
  if (!view) return null;
  return (
    <section className="thread-head" aria-label={t("thread.header")}>
      <span className="tag">{t("thread.tag")}</span>
      {task.parent_id && (
        <button type="button" className="link small" disabled={!parent} onClick={() => parent && onSelect(parent.id)}>
          ↳ {parent ? t("thread.from", { title: parent.title }) : t("thread.fromGone")}
        </button>
      )}
      <LinkChip link={view.link} />
      {pane && (
        <button type="button" className="btn btn-small btn-ghost" onClick={() => pane.openTarget({ k: "notes" })}>
          {t("thread.notes")}
        </button>
      )}
      <span className="small muted">{t(view.delivered ? "thread.delivered" : "thread.notDelivered")}</span>
      {view.summary && (
        // Agent-derived (or Mirante's own) text: a text node, never markup.
        <p className="thread-summary small">{view.summary}</p>
      )}
    </section>
  );
}

const bytes = (s: string) => new TextEncoder().encode(s).length;
const NOTES_MAX = 8 * 1024;

/** The side pane's notes editor for a thread: saved against the hash it was opened with; a
 * change made meanwhile (another window, the CLI) is a conflict the person resolves. */
export function ThreadNotes({ taskId }: { taskId: string }) {
  const { rpc, cockpit } = useEnv();
  const t = useT();
  const task = useSelect(useCallback((s: State) => s.tasks[taskId], [taskId]));
  const view = task?.thread;
  const [base, setBase] = useState(() => ({ hash: view?.notes_hash ?? "", notes: task?.notes ?? "" }));
  const [draft, setDraft] = useState(task?.notes ?? "");
  const [status, setStatus] = useState<{ kind: "saved" | "conflict" | "error"; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const dirty = draft !== base.notes;
  // Changed elsewhere while nothing is typed here: follow it.
  useEffect(() => {
    if (!view || dirty || view.notes_hash === base.hash) return;
    setBase({ hash: view.notes_hash, notes: task?.notes ?? "" });
    setDraft(task?.notes ?? "");
  }, [view, dirty, base.hash, task?.notes]);
  if (!task || !view) return <p className="muted small">{t("thread.notThread")}</p>;
  const size = bytes(draft);
  const save = async () => {
    if (busy || !dirty || size > NOTES_MAX) return;
    setBusy(true);
    setStatus(null);
    try {
      const v = await rpc<TaskView>(M.taskThread, { task: taskId, notes: draft, expect_hash: base.hash });
      cockpit.dispatch({ type: "task", view: v });
      setBase({ hash: v.thread?.notes_hash ?? "", notes: v.notes });
      setDraft(v.notes);
      setStatus({ kind: "saved", text: t("thread.notesSaved") });
    } catch (e) {
      if (e instanceof DaemonError && e.code === RpcCode.CONFLICT) setStatus({ kind: "conflict", text: t("thread.notesConflict") });
      else setStatus({ kind: "error", text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };
  const reload = () => {
    setBase({ hash: view.notes_hash, notes: task.notes });
    setDraft(task.notes);
    setStatus(null);
  };
  return (
    <form
      className="thread-notes"
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <label htmlFor={`notes-${taskId}`}>{t("thread.notesLabel")}</label>
      <p className="small muted">{t("thread.notesHint")}</p>
      <textarea id={`notes-${taskId}`} rows={12} value={draft} onChange={(e) => setDraft(e.target.value)} />
      <div className="thread-notes-foot">
        <span className={`small ${size > NOTES_MAX ? "error" : "muted"}`}>{t("thread.notesSize", { n: size, max: NOTES_MAX })}</span>
        <button type="submit" className="btn btn-small" disabled={busy || !dirty || size > NOTES_MAX}>
          {t("thread.notesSave")}
        </button>
      </div>
      {status && (
        <p className={`small ${status.kind === "saved" ? "muted" : "error"}`} role={status.kind === "saved" ? "status" : "alert"}>
          {status.text}{" "}
          {status.kind === "conflict" && (
            <button type="button" className="btn btn-small btn-ghost" onClick={reload}>
              {t("thread.notesReload")}
            </button>
          )}
        </p>
      )}
    </form>
  );
}
