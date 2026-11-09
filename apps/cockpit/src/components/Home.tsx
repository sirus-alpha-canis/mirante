import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { shallow, useEnv, useSelect } from "../env";
import { inboxOf, inboxSections, type ProjectGroup, type Section, sameSections, waitingLongerThan } from "../lib/inbox";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import type { Registry } from "../lib/repos";
import { useNow } from "../lib/useNow";
import { columnLabel, type Column, type Project } from "../store/select";
import { type Key, t, tn, useT } from "../lib/i18n";
import type { State } from "../store/state";
import { AgentCard } from "./AgentCard";
import { type ComposerForm, DispatchComposer, loadAgent, saveAgent } from "./DispatchComposer";
import { LeftRail } from "./LeftRail";
import { ProjectChip } from "./ProjectChip";
import { WorkingRow } from "./WorkingRow";

/** A nudge past this wait. */
export const OVERDUE_MS = 10 * 60_000;

const SECTION_KEY: Record<Section, Key> = {
  needs_you: "section.needs_you",
  review: "section.review",
  working: "section.working",
  idle: "section.idle",
};

/** A section's title in the current language. */
export const sectionTitle = (k: Section) => t(SECTION_KEY[k]);

// Both from the one memoized inbox (see `inboxOf`): an event works it out once.
const selectSections = (s: State) => inboxSections(s);
const selectColumns = (s: State) => inboxOf(s).columns;

/** "N agents have waited on you for over 10 min" (nothing when none have). */
function Nudge() {
  useT();
  const now = useNow(30_000);
  const n = useSelect((s) => waitingLongerThan(s, now, OVERDUE_MS));
  if (n === 0) return null;
  return (
    <p className="nudge" role="status">
      {tn(n, "nudge.one", "nudge.many")}
    </p>
  );
}

/** One task's card or row: renders when that task changes, not when another does. */
const TaskEntry = memo(function TaskEntry(props: { id: string; card: boolean; onOpen(taskId: string, tab?: "Terminal"): void; inlineAnswers: boolean }) {
  const task = useSelect(useCallback((s: State) => s.tasks[props.id], [props.id]));
  if (!task) return null;
  return props.card ? <AgentCard t={task} onOpen={props.onOpen} inlineAnswers={props.inlineAnswers} /> : <WorkingRow t={task} onOpen={props.onOpen} />;
});

function Groups(props: {
  groups: ProjectGroup[];
  section: Section;
  onOpen(taskId: string, tab?: "Terminal"): void;
  inlineAnswers: boolean;
}) {
  const card = props.section === "needs_you" || props.section === "review";
  return (
    <>
      {props.groups.map((g) => (
        <div key={g.path} className="inbox-group" role="group" aria-label={g.name}>
          <h3 className="inbox-project">
            <ProjectChip path={g.path} />
          </h3>
          {g.ids.map((id) => (
            <TaskEntry key={id} id={id} card={card} onOpen={props.onOpen} inlineAnswers={props.inlineAnswers} />
          ))}
        </div>
      ))}
    </>
  );
}

/**
 * Home, an inbox across every project: "Needs you" (each with its one action), "Ready for
 * review" (the agent's last answer and its diff), "Working" (one line each), grouped by
 * project; the dispatch form at the bottom. The rail keeps projects, agents, skills, system
 * memory and settings. `scroll` / `onScroll`: where the list was (restored once the tasks are
 * there). `focusTask`: the task whose card takes the keyboard (back from its detail).
 */
