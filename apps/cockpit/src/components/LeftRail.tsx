import { useEffect, useMemo, useState } from "react";
import { useEnv, useStore } from "../env";
import { dnaStatus } from "../lib/dna";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import { addRepo, loadRepos, removeRepo } from "../lib/repos";
import type { Project } from "../store/select";
import { dnaRev } from "../store/state";
import { MemoryPanel } from "./MemoryPanel";

export function LeftRail(props: {
  projects: Project[];
  selected: string | null;
  onSelect(path: string): void;
  onReposChanged(repos: string[]): void;
  onMention(agent: AgentKind): void;
  /** Opens the memory review queue of a project. */
  onReview?(path: string): void;
  status: DaemonStatus | null;
}) {
  const { storage, rpc } = useEnv();
  const s = useStore();
  // The selected project's memory proposals waiting for a person (none when its memory is
  // off, the daemon is older, or the path is no repo).
  const sel = props.selected;
  const version = sel ? dnaRev(s, sel) : 0;
  const [pending, setPending] = useState<{ path: string; n: number } | null>(null);
  useEffect(() => {
    if (!sel) return;
    let live = true;
    dnaStatus(rpc, sel)
      .then((r) => live && setPending({ path: sel, n: r.available && r.status?.enabled ? (r.status.pending ?? 0) : 0 }))
      .catch(() => live && setPending({ path: sel, n: 0 }));
    return () => {
      live = false;
    };
  }, [rpc, sel, version]);
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);
  const agents = useMemo(() => {
    const count = (a: AgentKind) => Object.values(s.tasks).filter((t) => t.agent === a && t.live).length;
    return [
      { agent: "claude" as const, live: count("claude") },
      { agent: "codex" as const, live: count("codex") },
    ];
  }, [s.tasks]);

  const add = () => {
    const r = addRepo(storage, path);
    if (!r.ok) return setError(r.error);
    setError(null);
    setPath("");
    props.onReposChanged(r.repos);
  };
  // Only repos added here can be removed; a task's repo stays listed while the task exists.
  const added = new Set(loadRepos(storage));
  const remove = (p: string) => props.onReposChanged(removeRepo(storage, p));

  return (
    <nav className="rail" aria-label="Projects and agents">
      <h2>Projects</h2>
      <ul>
        {props.projects.map((p) => (
          <li key={p.path} className="rail-row">
            <button
              className={`rail-item ${props.selected === p.path ? "selected" : ""}`}
              aria-current={props.selected === p.path ? "page" : undefined}
              title={p.path}
              onClick={() => props.onSelect(p.path)}
            >
              <span>{p.name}</span>
              {p.needsYou > 0 && (
                <>
                  <span className="badge" aria-hidden="true">
                    {p.needsYou}
                  </span>
                  <span className="sr-only">, {p.needsYou} {p.needsYou === 1 ? "needs" : "need"} you</span>
                </>
              )}
            </button>
            {pending?.path === p.path && pending.n > 0 && (
              <button
                className="btn btn-small btn-ghost dna-pending"
                title={`Memory proposals in ${p.path} waiting for your review`}
                disabled={!props.onReview}
                onClick={() => props.onReview?.(p.path)}
              >
                {pending.n} to review
              </button>
            )}
            {added.has(p.path) && (
              <button
                className="btn btn-small btn-ghost"
                aria-label={`Remove ${p.name} from Projects`}
                title={`Remove ${p.path} from Projects (nothing on disk changes)`}
                onClick={() => remove(p.path)}
              >
                ×
              </button>
            )}
          </li>
        ))}
      </ul>
      <form
        className="add-repo"
        onSubmit={(e) => {
          e.preventDefault();
          add();
        }}
      >
        <label htmlFor="repo-path" className="sr-only">
          Repository path
        </label>
        <input id="repo-path" placeholder="/path/to/repo" value={path} onChange={(e) => setPath(e.target.value)} />
        <button className="btn btn-small" type="submit">
          Add
        </button>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </form>
      <h2>Agents</h2>
      <ul>
        {agents.map((a) => (
          <li key={a.agent}>
            <button className="rail-item" onClick={() => props.onMention(a.agent)} title={`Start a message to @${a.agent}`}>
              <span>@{a.agent}</span>
              <span className="muted small">{a.live} live</span>
            </button>
          </li>
        ))}
      </ul>
      <MemoryPanel status={props.status} />
    </nav>
  );
}
