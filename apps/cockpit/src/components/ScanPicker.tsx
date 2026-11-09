import { useEffect, useId, useRef, useState } from "react";
import type { RepoScan } from "../lib/protocol";
import { tn, useT } from "../lib/i18n";
import { repoName } from "../store/select";

/**
 * The repositories found in a folder given to Add project, to pick from: main repositories
 * picked, linked worktrees not (a folder often holds many worktrees of one repository).
 * The keyboard goes to it when it opens; Escape or Cancel closes it.
 */
export function ScanPicker(props: { folder: string; scan: RepoScan; inProjects: Set<string>; onAdd(paths: string[]): void; onCancel(): void }) {
  const t = useT();
  const id = useId();
  const box = useRef<HTMLElement>(null);
  const { repos } = props.scan;
  const [picked, setPicked] = useState(() => new Set(repos.filter((r) => r.kind === "repo").map((r) => r.path)));
  useEffect(() => box.current?.focus(), []);
  const name = repoName(props.folder);
  const toggle = (path: string) =>
    setPicked((cur) => {
      const next = new Set(cur);
      if (!next.delete(path)) next.add(path);
      return next;
    });
  return (
    <section
      ref={box}
      className="scan-picker"
      aria-labelledby={id}
      tabIndex={-1}
      onKeyDown={(e) => {
        if (e.key !== "Escape") return;
        e.preventDefault();
        e.stopPropagation();
        props.onCancel();
      }}
    >
      <h3 id={id} className="small">
        {repos.length === 0 ? t("scan.none", { path: props.folder }) : tn(repos.length, "scan.foundOne", "scan.foundMany", { name })}
      </h3>
      {props.scan.truncated && <p className="small warn">{t("scan.truncated")}</p>}
      {repos.length > 0 && (
        <>
          <div className="scan-actions">
            <button type="button" className="btn btn-small btn-ghost" onClick={() => setPicked(new Set(repos.map((r) => r.path)))}>
              {t("scan.all")}
            </button>
            <button type="button" className="btn btn-small btn-ghost" onClick={() => setPicked(new Set())}>
              {t("scan.noneSelected")}
            </button>
          </div>
          <ul className="scan-list" aria-label={t("scan.list", { name })}>
            {repos.map((r) => (
              <li key={r.path}>
                <label className="small check" title={r.path}>
                  <input type="checkbox" checked={picked.has(r.path)} onChange={() => toggle(r.path)} /> {r.name}
                  {r.kind === "worktree" && <span className="muted"> {t("scan.worktreeOf", { name: r.main_repo ? repoName(r.main_repo) : "?" })}</span>}
                  {props.inProjects.has(r.path) && <span className="muted"> · {t("scan.inProjects")}</span>}
                </label>
              </li>
            ))}
          </ul>
        </>
      )}
      <div className="scan-actions">
        {repos.length > 0 && (
          <button type="button" className="btn btn-small" disabled={picked.size === 0} onClick={() => props.onAdd(repos.filter((r) => picked.has(r.path)).map((r) => r.path))}>
            {t("scan.add", { n: picked.size })}
          </button>
        )}
        <button type="button" className="btn btn-small btn-ghost" onClick={props.onCancel}>
          {t("ui.cancel")}
        </button>
      </div>
    </section>
  );
}
