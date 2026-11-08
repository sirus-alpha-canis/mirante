import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { useEnv, useStore } from "../env";
import { M, type TaskView } from "../lib/protocol";
import { columnOf, COLUMN_LABEL, type Column, pendingPerms, turnsEnded } from "../store/select";
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { DiffTab } from "./DiffTab";
import { MessageComposer } from "./MessageComposer";
import { PermissionCard } from "./PermissionCard";
import { QueueList } from "./QueueList";
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

/** Interrupt / Stop / Resume: one at a time; Stop asks first; a new action clears the error. */
function Actions({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const [confirmStop, setConfirmStop] = useState(false);
  const inFlight = useRef(false);
  const [busy, setBusy] = useState(false);
  const run = async (method: string) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    setConfirmStop(false);
    try {
      await rpc(method, { task: t.id });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return (
    <span className="actions">
      {!t.live ? (
        <button className="btn btn-small" disabled={busy} onClick={() => run(M.sessionResume)}>
          Resume
        </button>
      ) : confirmStop ? (
        <span role="group" aria-label="Confirm stop">
          <span className="small">Stop the agent? The task stays; resume it later.</span>{" "}
          <button className="btn btn-small btn-warn" disabled={busy} onClick={() => run(M.sessionStop)}>
            Stop the session
          </button>
          <button className="btn btn-small" onClick={() => setConfirmStop(false)}>
            Cancel
          </button>
        </span>
      ) : (
        <>
          <button className="btn btn-small" disabled={busy} onClick={() => run(M.sessionInterrupt)}>
            Interrupt
          </button>
          <button className="btn btn-small" disabled={busy} onClick={() => setConfirmStop(true)}>
            Stop
          </button>
        </>
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
 * `initialTab` is the tab it opens on. `focusTerminalNonce`: each new value after mount
 * selects the Terminal tab again ("Answer in terminal" pressed twice, or from another view);
 * the value it mounts with is not a request (`initialTab` covers that). `inlineAnswers` is
 * `daemon.status.claude_inline_answers` (false when unknown). Per-task state (draft, terminal,
 * diff, queue, session actions) is keyed by task, so nothing follows a task switch.
 */
export function Detail({
  taskId,
  onSelect,
  initialTab,
  focusTerminalNonce,
  inlineAnswers = false,
}: {
  taskId: string;
  onSelect(id: string): void;
  initialTab?: Tab;
  focusTerminalNonce?: number;
  inlineAnswers?: boolean;
}) {
  const s = useStore();
  const [tab, setTab] = useState<Tab>(initialTab ?? "Steps");
  const t = s.tasks[taskId];
  const feed = s.feed[taskId] ?? [];
  const perms = useMemo(() => pendingPerms(s.perms, taskId), [s.perms, taskId]);
  const tabRefs = useRef(new Map<Tab, HTMLButtonElement>());
  const seenNonce = useRef(focusTerminalNonce);
  useEffect(() => {
    if (focusTerminalNonce === seenNonce.current) return;
    seenNonce.current = focusTerminalNonce;
    setTab("Terminal");
  }, [focusTerminalNonce]);
  const selectTab = (name: Tab) => {
    setTab(name);
    tabRefs.current.get(name)?.focus();
  };
  const onTabKey = (e: KeyboardEvent) => {
    const i = TABS.indexOf(tab);
    const next =
      e.key === "ArrowRight"
        ? TABS[(i + 1) % TABS.length]
        : e.key === "ArrowLeft"
          ? TABS[(i + TABS.length - 1) % TABS.length]
          : e.key === "Home"
            ? TABS[0]
            : e.key === "End"
              ? TABS[TABS.length - 1]
              : null;
    if (!next) return;
    e.preventDefault();
    selectTab(next);
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
            <Actions key={t.id} t={t} />
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
                ref={(el) => {
                  if (el) tabRefs.current.set(name, el);
                  else tabRefs.current.delete(name);
                }}
                aria-selected={tab === name}
                aria-controls="task-panel"
                tabIndex={tab === name ? 0 : -1}
                onClick={() => setTab(name)}
              >
                {name}
              </button>
            ))}
          </div>
          <section role="tabpanel" id="task-panel" aria-labelledby={`tab-${tab}`} className="panel">
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
            {tab === "Terminal" && <TerminalTab key={t.id} t={t} />}
            {tab === "Diff" && <DiffTab key={t.id} taskId={t.id} version={turnsEnded(feed)} />}
            {tab === "Memory" && (
              <p className="muted">
                Repo memory arrives with the memory layer (a later plan). Goal: <q>{t.goal}</q>
                {t.notes && <> · Notes: {t.notes}</>}
              </p>
            )}
          </section>
          {t.live && <QueueList key={t.id} t={t} />}
          <MessageComposer key={t.id} t={t} />
        </main>
      )}
    </div>
  );
}
