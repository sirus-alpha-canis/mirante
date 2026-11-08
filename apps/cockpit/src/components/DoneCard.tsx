import { useState } from "react";
import { useEnv } from "../env";
import type { ReactNode } from "react";
import type { TaskDiff } from "../lib/protocol";
import { ErrorRetry } from "./ErrorRetry";
import { diffTotals, useDiff } from "./useDiff";
import { tn, useT } from "../lib/i18n";

/** "1 file changed, +10 −2 vs main · 2 new untracked files". `against` names the base;
 * `quiet` drops "No changes …" when only untracked files are there (the card). */
export function DiffCounts({ diff, against, quiet }: { diff: TaskDiff; against: ReactNode; quiet?: boolean }) {
  const tr = useT();
  const t = diffTotals(diff);
  const untracked = t.untracked > 0 && tn(t.untracked, "diff.newUntrackedOne", "diff.newUntrackedMany");
  if (t.changed === 0) {
    if (untracked && quiet) return <>{untracked}</>;
    return (
      <>
        {tr("diff.noChanges")} {against}
        {untracked && <> · {untracked}</>}
      </>
    );
  }
  return (
    <>
      {tn(t.changed, "diff.filesOne", "diff.filesMany")} <span className="add">+{t.added}</span> <span className="del">−{t.removed}</span>{" "}
      {against}
      {untracked && <> · {untracked}</>}
    </>
  );
}

/** A task whose turn ended: what changed, and the way to a pull request. */
export function DoneCard({ taskId, version, onOpen }: { taskId: string; version: number; onOpen(): void }) {
  const { openUrl } = useEnv();
  const t = useT();
  const d = useDiff(taskId, version);
  const [openError, setOpenError] = useState(false);
  if (d.state === "loading") return <div className="done muted">{t("diff.reading")}</div>;
  if (d.state === "error")
    return (
      <div className="done">
        <ErrorRetry text={t("diff.unavailable", { error: d.error })} onRetry={d.retry} />
      </div>
    );
  const url = d.diff.compare_url;
  return (
    <div className="done" role="group" aria-label={t("diff.turnFinished")}>
      <span>
        <DiffCounts
          diff={d.diff}
          against={
            <>
              {t("diff.vs")} <code>{d.diff.base}</code>
            </>
          }
          quiet
        />
      </span>
      <span className="done-actions">
        <button className="btn" onClick={onOpen}>
          {t("diff.review")}
        </button>
        {url && (
          <button
            className="btn btn-primary"
            disabled={!d.diff.pushed}
            title={t(d.diff.pushed ? "diff.prTitle" : "diff.pushFirstTitle")}
            onClick={() => {
              setOpenError(false);
              openUrl(url).catch(() => setOpenError(true));
            }}
          >
            {t("diff.openPr")}
          </button>
        )}
        {openError && (
          <span className="error" role="alert">
            {t("diff.browserFailed")}
          </span>
        )}
      </span>
    </div>
  );
}
