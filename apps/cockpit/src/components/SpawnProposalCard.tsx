// A sub-task its parent's agent proposed through `task_spawn`, waiting for the person
// (founder decision 1: no budget, every proposal is a card). What the agent wrote (goal,
// title, why) is shown as text nodes, never markup. "Edit" lets the person change the goal,
// title, agent and (a workspace parent) which repos it runs on before "Create child"; the
// daemon then makes the child under its parent and starts it in manual mode, like every
// task: its permissions are still the person's. The agent can never approve its own.
import { memo, useId, useState } from "react";
import { useEnv } from "../env";
import { useT } from "../lib/i18n";
import type { AgentKind, SpawnEdits, SpawnProposalView, SpawnReviewed } from "../lib/protocol";
import { M } from "../lib/protocol";

const AGENTS: AgentKind[] = ["claude", "codex"];
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** The proposal's name: its title, else its goal's first line. */
export function proposalName(p: SpawnProposalView): string {
  const first = p.goal.split("\n").find((l) => l.trim()) ?? "";
  return (p.title ?? "").trim() || first.trim().slice(0, 80);
}

export const SpawnProposalCard = memo(function SpawnProposalCard(props: {
  p: SpawnProposalView;
  /** Decided: the caller reloads its list. */
  onDone(): void;
  onSelect(id: string): void;
}) {
  const { p } = props;
  const { rpc, cockpit } = useEnv();
  const t = useT();
  const id = useId();
  const [editing, setEditing] = useState(false);
  const [goal, setGoal] = useState(p.goal);
  const [title, setTitle] = useState(p.title ?? "");
  const [agent, setAgent] = useState<AgentKind>(p.agent);
  const repos = p.repos ?? [];
  const [picked, setPicked] = useState<string[]>(repos);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notStarted, setNotStarted] = useState<{ child: string; why: string } | null>(null);

  const edits = (): SpawnEdits | undefined => {
    if (!editing) return undefined;
    const e: SpawnEdits = {};
    if (goal !== p.goal) e.goal = goal;
    if (title.trim() && title !== (p.title ?? "")) e.title = title;
    if (agent !== p.agent) e.agent = agent;
    if (repos.length > 0 && picked.length !== repos.length) e.repos = repos.filter((r) => picked.includes(r));
    return Object.keys(e).length > 0 ? e : undefined;
  };

  const decide = async (accept: boolean) => {
    if (busy) return;
    if (accept && !goal.trim()) {
      setError(t("spawn.goalEmpty"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const e = accept ? edits() : undefined;
      const r = await rpc<SpawnReviewed>(M.taskSpawnReview, { id: p.id, accept, ...(e ? { edits: e } : {}) });
      if (r.child) cockpit.dispatch({ type: "task", view: r.child });
      if (r.child && r.start_error) {
        setNotStarted({ child: r.child.id, why: r.start_error });
        return;
      }
      props.onDone();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  const name = proposalName(p);
  return (
    <article className="card spawn-card" aria-label={t("spawn.card", { name })}>
      <header className="card-head">
        <span className="child-title">{name}</span>
        <span className="tag">{t("spawn.byAgent")}</span>
      </header>
      <div className="card-meta">
        <span className={`agent agent-${p.agent}`}>@{p.agent}</span>
      </div>
      {editing ? (
        <div className="spawn-edit">
          <label htmlFor={`${id}-goal`}>{t("spawn.goal")}</label>
          <textarea id={`${id}-goal`} rows={4} value={goal} onChange={(e) => setGoal(e.target.value)} />
          <label htmlFor={`${id}-title`}>{t("spawn.title")}</label>
          <input id={`${id}-title`} value={title} maxLength={120} onChange={(e) => setTitle(e.target.value)} />
          <label htmlFor={`${id}-agent`}>{t("spawn.agent")}</label>
          <select id={`${id}-agent`} value={agent} onChange={(e) => setAgent(e.target.value as AgentKind)}>
            {AGENTS.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>
          {repos.length > 0 && (
            <fieldset className="spawn-repos">
              <legend>{t("spawn.repos")}</legend>
              {repos.map((r, i) => (
                <label key={r} className="small">
                  <input
                    type="checkbox"
                    checked={picked.includes(r)}
                    // The primary repo stays: the child's origin (and tenant) comes from it.
                    disabled={i === 0}
                    onChange={(e) => setPicked((cur) => (e.target.checked ? [...cur, r] : cur.filter((x) => x !== r)))}
                  />{" "}
                  {r}
                </label>
              ))}
            </fieldset>
          )}
        </div>
      ) : (
        // Agent text: text nodes only.
        <p className="answer-text spawn-goal">{p.goal}</p>
      )}
      {p.why && (
        <p className="small muted">
          {t("spawn.why")} {p.why}
        </p>
      )}
      <p className="small muted">{t("spawn.manual")}</p>
      <div className="child-actions">
        <button type="button" className="btn btn-small btn-primary" disabled={busy} onClick={() => void decide(true)}>
          {t("spawn.create")}
        </button>
        {!editing && (
          <button type="button" className="btn btn-small btn-ghost" disabled={busy} onClick={() => setEditing(true)}>
            {t("spawn.edit")}
          </button>
        )}
        <button type="button" className="btn btn-small btn-ghost" disabled={busy} onClick={() => void decide(false)}>
          {t("spawn.reject")}
        </button>
      </div>
      {error && (
        <p className="error small" role="alert">
          {error}
        </p>
      )}
      {notStarted && (
        <p className="small" role="status">
          {t("spawn.notStarted", { why: notStarted.why })}{" "}
          <button type="button" className="btn btn-small" onClick={() => props.onSelect(notStarted.child)}>
            {t("spawn.openChild")}
          </button>
        </p>
      )}
    </article>
  );
});
