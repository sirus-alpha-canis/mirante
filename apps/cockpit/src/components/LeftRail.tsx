import { useMemo, useState } from "react";
import { useEnv, useStore } from "../env";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import { addRepo } from "../lib/repos";
import type { Project } from "../store/select";

export function LeftRail(props: {
  projects: Project[];
  selected: string | null;
  onSelect(path: string): void;
  onReposChanged(repos: string[]): void;
  onMention(agent: AgentKind): void;
  status: DaemonStatus | null;
}) {
  const { storage } = useEnv();
  const s = useStore();
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

  return (
    <nav className="rail" aria-label="Projects and agents">
      <h2>Projects</h2>
      <ul>
        {props.projects.map((p) => (
          <li key={p.path}>
            <button
              className={`rail-item ${props.selected === p.path ? "selected" : ""}`}
              aria-current={props.selected === p.path ? "page" : undefined}
              title={p.path}
              onClick={() => props.onSelect(p.path)}
            >
              <span>{p.name}</span>
              {p.needsYou > 0 && (
                <span className="badge" aria-label={`${p.needsYou} need you`}>
                  {p.needsYou}
                </span>
              )}
            </button>
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
    </nav>
  );
}
