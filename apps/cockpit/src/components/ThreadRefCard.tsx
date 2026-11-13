// A thread inside the conversation it came from: one compact card, never the thread's own
// events (its context stays its own). It reads the thread's live view, so it updates in
// place. Open goes to the thread; Continue resumes it when it ended and focuses its composer.
import { memo, useCallback, useState } from "react";
import { useEnv, useLive, useSelect } from "../env";
import { useT } from "../lib/i18n";
import type { ThreadLink } from "../lib/protocol";
import { continueThread, linkLabel } from "../lib/threads";
import type { State } from "../store/state";
import { useThreads } from "./threadContext";

export const ThreadRefCard = memo(function ThreadRefCard({
  id,
  title,
  link,
  moved,
}: {
  id: string;
  /** What the parent's event said (the thread may be gone from the list). */
  title: string;
  link?: ThreadLink;
  moved?: "moved" | "copied";
}) {
  const t = useT();
  const { rpc } = useEnv();
  const live = useLive();
  const api = useThreads();
  const thread = useSelect(useCallback((s: State) => s.tasks[id], [id]));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const shownLink = thread?.thread?.link ?? link;
  const label = shownLink ? linkLabel(shownLink) : t("thread.message");
  const onContinue = async () => {
    if (!thread || busy) return;
    setBusy(true);
    setError(null);
    try {
      await continueThread(rpc, thread);
      api?.open(id, true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <article className="thread-card" aria-label={t("thread.card", { label })} data-thread={id}>
      <span className="thread-card-mark" aria-hidden="true">
        ⑂
      </span>
      <span className="thread-card-body">
        {moved && <span className="small muted">{t(moved === "moved" ? "thread.movedTo" : "thread.continuedIn")} </span>}
        <span className="tag">{label}</span> <span className="thread-card-title">{thread?.title ?? title}</span>
      </span>
      {thread && api && (
        <span className="thread-card-actions">
          <button type="button" className="btn btn-small btn-ghost" onClick={() => api.open(id)}>
            {t("thread.open")}
          </button>
          {thread.lifecycle !== "archived" && (
            <button type="button" className="btn btn-small" disabled={busy || !live} onClick={() => void onContinue()}>
              {t("thread.continue")}
            </button>
          )}
        </span>
      )}
      {error && (
        <span className="error small" role="alert">
          {error}
        </span>
      )}
    </article>
  );
});
