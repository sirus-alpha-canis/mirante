import { memo } from "react";
import { shallow, useSelect } from "../env";
import { lastAnswer, stepFile } from "../lib/feed";
import { toWorktreeRel } from "../lib/paths";
import { agentModeLabel } from "../lib/labels";
import type { TaskView } from "../lib/protocol";
import { columnOf, pendingPerms, turnsEnded } from "../store/select";
import { feedText, type FeedItem } from "../store/state";
import { useT } from "../lib/i18n";
import { AutoModeBadge, StatusPill } from "./Badges";
import { DoneCard } from "./DoneCard";
import { LastAnswer } from "./LastAnswer";
import { PermissionCard } from "./PermissionCard";
import { ProjectChip } from "./ProjectChip";
import { SkillsCount } from "./SkillsChip";
import { TrustCard } from "./TrustCard";
import { usePane } from "./paneContext";
import { useThreads } from "./threadContext";
import { ThreadRefCard } from "./ThreadRefCard";

/** One feed line; `count` > 1 when identical lines in a row were folded into it. */
/** Inside a task's Detail: a Read/Edit step's file, opened in the side pane. */
function StepFileButton({ path }: { path: string }) {
  const pane = usePane();
  const t = useT();
  const task = useSelect((s) => (pane ? s.tasks[pane.taskId] : undefined));
  const rel = task ? toWorktreeRel(path, task) : null;
  if (!pane || !rel) return null;
  return (
    <button type="button" className="btn btn-small btn-ghost step-open" aria-label={t("step.openFile", { path: rel })} title={t("step.openFile", { path: rel })} onClick={() => pane.openFile(rel)}>
      ↗
    </button>
  );
}

export const FeedLine = memo(function FeedLine({ item, count = 1 }: { item: FeedItem; count?: number }) {
  const t = useT();
  const threads = useThreads();
  if (item.kind === "thread" && item.thread) {
    return (
      <li className="line line-thread">
        <ThreadRefCard id={item.thread} title={item.text} link={item.link} moved={item.moved} />
      </li>
    );
  }
  const who = item.kind === "message" ? t(item.mode === "queue" ? "card.youQueued" : "card.youSteer") : null;
  const file = stepFile(item);
  // Inside a task's Detail: a person's message or an agent's final answer may become a thread.
  const threadable = threads && (item.kind === "message" || (item.kind === "agent_message" && item.final !== false));
  return (
    <li className={`line line-${item.kind} ${item.ok === false ? "line-failed" : ""} ${item.warn ? "line-warn" : ""}`}>
      {who && <span className="who">{who}</span>}
      <span className="text">{feedText(item)}</span>
      {file && <StepFileButton path={file} />}
      {threadable && (
        <button type="button" className="btn btn-small btn-ghost line-thread-new" onClick={() => threads.newThreadFrom(item)}>
          {t("thread.newFrom")}
        </button>
      )}
      {count > 1 && (
        <span className="repeat" aria-label={t("card.times", { n: count })}>
          ×{count}
        </span>
      )}
    </li>
  );
});

/**
 * One task in Home's inbox (needs you, or ready for review): its project, title and agent,
 * the agent's last answer (two lines, expandable), and the one thing to do: answer the
 * request (Allow/Deny for Codex, Answer in terminal for Claude), trust the folder, open the
 * terminal, or review the diff. Raw steps stay in the task's detail.
 * `onOpen(id, "Terminal")` opens the task's Terminal tab (Claude answered there).
 */
export const AgentCard = memo(function AgentCard({
  t,
  onOpen,
  inlineAnswers = false,
}: {
  t: TaskView;
  onOpen(id: string, tab?: "Terminal"): void;
  /** `daemon.status.claude_inline_answers` (false when unknown). */
  inlineAnswers?: boolean;
}) {
  const tt = useT();
  // Its own task's slices only: another task's event does not render this card.
  const feed = useSelect((s) => s.feed[t.id]);
  const col = columnOf(t, feed);
  const perms = useSelect((s) => pendingPerms(s.perms, t.id), shallow);
  // The task waits on the user but its request has not loaded (or is a dialog Mirante does
  // not model): the terminal is where it can be answered.
  const unexplained = t.status === "needs_you" && perms.length === 0 && t.boot !== "trust_prompt";
  const answer = lastAnswer(feed);
  const mode = agentModeLabel(t.agent);
  return (
    <article className={`card card-${col}`} aria-label={tt("card.label", { agent: t.agent, branch: t.branch })} data-task={t.id}>
      <header className="card-head">
        <button className="link" onClick={() => onOpen(t.id)}>
          {t.title}
        </button>
        <StatusPill column={col} />
      </header>
      {t.group && (
        <p className="card-crumb small muted">
          <span aria-hidden="true">↳ </span>
          {tt("group.breadcrumb", { title: t.group.title })}
        </p>
      )}
      {/* Who and where, muted on one line. */}
      <div className="card-meta">
        <ProjectChip path={t.repo_path} />
        <span className={`agent agent-${t.agent}`}>@{t.agent}</span>
        {mode && (
          <span className="tag" title={mode.title}>
            {mode.text}
          </span>
        )}
        <AutoModeBadge t={t} />
        <SkillsCount task={t.id} />
      </div>
      {answer && <LastAnswer text={answer} />}
      {t.boot === "trust_prompt" && <TrustCard taskId={t.id} />}
      {unexplained && (
        <p className="perm perm-compact">
          <span className="muted">{tt("card.waitingTerminal")}</span>{" "}
          <button className="btn btn-small" onClick={() => onOpen(t.id, "Terminal")}>
            {tt("card.openTerminal")}
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
});
