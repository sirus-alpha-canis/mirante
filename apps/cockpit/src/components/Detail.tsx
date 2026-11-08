import { type KeyboardEvent, useMemo, useState } from "react";
import { useEnv, useStore } from "../env";
import { M, type TaskView } from "../lib/protocol";
import { columnOf, COLUMN_LABEL, type Column, pendingPerms, turnsEnded } from "../store/select";
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { DiffTab } from "./DiffTab";
import { MessageComposer } from "./MessageComposer";
import { PermissionCard } from "./PermissionCard";
import { TerminalTab } from "./TerminalTab";
import { TrustCard } from "./TrustCard";

const GROUPS: Column[] = ["needs_you", "working", "review", "idle"];
const TABS = ["Steps", "Terminal", "Diff", "Memory"] as const;
export type Tab = (typeof TABS)[number];

function TaskList({ selected, onSelect }: { selected: string; onSelect(id: string): void }) {
  const s = useStore();
  const groups = useMemo(
    () =>
      GROUPS.map((g) => ({
        g,
        tasks: Object.values(s.tasks)
          .filter((t) => columnOf(t, s.feed[t.id]) === g)
          .sort((a, b) => b.updated_at - a.updated_at),
      })).filter((x) => x.tasks.length > 0),
    [s],
  );
  return (
    <nav className="tasklist" aria-label="Tasks">
      {groups.map(({ g, tasks }) => (
        <section key={g}>
          <h2>{COLUMN_LABEL[g]}</h2>
          <ul>
            {tasks.map((t) => (
              <li key={t.id}>
                <button
                  className={`rail-item ${t.id === selected ? "selected" : ""}`}
                  aria-current={t.id === selected ? "page" : undefined}
                  onClick={() => onSelect(t.id)}
                >
                  <span className="chip-title">{t.title}</span>
                  <span className="muted small">
                    @{t.agent} · {t.branch}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </nav>
  );
}

function Actions({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const run = (method: string) => rpc(method, { task: t.id }).catch((e) => setError(e instanceof Error ? e.message : String(e)));
  return (
    <span className="actions">
      {t.live ? (
        <>
          <button className="btn btn-small" onClick={() => run(M.sessionInterrupt)}>
            Interrupt
          </button>
          <button className="btn btn-small" onClick={() => run(M.sessionStop)}>
            Stop
          </button>
        </>
      ) : (
        <button className="btn btn-small" onClick={() => run(M.sessionResume)}>
          Resume
        </button>
      )}
      {error && (
        <span className="error" role="alert">
          {error}
        </span>
      )}
    </span>
  );
}

/**
 * V1, the classic detail: tasks by state | one task with Steps / Terminal / Diff / Memory.
 * `initialTab` lets "Answer in terminal" land on the Terminal tab (mount with
 * `key={taskId + (initialTab ?? "")}` so a new request re-focuses). `inlineAnswers` is
 * `daemon.status.claude_inline_answers` (false when unknown).
 */
export function Detail({
  taskId,
  onSelect,
  initialTab,
  inlineAnswers = false,
}: {
  taskId: string;
  onSelect(id: string): void;
  initialTab?: Tab;
  inlineAnswers?: boolean;
}) {
  const s = useStore();
  const [tab, setTab] = useState<Tab>(initialTab ?? "Steps");
  const t = s.tasks[taskId];
  const feed = s.feed[taskId] ?? [];
  const perms = useMemo(() => pendingPerms(s.perms, taskId), [s.perms, taskId]);
  const onTabKey = (e: KeyboardEvent) => {
    const i = TABS.indexOf(tab);
    if (e.key === "ArrowRight") setTab(TABS[(i + 1) % TABS.length]);
    if (e.key === "ArrowLeft") setTab(TABS[(i + TABS.length - 1) % TABS.length]);
  };
  return (
    <div className="detail">
      <TaskList selected={taskId} onSelect={onSelect} />
      {!t ? (
        <main className="detail-main">
          <p className="muted">This task is gone.</p>
        </main>
      ) : (
        <main className="detail-main" aria-label={`Task ${t.title}`}>
          <header className="detail-head">
            <h1>{t.title}</h1>
            <TaskBadges t={t} />
            <StatusPill column={columnOf(t, feed)} />
            <Actions t={t} />
          </header>
          {t.boot === "trust_prompt" && <TrustCard taskId={t.id} />}
          {perms.map((p) => (
            <PermissionCard
              key={p.id}
              perm={p}
              agent={t.agent}
              inlineAnswers={inlineAnswers}
              onAnswerInTerminal={() => setTab("Terminal")}
            />
          ))}
          <div role="tablist" aria-label="Task views" className="tabs" onKeyDown={onTabKey}>
            {TABS.map((name) => (
              <button
                key={name}
                role="tab"
                id={`tab-${name}`}
                aria-selected={tab === name}
                aria-controls={`panel-${name}`}
                tabIndex={tab === name ? 0 : -1}
                onClick={() => setTab(name)}
              >
                {name}
              </button>
            ))}
          </div>
          <section role="tabpanel" id={`panel-${tab}`} aria-labelledby={`tab-${tab}`} className="panel">
            {tab === "Steps" &&
              (feed.length === 0 ? (
                <p className="muted">No steps yet.</p>
              ) : (
                <ol className="lines lines-full">
                  {feed.map((i) => (
                    <FeedLine key={i.seq} item={i} />
                  ))}
                </ol>
              ))}
            {tab === "Terminal" && <TerminalTab t={t} />}
            {tab === "Diff" && <DiffTab taskId={t.id} version={turnsEnded(feed)} />}
            {tab === "Memory" && (
              <p className="muted">
                Repo memory arrives with the memory layer (a later plan). Goal: <q>{t.goal}</q>
                {t.notes && <> · Notes: {t.notes}</>}
              </p>
            )}
          </section>
          <MessageComposer t={t} />
        </main>
      )}
    </div>
  );
}
