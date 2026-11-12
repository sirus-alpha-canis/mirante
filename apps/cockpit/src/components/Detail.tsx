import { type KeyboardEvent, memo, type PointerEvent, useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { shallow, useEnv, useLive, useSelect } from "../env";
import { isHibernated } from "../lib/memguard";
import { collapseRepeats, summarizeSteps } from "../lib/feed";
import { inboxOf } from "../lib/inbox";
import { type Key, useLang, useT } from "../lib/i18n";
import { M, type TaskView } from "../lib/protocol";
import type { FeedItem, State } from "../store/state";
import { useDnaKey, useDnaRev } from "../lib/useDna";
import { columnLabel, columnOf, type Column, pendingPerms, repoName, turnsEnded } from "../store/select";
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { DiffTab } from "./DiffTab";
import { ErrorRetry } from "./ErrorRetry";
import { HibernateButton } from "./MemoryPanel";
import { atRiskCount, MemoryTab, useTaskMemory } from "./MemoryTab";
import { MessageComposer } from "./MessageComposer";
import { PermissionCard } from "./PermissionCard";
import { ProjectChip } from "./ProjectChip";
import { QueueList } from "./QueueList";
import { SkillsChip } from "./SkillsChip";
import { TerminalTab } from "./TerminalTab";
import { TrustCard } from "./TrustCard";
import { onMac, shortcutFor } from "../lib/shortcuts";
import { clampWidth, loadPane, type PaneState, type PaneTarget, PANE_MAX, PANE_MIN, savePane } from "../lib/sidepane";
import { type PaneApi, PaneContext, SidePane } from "./SidePane";

const GROUPS: Column[] = ["needs_you", "working", "review", "idle"];
const TABS = ["Steps", "Terminal", "Diff", "Memory"] as const;
export type Tab = (typeof TABS)[number];

const NO_FEED: FeedItem[] = [];

/** The task list's filters: one column, or everything not archived. */
type Filter = "needs_you" | "working" | "review" | "all";
const FILTERS: Filter[] = ["needs_you", "working", "review", "all"];
const FILTER_KEY: Record<Filter, Key> = {
  needs_you: "detail.filter.needs_you",
  working: "detail.filter.working",
  review: "detail.filter.review",
  all: "detail.filter.all",
};
const TAB_KEY: Record<Tab, Key> = { Steps: "detail.tab.Steps", Terminal: "detail.tab.Terminal", Diff: "detail.tab.Diff", Memory: "detail.tab.Memory" };

/** Every task's column, as one map (the inbox's, computed once per change): renders when
 * one changes, not on every event. */
const selectColumns = (s: State) => inboxOf(s).columns;

const matches = (t: TaskView, q: string) =>
  !q || [t.title, t.branch, t.agent, repoName(t.repo_path)].some((x) => x.toLowerCase().includes(q));

/** The left list: by project, then by state; filter chips and a search. */
const TaskList = memo(function TaskList({ selected, onSelect }: { selected: string; onSelect(id: string): void }) {
  const tt = useT();
  const tasks = useSelect((s) => s.tasks);
  const columns = useSelect(selectColumns, shallow);
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const q = query.trim().toLowerCase();
  const counts = useMemo(() => {
    const c: Record<Filter, number> = { needs_you: 0, working: 0, review: 0, all: 0 };
    for (const t of Object.values(tasks)) {
      const col = columns[t.id];
      if (!col || col === "archived") continue;
      c.all++;
      if (col === "needs_you" || col === "working" || col === "review") c[col]++;
    }
    return c;
  }, [tasks, columns]);
  const projects = useMemo(() => {
    const by = new Map<string, Map<Column, TaskView[]>>();
    for (const t of Object.values(tasks)) {
      const col = columns[t.id];
      if (!col || col === "archived" || (filter !== "all" && col !== filter) || !matches(t, q)) continue;
      const p = by.get(t.repo_path) ?? new Map<Column, TaskView[]>();
      p.set(col, [...(p.get(col) ?? []), t]);
      by.set(t.repo_path, p);
    }
    return [...by.entries()]
      .sort(([a], [b]) => repoName(a).localeCompare(repoName(b)) || a.localeCompare(b))
      .map(([path, cols]) => ({
        path,
        groups: GROUPS.filter((g) => cols.has(g)).map((g) => ({ g, tasks: cols.get(g)!.sort((a, b) => b.updated_at - a.updated_at) })),
      }));
  }, [tasks, columns, filter, q]);
  return (
    <nav className="tasklist" aria-label={tt("detail.tasks")}>
      <div className="tasklist-tools">
        <label htmlFor="task-search" className="sr-only">
          {tt("detail.search")}
        </label>
        <input id="task-search" type="search" placeholder={tt("detail.search")} value={query} onChange={(e) => setQuery(e.target.value)} />
        <div className="filters" role="group" aria-label={tt("detail.filterTasks")}>
          {FILTERS.map((f) => (
            <button key={f} type="button" className="token" aria-pressed={filter === f} onClick={() => setFilter(f)}>
              {tt(FILTER_KEY[f])} ({counts[f]})
            </button>
          ))}
        </div>
      </div>
      {projects.length === 0 && <p className="muted small">{tt("detail.noMatch")}</p>}
      {projects.map(({ path, groups }) => (
        <section key={path} aria-label={repoName(path)} className="tasklist-project">
          <h2>
            <ProjectChip path={path} />
          </h2>
          {groups.map(({ g, tasks }) => (
            <div key={g}>
              <h3>{columnLabel(g)}</h3>
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
            </div>
          ))}
        </section>
      ))}
    </nav>
  );
});

/** The Steps tab: runs of tool steps summed up ("read 14 files, ran 3 commands",
 * expandable to their lines), identical lines folded with ×N. A run stays open while any of
 * its lines is in the feed (its first may fall off the store's window). While scrolled to
 * the bottom, new lines keep it there. */
const Steps = memo(function Steps({ feed }: { feed: FeedItem[] }) {
  // Run summaries are words: said again when the language changes.
  const lang = useLang();
  const entries = useMemo(() => summarizeSteps(feed), [feed, lang]);
  const [open, setOpen] = useState<ReadonlySet<number>>(() => new Set());
  const list = useRef<HTMLOListElement>(null);
  const atBottom = useRef(true);
  useEffect(() => {
    const pane = list.current?.closest<HTMLElement>(".panel");
    if (!pane) return;
    const onScroll = () => {
      atBottom.current = pane.scrollHeight - pane.scrollTop - pane.clientHeight < 24;
    };
    pane.addEventListener("scroll", onScroll);
    return () => pane.removeEventListener("scroll", onScroll);
  }, []);
  useLayoutEffect(() => {
    const pane = list.current?.closest<HTMLElement>(".panel");
    if (pane && atBottom.current) pane.scrollTop = pane.scrollHeight;
  }, [feed]);
  const toggle = (items: FeedItem[], isOpen: boolean) =>
    setOpen((cur) => {
      const next = new Set(cur);
      for (const i of items) {
        if (isOpen) next.delete(i.seq);
        else next.add(i.seq);
      }
      return next;
    });
  return (
    <ol className="lines lines-full" ref={list}>
      {entries.map((e, n) => {
        if (e.type === "line") return <FeedLine key={e.item.seq} item={e.item} count={e.count} />;
        const isOpen = e.items.some((i) => open.has(i.seq));
        // Keyed by position among runs: a run whose first line fell off keeps its row.
        const runIndex = entries.slice(0, n).filter((x) => x.type === "run").length;
        return <StepRun key={`run-${runIndex}`} items={e.items} text={e.text} open={isOpen} onToggle={() => toggle(e.items, isOpen)} />;
      })}
    </ol>
  );
});

function StepRun({ items, text, open, onToggle }: { items: FeedItem[]; text: string; open: boolean; onToggle(): void }) {
  const id = useId();
  return (
    <li className="line line-run">
      <button type="button" className="btn btn-small btn-ghost" aria-expanded={open} aria-controls={id} onClick={onToggle}>
        <span aria-hidden="true">{open ? "▾" : "▸"}</span> {text}
      </button>
      {open && (
        <ol id={id} className="lines run-lines">
          {collapseRepeats(items).map((f) => (
            <FeedLine key={f.item.seq} item={f.item} count={f.count} />
          ))}
        </ol>
      )}
    </li>
  );
}

/** Steps, or why the history is missing (with Retry). */
function StepsPanel({ taskId, feed }: { taskId: string; feed: FeedItem[] }) {
  const { cockpit } = useEnv();
  const t = useT();
  const error = useSelect((s) => s.historyErrors[taskId]);
  return (
    <>
      {error && <ErrorRetry text={t("detail.historyFailed", { error })} onRetry={() => void cockpit.loadHistory(taskId)} />}
      {feed.length === 0 ? !error && <p className="muted">{t("detail.noSteps")}</p> : <Steps feed={feed} />}
    </>
  );
}

/** Interrupt / Stop / Resume: one at a time; Stop asks first; a new action clears the error. */
function Actions({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const tt = useT();
  // Off while the daemon is away (nothing could carry them out).
  const live = useLive();
  const [error, setError] = useState<string | null>(null);
  const [confirmStop, setConfirmStop] = useState(false);
  const inFlight = useRef(false);
  const [state, setBusy] = useState(false);
  const busy = state || !live;
  const run = async (method: string) => {
    if (inFlight.current || !live) return;
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
            {tt("detail.resume")}
          </button>
        )
      ) : confirmStop ? (
        <span role="group" aria-label={tt("detail.confirmStop")}>
          <span className="small">{tt("detail.stopQuestion")}</span>{" "}
          <button className="btn btn-small btn-warn" disabled={busy} onClick={() => run(M.sessionStop)}>
            {tt("detail.stopSession")}
          </button>
          <button className="btn btn-small" onClick={() => setConfirmStop(false)}>
            {tt("ui.cancel")}
          </button>
        </span>
      ) : (
        <>
          <button className="btn btn-small" disabled={busy} onClick={() => run(M.sessionInterrupt)}>
            {tt("detail.interrupt")}
          </button>
          <button className="btn btn-small" disabled={busy} onClick={() => setConfirmStop(true)}>
            {tt("detail.stop")}
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

/** The side pane of `taskId`, loaded from storage when the task changes and saved on
 * every change. */
function usePaneState(taskId: string): [PaneState, (update: (p: PaneState) => PaneState) => void] {
  const { storage } = useEnv();
  const load = (id: string) => loadPane(storage, id, () => true);
  const [st, setSt] = useState(() => ({ id: taskId, pane: load(taskId) }));
  let current = st;
  if (st.id !== taskId) {
    current = { id: taskId, pane: load(taskId) };
    setSt(current);
  }
  const ref = useRef(current);
  ref.current = current;
  const update = useCallback(
    (fn: (p: PaneState) => PaneState) => {
      const { id, pane } = ref.current;
      const next = fn(pane);
      if (next === pane) return;
      ref.current = { id, pane: next };
      setSt(ref.current);
      savePane(storage, id, next);
    },
    [storage],
  );
  return [current.pane, update];
}

/** The separator between the main view and the pane: drag it, or ←/→ by 2%. */
function PaneSeparator({ width, swapped, body, onWidth }: { width: number; swapped: boolean; body: React.RefObject<HTMLDivElement | null>; onWidth(w: number): void }) {
  const tt = useT();
  const dragging = useRef(false);
  const onKey = (e: KeyboardEvent) => {
    const grow = e.key === "ArrowLeft" ? 2 : e.key === "ArrowRight" ? -2 : 0;
    if (!grow) return;
    e.preventDefault();
    onWidth(clampWidth(width + (swapped ? -grow : grow)));
  };
  const move = (e: PointerEvent) => {
    const r = body.current?.getBoundingClientRect();
    if (!dragging.current || !r || r.width <= 0) return;
    onWidth(clampWidth(((swapped ? e.clientX - r.left : r.right - e.clientX) / r.width) * 100));
  };
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={tt("pane.resize")}
      aria-valuenow={width}
      aria-valuemin={PANE_MIN}
      aria-valuemax={PANE_MAX}
      tabIndex={0}
      className="pane-sep"
      onKeyDown={onKey}
      onPointerDown={(e) => {
        dragging.current = true;
        e.currentTarget.setPointerCapture?.(e.pointerId);
      }}
      onPointerMove={move}
      onPointerUp={() => {
        dragging.current = false;
      }}
    />
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
  onTab,
  onRules,
}: {
  taskId: string;
  onSelect(id: string): void;
  initialTab?: Tab;
  focusTerminalNonce?: number;
  inlineAnswers?: boolean;
  /** Told the selected tab (the app remembers it for its next start). */
  onTab?(tab: Tab): void;
  /** "Rules this agent received": the Rules view, for this task. */
  onRules?(id: string): void;
}) {
  const [tab, setTab] = useState<Tab>(initialTab ?? "Steps");
  useEffect(() => onTab?.(tab), [tab, onTab]);
  const [pane, setPane] = usePaneState(taskId);
  const body = useRef<HTMLDivElement>(null);
  const openTarget = useCallback(
    (target: PaneTarget) => {
      // The task's own terminal is the main view, never the pane's.
      if (target.k === "terminal" && target.task === taskId) return setTab("Terminal");
      setPane((p) => ({ ...p, open: true, target }));
    },
    [taskId, setPane],
  );
  const paneApi = useMemo<PaneApi>(
    () => ({ taskId, openTarget, openFile: (path, line) => openTarget(line ? { k: "file", path, line } : { k: "file", path }) }),
    [taskId, openTarget],
  );
  const togglePane = useCallback(() => setPane((p) => ({ ...p, open: !p.open })), [setPane]);
  // ⌘\ (the pane) is this view's chord; the app leaves it here. Off macOS the terminal keeps
  // Ctrl chords, as with the app's own.
  const keys = useRef({ togglePane });
  keys.current = { togglePane };
  useEffect(() => {
    const mac = onMac();
    const onKey = (e: globalThis.KeyboardEvent) => {
      const a = shortcutFor(e, mac);
      if (a !== "pane") return;
      if (!mac && e.target instanceof Element && e.target.closest(".xterm")) return;
      // A modal (the command bar) owns the keys while open.
      if (document.querySelector('[aria-modal="true"]')) return;
      e.preventDefault();
      keys.current.togglePane();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  // Before the first snapshot nothing is known: "loading", not "gone".
  const loaded = useSelect((s) => s.bootId !== null);
  // This task's slices only: another task's event does not render the detail.
  const t = useSelect((s) => s.tasks[taskId]);
  const feed = useSelect((s) => s.feed[taskId]) ?? NO_FEED;
  const perms = useSelect((s) => pendingPerms(s.perms, taskId), shallow);
  // Repo memory: refetched when the repo's memory changes or a turn ends (the branch moved).
  useDnaKey(t?.repo_path ?? null);
  const dnaVersion = useDnaRev(t?.repo_path ?? null);
  const memory = useTaskMemory(taskId, dnaVersion + turnsEnded(feed));
  const atRisk = atRiskCount(memory.mem);
  const tt = useT();
  const tabLabel = (name: Tab) => (name === "Memory" && atRisk > 0 ? tt("detail.memoryAtRisk", { n: atRisk }) : tt(TAB_KEY[name]));
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
  // The keyboard starts at the task (a screen reader reads where it landed), once per task.
  const title = useRef<HTMLHeadingElement>(null);
  const hasTask = !!t;
  useEffect(() => {
    if (hasTask) title.current?.focus();
  }, [taskId, hasTask]);
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
          <p className="muted">{tt(loaded ? "detail.gone" : "detail.loading")}</p>
        </main>
      ) : (
        <PaneContext.Provider value={paneApi}>
        <main className="detail-main" aria-label={tt("detail.task", { title: t.title })}>
          <header className="detail-head">
            <h1 tabIndex={-1} ref={title}>
              {t.title}
            </h1>
            <TaskBadges t={t} />
            {t.repos && t.repos.length > 0 && (
              <span className="detail-repos small muted" aria-label={tt("detail.repos")} title={t.workspace_path ?? undefined}>
                {repoName(t.group_path ?? "")}: {t.repos.map((r) => r.name).join(", ")}
              </span>
            )}
            <StatusPill column={columnOf(t, feed)} />
            <SkillsChip key={`skills-${t.id}`} task={t.id} />
            {/* One place for rules: the Rules view, scoped to this task. */}
            <button className="btn btn-small btn-ghost" onClick={() => onRules?.(t.id)}>
              {tt("detail.rules")}
            </button>
            <button
              className="btn btn-small btn-ghost"
              title={tt("pane.reviewTitle")}
              onClick={() => {
                setTab("Terminal");
                setPane((p) => ({ ...p, open: true, target: { k: "diff" } }));
              }}
            >
              {tt("pane.review")}
            </button>
            <Actions key={t.id} t={t} />
          </header>
          {!onTerminal && notices}
          <div role="tablist" aria-label={tt("detail.views")} className="tabs" onKeyDown={onTabKey}>
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
          <div className={`detail-body${pane.swapped ? " detail-body-swapped" : ""}`} ref={body}>
          <div className="panel-wrap">
            <section role="tabpanel" id="task-panel" aria-labelledby={`tab-${tab}`} className="panel">
              {tab === "Steps" && <StepsPanel taskId={t.id} feed={feed} />}
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
          {pane.open && (
            <>
              <PaneSeparator width={pane.width} swapped={pane.swapped} body={body} onWidth={(w) => setPane((p) => ({ ...p, width: w }))} />
              <div className="pane-slot" style={{ flexBasis: `${pane.width}%` }}>
                <SidePane
                  task={t}
                  target={pane.target}
                  version={turnsEnded(feed)}
                  steps={<StepsPanel taskId={t.id} feed={feed} />}
                  onOpenFile={paneApi.openFile}
                  onSwap={() => setPane((p) => ({ ...p, swapped: !p.swapped }))}
                  onClose={() => setPane((p) => ({ ...p, open: false }))}
                />
              </div>
            </>
          )}
          </div>
          {!onTerminal && queue}
          <MessageComposer key={t.id} t={t} />
        </main>
        </PaneContext.Provider>
      )}
    </div>
  );
}
