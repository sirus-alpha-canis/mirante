import { useMemo } from "react";
import { useStore } from "../env";
import type { TaskView } from "../lib/protocol";
import { columnOf, lastLines, pendingPerms, turnsEnded } from "../store/select";
import type { FeedItem } from "../store/state";
import { StatusPill, TaskBadges } from "./Badges";
import { DoneCard } from "./DoneCard";
import { PermissionCard } from "./PermissionCard";
import { TrustCard } from "./TrustCard";

export function FeedLine({ item }: { item: FeedItem }) {
  const who = item.kind === "message" ? (item.mode === "queue" ? "you · queued" : "you · steer") : null;
  return (
    <li className={`line line-${item.kind} ${item.ok === false ? "line-failed" : ""}`}>
      {who && <span className="who">{who}</span>}
      <span className="text">{item.text}</span>
    </li>
  );
}

/** One task in the project feed: the dispatch, live progress, and whatever needs the user.
 * `onOpen(id, "Terminal")` opens V1 on the task's Terminal tab (Claude cards answered there). */
export function AgentCard({
  t,
  onOpen,
  inlineAnswers = false,
}: {
  t: TaskView;
  onOpen(id: string, tab?: "Terminal"): void;
  /** `daemon.status.claude_inline_answers` (false when unknown). */
  inlineAnswers?: boolean;
}) {
  const s = useStore();
  const feed = s.feed[t.id];
  const col = columnOf(t, feed);
  const perms = useMemo(() => pendingPerms(s.perms, t.id), [s.perms, t.id]);
  // The task waits on the user but its request has not loaded (or is a dialog Mirante does
  // not model): the terminal is where it can be answered.
  const unexplained = t.status === "needs_you" && perms.length === 0 && t.boot !== "trust_prompt";
  const lines = lastLines(feed, 5);
  return (
    <article className={`card card-${col}`} aria-label={`${t.agent} on ${t.branch}`}>
      <p className="dispatch">
        <span className="who">you</span> @{t.agent} <code>{t.branch}</code> {t.goal}
      </p>
      <header className="card-head">
        <button className="link" onClick={() => onOpen(t.id)}>
          {t.title}
        </button>
        <TaskBadges t={t} />
        <StatusPill column={col} />
      </header>
      {lines.length > 0 && (
        <ol className="lines">
          {lines.map((i) => (
            <FeedLine key={i.seq} item={i} />
          ))}
        </ol>
      )}
      {t.boot === "trust_prompt" && <TrustCard taskId={t.id} />}
      {unexplained && (
        <p className="perm perm-compact">
          <span className="muted">Waiting for you in the agent's terminal.</span>{" "}
          <button className="btn btn-small" onClick={() => onOpen(t.id, "Terminal")}>
            Open terminal
          </button>
        </p>
      )}
      {perms.map((p) => (
        <PermissionCard
          key={p.id}
          perm={p}
          agent={t.agent}
          inlineAnswers={inlineAnswers}
          onAnswerInTerminal={() => onOpen(t.id, "Terminal")}
        />
      ))}
      {col === "review" && <DoneCard taskId={t.id} version={turnsEnded(feed)} onOpen={() => onOpen(t.id)} />}
    </article>
  );
}
