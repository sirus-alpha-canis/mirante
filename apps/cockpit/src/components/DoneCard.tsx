import { useEnv } from "../env";
import { diffTotals, useDiff } from "./useDiff";

/** A task whose turn ended: what changed, and the way to a pull request. */
export function DoneCard({ taskId, version, onOpen }: { taskId: string; version: number; onOpen(): void }) {
  const { openUrl } = useEnv();
  const d = useDiff(taskId, version);
  if (d.state === "loading") return <div className="done muted">Reading the diff…</div>;
  if (d.state === "error") return <div className="done error">Diff unavailable: {d.error}</div>;
  const t = diffTotals(d.diff);
  const url = d.diff.compare_url;
  return (
    <div className="done" role="group" aria-label="Turn finished">
      <span>
        {t.files} file{t.files === 1 ? "" : "s"} changed, <span className="add">+{t.added}</span>{" "}
        <span className="del">−{t.removed}</span> vs <code>{d.diff.base}</code>
      </span>
      <span className="done-actions">
        <button className="btn" onClick={onOpen}>
          Review diff
        </button>
        {url && (
          <button
            className="btn btn-primary"
            disabled={!d.diff.pushed}
            title={d.diff.pushed ? "Open GitHub's pull request page" : "Push the branch first (ask the agent, or git push)"}
            onClick={() => openUrl(url)}
          >
            Open PR
          </button>
        )}
      </span>
    </div>
  );
}