export const Home = memo(function Home(props: {
  /** Every project, hidden ones too (marked): the rail lists those on request; the inbox and
   * the dispatch form leave them out. */
  projects: Project[];
  registry?: Registry;
  project: string | null;
  onProject(path: string): void;
  onReposChanged(registry: Registry): void;
  /** `tab: "Terminal"` opens the task's detail on its Terminal tab (Claude answered there). */
  onOpen(taskId: string, tab?: "Terminal"): void;
  /** Opens the memory review queue of a project. */
  onReview?(path: string): void;
  /** Opens the Skills view (its review queue when proposals wait). */
  onSkills?(): void;
  status: DaemonStatus | null;
  draft: string;
  onDraft(v: string): void;
  /** The rest of the dispatch form, when the caller keeps it. */
  form?: ComposerForm;
  onForm?(update: (cur: ComposerForm) => ComposerForm): void;
  scroll?: number;
  onScroll?(top: number): void;
  focusTask?: string | null;
  onFocused?(): void;
}) {
  const env = useEnv();
  const t = useT();
  const all = useSelect(selectSections, sameSections);
  const { projects: everyProject } = props;
  const visible = useMemo(() => everyProject.filter((p) => !p.hidden), [everyProject]);
  // A hidden project's tasks are left out of the inbox.
  const hiddenKey = everyProject
    .filter((p) => p.hidden)
    .map((p) => p.path)
    .join("\n");
  const sections = useMemo(() => {
    if (!hiddenKey) return all;
    const hidden = new Set(hiddenKey.split("\n"));
    const keep = (gs: ProjectGroup[]) => gs.filter((g) => !hidden.has(g.path));
    return { needs_you: keep(all.needs_you), review: keep(all.review), working: keep(all.working), idle: keep(all.idle) };
  }, [all, hiddenKey]);
  const inlineAnswers = props.status?.claude_inline_answers ?? false;
  const count = (k: Section) => sections[k].reduce((n, g) => n + g.ids.length, 0);
  const total = count("needs_you") + count("review") + count("working") + count("idle");
  // Every task's column (for the announcement below): renders when one changes, not per event.
  const columns = useSelect(selectColumns, shallow);
  const composer = useRef<HTMLTextAreaElement>(null);
  const [agent, setAgent] = useState<AgentKind>(() => loadAgent(env.storage));
  const pickAgent = useCallback(
    (a: AgentKind) => {
      setAgent(a);
      saveAgent(env.storage, a);
    },
    [env],
  );
  const mention = useCallback(
    (a: AgentKind) => {
      pickAgent(a);
      composer.current?.focus();
    },
    [pickAgent],
  );
  // One polite region says when a task changes state (not every new line of every task).
  const [announce, setAnnounce] = useState("");
  const seen = useRef<Record<string, Column> | null>(null);
  useEffect(() => {
    const before = seen.current;
    seen.current = columns;
    if (!before) return;
    const tasks = env.cockpit.getState().tasks;
    const changed = Object.keys(columns).filter((id) => id in before && before[id] !== columns[id] && tasks[id]);
    if (changed.length > 0) setAnnounce(changed.map((id) => `${tasks[id].title}: ${columnLabel(columns[id])}`).join(". "));
  }, [columns, env]);
  // Back where the list was, once there is something to scroll.
  const list = useRef<HTMLDivElement>(null);
  const restored = useRef(false);
  useLayoutEffect(() => {
    if (restored.current || total === 0 || !list.current) return;
    restored.current = true;
    if (props.scroll) list.current.scrollTop = props.scroll;
  }, [total, props.scroll]);
  // Back from a task's detail: its card (or row) takes the keyboard.
  const { focusTask, onFocused } = props;
  useEffect(() => {
    if (!focusTask) return;
    const el = list.current?.querySelector(`[data-task="${CSS.escape(focusTask)}"]`);
    const target = el?.querySelector<HTMLElement>("button, [href], [tabindex]");
    if (target) {
      target.focus();
      onFocused?.();
    }
  }, [focusTask, onFocused, total]);
  const onScroll = props.onScroll;
  const scrolled = useMemo(() => {
    let frame = 0;
    return (top: number) => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => onScroll?.(top));
    };
  }, [onScroll]);
  const section = (k: Section) => {
    const n = count(k);
    if (n === 0) return null;
    const id = `inbox-${k}`;
    const body = <Groups groups={sections[k]} section={k} onOpen={props.onOpen} inlineAnswers={inlineAnswers} />;
    return k === "idle" ? (
      <details key={k} className={`inbox-section inbox-${k}`}>
        <summary>
          <h2 id={id}>
            {sectionTitle(k)} ({n})
          </h2>
        </summary>
        {body}
      </details>
    ) : (
      <section key={k} className={`inbox-section inbox-${k}`} aria-labelledby={id}>
        <h2 id={id}>
          {sectionTitle(k)} ({n})
        </h2>
        {body}
      </section>
    );
  };
  return (
    <div className="home">
      <LeftRail
        projects={props.projects}
        registry={props.registry}
        selected={props.project}
        onSelect={props.onProject}
        onReposChanged={props.onReposChanged}
        onMention={mention}
        onOpen={props.onOpen}
        onReview={props.onReview}
        onSkills={props.onSkills}
        status={props.status}
      />
      <main className="feed inbox" aria-label={t("home.inbox")}>
        <div className="cards" ref={list} onScroll={(e) => scrolled(e.currentTarget.scrollTop)}>
          <Nudge />
          {total === 0 && <p className="muted empty">{visible.length > 0 ? t("home.empty") : t("home.noProject")}</p>}
          {section("needs_you")}
          {section("review")}
          {section("working")}
          {section("idle")}
        </div>
        <p className="sr-only" role="status" aria-label={t("home.updates")}>
          {announce}
        </p>
        <DispatchComposer
          projects={visible}
          project={props.project}
          value={props.draft}
          onChange={props.onDraft}
          agent={agent}
          onAgent={pickAgent}
          inputRef={composer}
          pressure={props.status?.memory?.level === "elevated" || props.status?.memory?.level === "critical"}
          form={props.form}
          onForm={props.onForm}
        />
      </main>
    </div>
  );
});
