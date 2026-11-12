import { memo, useMemo, useState } from "react";
import { useEnv } from "../env";
import { DiffCounts } from "./DoneCard";
import { ErrorRetry } from "./ErrorRetry";
import { useT } from "../lib/i18n";
import { useDiff } from "./useDiff";
import type { TaskDiff } from "../lib/protocol";

function lineClass(l: string): string {
  if (l.startsWith("+++") || l.startsWith("---") || l.startsWith("diff ") || l.startsWith("index ")) return "d-meta";
  if (l.startsWith("@@")) return "d-hunk";
  if (l.startsWith("+")) return "d-add";
  if (l.startsWith("-")) return "d-del";
  return "";
}

/** Up to 512 KiB of patch: split and rendered only when the patch itself changes. */
const Patch = memo(function Patch({ patch }: { patch: string }) {
  const t = useT();
  const lines = useMemo(
    () =>
      patch.split("\n").map((l, i) => (
        <div key={i} className={lineClass(l)}>
          {l || " "}
        </div>
      )),
    [patch],
  );
  return (
    <pre className="patch" aria-label={t("diff.patch")}>
      {lines}
    </pre>
  );
});

export function DiffTab({ taskId, version }: { taskId: string; version: number }) {
  const t = useT();
  const d = useDiff(taskId, version, true);
  if (d.state === "loading") return <p className="muted">{t("diff.reading")}</p>;
  if (d.state === "error") return <ErrorRetry text={t("diff.unavailable", { error: d.error })} onRetry={d.retry} />;
  const { diff } = d;
  // A task across several repos: one section per repo, each with its own pull request.
  if (diff.repos?.length) {
    return (
      <div className="diff diff-repos">
        <nav className="diff-repo-nav small" aria-label={t("detail.repos")}>
          {diff.repos.map((r) => (
            <a key={r.repo} href={`#diff-${encodeURIComponent(r.name)}`}>
              {r.name}
            </a>
          ))}
        </nav>
        {diff.repos.map((r) => (
          <section key={r.repo} id={`diff-${encodeURIComponent(r.name)}`} className="diff-repo" aria-label={r.name}>
            <h3 className="diff-repo-name" title={r.repo}>
              {r.name}
            </h3>
            <DiffBody diff={r} name={r.name} />
          </section>
        ))}
      </div>
    );
  }
  return (
    <div className="diff">
      <DiffBody diff={diff} />
    </div>
  );
}

/** One repo's changes: the counts, Open PR, the files and the patch. `name`: the repo's, in
 * a task across several (its Open PR says which). */
function DiffBody({ diff, name }: { diff: TaskDiff; name?: string }) {
  const { openUrl } = useEnv();
  const t = useT();
  const [openError, setOpenError] = useState(false);
  const openPr = (url: string) => {
    setOpenError(false);
    openUrl(url).catch(() => setOpenError(true));
  };
  return (
    <>
      <p>
        <DiffCounts
          diff={diff}
          against={
            <>
              {t("diff.against")} <code>{diff.base}</code> ({diff.merge_base.slice(0, 8)})
            </>
          }
        />
        {diff.compare_url && (
          <button
            className="btn btn-small"
            disabled={!diff.pushed}
            aria-label={name ? t("diff.openPrFor", { name }) : undefined}
            onClick={() => openPr(diff.compare_url!)}
          >
            {t(diff.pushed ? "diff.openPr" : "diff.openPrPush")}
          </button>
        )}
        {openError && (
          <span className="error" role="alert">
            {" "}
            {t("diff.browserFailed")}
          </span>
        )}
      </p>
      <ul className="diff-files">
        {diff.files.map((f) => (
          <li key={f.path}>
            <code>{f.path}</code>{" "}
            {f.added === null ? <span className="muted">{t("diff.binary")}</span> : <span className="add">+{f.added}</span>}{" "}
            {f.removed !== null && <span className="del">−{f.removed}</span>}
          </li>
        ))}
        {diff.untracked.map((p) => (
          <li key={p}>
            <code>{p}</code> <span className="muted">{t("diff.untracked")}</span>
          </li>
        ))}
      </ul>
      <Patch patch={diff.patch} />
      {diff.truncated && <p className="muted small">{t("diff.cut")}</p>}
    </>
  );
}
