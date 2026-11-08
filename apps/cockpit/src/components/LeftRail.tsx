import { useEffect, useMemo, useState } from "react";
import { useEnv, useStore } from "../env";
import { DaemonError } from "../lib/daemon";
import { dnaStatus } from "../lib/dna";
import { RpcCode } from "../lib/protocol";
import { skillProposals } from "../lib/skills";
import { useSkillsVersion } from "../lib/useSkills";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import { addRepo, expandHome, loadRepos, normaliseRepoPath, removeRepo } from "../lib/repos";
import type { Project } from "../store/select";
import { learnDnaKey, useDnaRev } from "../lib/useDna";
import { MemoryPanel } from "./MemoryPanel";

export function LeftRail(props: {
  projects: Project[];
  selected: string | null;
  onSelect(path: string): void;
  onReposChanged(repos: string[]): void;
  onMention(agent: AgentKind): void;
  /** Opens the memory review queue of a project. */
  onReview?(path: string): void;
  /** Opens the Skills view. */
  onSkills?(): void;
  status: DaemonStatus | null;
}) {
  const { storage, rpc, cockpit, pickFolder, homeDir, pathKind } = useEnv();
  const s = useStore();
  // The selected project's memory proposals waiting for a person (none when its memory is
  // off, the daemon is older, or the path is no repo).
  const sel = props.selected;
  const version = useDnaRev(sel);
  const [pending, setPending] = useState<{ path: string; n: number } | null>(null);
  useEffect(() => {
    if (!sel) return;
    let live = true;
    dnaStatus(rpc, sel)
      .then((r) => {
        if (r.available) learnDnaKey(cockpit, sel, r.status);
        if (live) setPending({ path: sel, n: r.available && r.status?.enabled ? (r.status.pending ?? 0) : 0 });
      })
      .catch(() => live && setPending({ path: sel, n: 0 }));
    return () => {
      live = false;
    };
  }, [rpc, cockpit, sel, version]);
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);
  const agents = useMemo(() => {
    const count = (a: AgentKind) => Object.values(s.tasks).filter((t) => t.agent === a && t.live).length;
    return [
      { agent: "claude" as const, live: count("claude") },
      { agent: "codex" as const, live: count("codex") },
    ];
  }, [s.tasks]);

  /** Adds a typed or picked folder: "~/…" expanded with the app's home, then it must be a
   * folder (whether it is a git checkout the daemon checks when a task is created there). */
  const add = async (raw: string, typed: boolean) => {
    try {
      const text = raw.trim();
      const expanded = text === "~" || text.startsWith("~/") ? expandHome(text, await homeDir()) : text;
      const n = normaliseRepoPath(expanded);
      if (!n.ok) return setError(n.error);
      const kind = await pathKind(n.path);
      if (kind === "missing") return setError(`${n.path} does not exist`);
      if (kind === "file") return setError(`${n.path} is a file, not a folder`);
      const r = addRepo(storage, n.path);
      if (!r.ok) return setError(r.error);
      setError(null);
      if (typed) setPath("");
      props.onReposChanged(r.repos);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  const choose = async () => {
    try {
      const picked = await pickFolder();
      if (picked) await add(picked, false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
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
          void add(path, true);
        }}
      >
        <label htmlFor="repo-path" className="sr-only">
          Repository path
        </label>
        <input id="repo-path" placeholder="/path/to/repo or ~/…" value={path} onChange={(e) => setPath(e.target.value)} />
        <button className="btn btn-small" type="button" onClick={() => void choose()} title="Pick a repository folder">
          Choose…
        </button>
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
      {props.onSkills && <SkillsEntry onOpen={props.onSkills} />}
      <MemoryPanel status={props.status} />
    </nav>
  );
}

/** "Skills (n to review)": skill proposals waiting for a person, across all clients. */
function SkillsEntry({ onOpen }: { onOpen(): void }) {
  const { rpc } = useEnv();
  const version = useSkillsVersion();
  const [pending, setPending] = useState(0);
  useEffect(() => {
    let live = true;
    skillProposals(rpc, { status: "pending", limit: 200 })
      .then((l) => live && setPending((l ?? []).length))
      .catch((e) => {
        // An older daemon has no skills; any other failure shows no count either.
        if (live && !(e instanceof DaemonError && e.code === RpcCode.METHOD_NOT_FOUND)) setPending(0);
      });
    return () => {
      live = false;
    };
  }, [rpc, version]);
  return (
    <>
      <h2>Skills</h2>
      <button className="rail-item" onClick={onOpen} title="Reviewed instructions Mirante gives matching sessions">
        {pending > 0 ? `Skills (${pending} to review)` : "Skills"}
      </button>
    </>
  );
}
