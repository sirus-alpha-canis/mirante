import { useEnv } from "../env";
import { diffTotals, useDiff } from "./useDiff";

function lineClass(l: string): string {
  if (l.startsWith("+++") || l.startsWith("---") || l.startsWith("diff ") || l.startsWith("index ")) return "d-meta";
  if (l.startsWith("@@")) return "d-hunk";
  if (l.startsWith("+")) return "d-add";
  if (l.startsWith("-")) return "d-del";
  return "";
}

export function DiffTab({ taskId, version }: { taskId: string; version: number }) {
  const { openUrl } = useEnv();
  const d = useDiff(taskId, version);
  if (d.state === "loading") return <p className="muted">Reading the diff…</p>;
  if (d.state === "error") return <p className="error">Diff unavailable: {d.error}</p>;
  const { diff } = d;
  const t = diffTotals(diff);
  return (
    <div className="diff">
      <p>
        {t.files} files, <span className="add">+{t.added}</span> <span className="del">−{t.removed}</span> against{" "}
        <code>{diff.base}</code> ({diff.merge_base.slice(0, 8)})
        {diff.compare_url && (
          <button className="btn btn-small" disabled={!diff.pushed} onClick={() => openUrl(diff.compare_url!)}>
            {diff.pushed ? "Open PR" : "Open PR (push first)"}
          </button>
        )}
      </p>
      <ul className="diff-files">
        {diff.files.map((f) => (
          <li key={f.path}>
            <code>{f.path}</code>{" "}
            {f.added === null ? <span className="muted">binary</span> : <span className="add">+{f.added}</span>}{" "}
            {f.removed !== null && <span className="del">−{f.removed}</span>}
          </li>
        ))}
        {diff.untracked.map((p) => (
          <li key={p}>
            <code>{p}</code> <span className="muted">new, untracked</span>
          </li>
        ))}
      </ul>
      <pre className="patch" aria-label="Patch">
        {diff.patch.split("\n").map((l, i) => (
          <div key={i} className={lineClass(l)}>
            {l || " "}
          </div>
        ))}
      </pre>
      {diff.truncated && <p className="muted small">Patch cut at 512 KiB.</p>}
    </div>
  );
}
