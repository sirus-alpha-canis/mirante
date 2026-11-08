import { useState } from "react";
import { useEnv } from "../env";
import type { ReactNode } from "react";
import type { TaskDiff } from "../lib/protocol";
import { diffTotals, plural, useDiff } from "./useDiff";

/** "1 file changed, +10 −2 vs main · 2 new untracked files". `against` names the base;
 * `quiet` drops "No changes …" when only untracked files are there (the card). */
export function DiffCounts({ diff, against, quiet }: { diff: TaskDiff; against: ReactNode; quiet?: boolean }) {
  const t = diffTotals(diff);
  const untracked = t.untracked > 0 && plural(t.untracked, "new untracked file", "new untracked files");
  if (t.changed === 0) {
    if (untracked && quiet) return <>{untracked}</>;
    return (
      <>
        No changes {against}
        {untracked && <> · {untracked}</>}
      </>
    );
  }
  return (
    <>
      {plural(t.changed, "file", "files")} changed, <span className="add">+{t.added}</span> <span className="del">−{t.removed}</span>{" "}
      {against}
      {untracked && <> · {untracked}</>}
    </>
  );
}

/** A task whose turn ended: what changed, and the way to a pull request. */
export function DoneCard({ taskId, version, onOpen }: { taskId: string; version: number; onOpen(): void }) {
  const { openUrl } = useEnv();
  const d = useDiff(taskId, version);
  const [openError, setOpenError] = useState(false);
  if (d.state === "loading") return <div className="done muted">Reading the diff…</div>;
  if (d.state === "error") return <div className="done error">Diff unavailable: {d.error}</div>;
  const url = d.diff.compare_url;
  return (
    <div className="done" role="group" aria-label="Turn finished">
      <span>
        <DiffCounts
          diff={d.diff}
          against={
            <>
              vs <code>{d.diff.base}</code>
            </>
          }
          quiet
        />
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
            onClick={() => {
              setOpenError(false);
              openUrl(url).catch(() => setOpenError(true));
            }}
          >
            Open PR
          </button>
        )}
        {openError && (
          <span className="error" role="alert">
            could not open the browser
          </span>
        )}
      </span>
    </div>
  );
}
