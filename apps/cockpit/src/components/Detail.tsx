import { type KeyboardEvent, memo, useEffect, useMemo, useRef, useState } from "react";
import { shallow, useEnv, useSelect } from "../env";
import { isHibernated } from "../lib/memguard";
import { M, type TaskView } from "../lib/protocol";
import type { FeedItem } from "../store/state";
import { useDnaKey, useDnaRev } from "../lib/useDna";
import { columnOf, COLUMN_LABEL, type Column, pendingPerms, turnsEnded } from "../store/select";
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { DiffTab } from "./DiffTab";
import { HibernateButton } from "./MemoryPanel";
import { atRiskCount, MemoryTab, useTaskMemory } from "./MemoryTab";
import { MessageComposer } from "./MessageComposer";
import { PermissionCard } from "./PermissionCard";
import { QueueList } from "./QueueList";
import { SkillsChip } from "./SkillsChip";
import { TerminalTab } from "./TerminalTab";
import { TrustCard } from "./TrustCard";

const GROUPS: Column[] = ["needs_you", "working", "review", "idle"];
const TABS = ["Steps", "Terminal", "Diff", "Memory"] as const;
export type Tab = (typeof TABS)[number];

const NO_FEED: FeedItem[] = [];

const TaskList = memo(function TaskList({ selected, onSelect }: { selected: string; onSelect(id: string): void }) {
  const tasks = useSelect((s) => s.tasks);
  // Each task's column: renders when one changes, not on every event.
  const columns = useSelect((s) => Object.fromEntries(Object.values(s.tasks).map((t) => [t.id, columnOf(t, s.feed[t.id])])), shallow);
  const groups = useMemo(
    () =>
      GROUPS.map((g) => ({
        g,
        tasks: Object.values(tasks)
          .filter((t) => columns[t.id] === g)
          .sort((a, b) => b.updated_at - a.updated_at),
      })).filter((x) => x.tasks.length > 0),
    [tasks, columns],
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
});

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
        // A hibernated session is resumed by Wake (below), which keeps the memory guard's mark.
        !isHibernated(t) && (
          <button className="btn btn-small" disabled={busy} onClick={() => run(M.sessionResume)}>
            Resume
          </button>
        )
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
      <HibernateButton t={t} />
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
 * diff, queue, session actions) is keyed by task, so nothing follows a task switch. The
 * terminal stays mounted (and attached) on the other tabs; on its own tab, requests and the
 * queue float over it, so their coming and going does not resize the agent's PTY.
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
  const [tab, setTab] = useState<Tab>(initialTab ?? "Steps");
  // This task's slices only: another task's event does not render the detail.
  const t = useSelect((s) => s.tasks[taskId]);
  const feed = useSelect((s) => s.feed[taskId]) ?? NO_FEED;
  const perms = useSelect((s) => pendingPerms(s.perms, taskId), shallow);
  // Repo memory: refetched when the repo's memory changes or a turn ends (the branch moved).
  useDnaKey(t?.repo_path ?? null);
  const dnaVersion = useDnaRev(t?.repo_path ?? null);
  const memory = useTaskMemory(taskId, dnaVersion + turnsEnded(feed));
  const atRisk = atRiskCount(memory.mem);
  const tabLabel = (name: Tab) => (name === "Memory" && atRisk > 0 ? `Memory (${atRisk} at risk)` : name);
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
  const onTerminal = tab === "Terminal";
  const notices = t && (
    <>
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
    </>
  );
  // Its own key: it moves between the overlay and the column, beside the composer's `t.id`.
  const queue = t?.live && <QueueList key={`queue-${t.id}`} t={t} />;
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
            <SkillsChip key={`skills-${t.id}`} task={t.id} />
            <Actions key={t.id} t={t} />
          </header>
          {!onTerminal && notices}
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
                {tabLabel(name)}
              </button>
            ))}
          </div>
          <div className="panel-wrap">
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
              {/* Mounted on every tab (hidden off its own): coming back to it is no new
                  terminal, attach or repaint. */}
              <div className="terminal-slot" hidden={!onTerminal}>
                <TerminalTab key={t.id} t={t} active={onTerminal} />
              </div>
              {tab === "Diff" && <DiffTab key={t.id} taskId={t.id} version={turnsEnded(feed)} />}
              {tab === "Memory" && <MemoryTab key={t.id} t={t} mem={memory.mem} reload={memory.reload} version={dnaVersion} />}
            </section>
            {/* Over the terminal, not above or below it: a card or the queue coming and
                going must not resize the terminal (each resize makes the agent redraw). */}
            {onTerminal && (
              <div className="term-overlay">
                {notices}
                {queue}
              </div>
            )}
          </div>
          {!onTerminal && queue}
          <MessageComposer key={t.id} t={t} />
        </main>
      )}
    </div>
  );
}
