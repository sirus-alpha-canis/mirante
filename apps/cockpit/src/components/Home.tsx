import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { shallow, useEnv, useSelect } from "../env";
import { inboxOf, inboxSections, type ProjectGroup, type Section, sameSections, waitingLongerThan } from "../lib/inbox";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import { useNow } from "../lib/useNow";
import { COLUMN_LABEL, type Column, type Project } from "../store/select";
import type { State } from "../store/state";
import { AgentCard } from "./AgentCard";
import { DispatchComposer, loadAgent, saveAgent } from "./DispatchComposer";
import { LeftRail } from "./LeftRail";
import { ProjectChip } from "./ProjectChip";
import { WorkingRow } from "./WorkingRow";

/** A nudge past this wait. */
export const OVERDUE_MS = 10 * 60_000;

export const SECTION_TITLE: Record<Section, string> = {
  needs_you: "Precisa de você",
  review: "Pronto para revisar",
  working: "Trabalhando",
  idle: "Parados",
};

// Both from the one memoized inbox (see `inboxOf`): an event works it out once.
const selectSections = (s: State) => inboxSections(s);
const selectColumns = (s: State) => inboxOf(s).columns;

/** "N agentes esperam você há mais de 10 min" (nothing when none do). */
function Nudge() {
  const now = useNow(30_000);
  const n = useSelect((s) => waitingLongerThan(s, now, OVERDUE_MS));
  if (n === 0) return null;
  return (
    <p className="nudge" role="status">
      {n === 1 ? "1 agente espera você há mais de 10 min" : `${n} agentes esperam você há mais de 10 min`}
    </p>
  );
}

function Groups(props: {
  groups: ProjectGroup[];
  section: Section;
  onOpen(taskId: string, tab?: "Terminal"): void;
  inlineAnswers: boolean;
}) {
  const tasks = useSelect((s) => s.tasks);
  return (
    <>
      {props.groups.map((g) => (
        <div key={g.path} className="inbox-group" role="group" aria-label={g.name}>
          <h3 className="inbox-project">
            <ProjectChip path={g.path} />
          </h3>
          {g.ids.map((id) =>
            tasks[id] ? (
              props.section === "needs_you" || props.section === "review" ? (
                <AgentCard key={id} t={tasks[id]} onOpen={props.onOpen} inlineAnswers={props.inlineAnswers} />
              ) : (
                <WorkingRow key={id} t={tasks[id]} onOpen={props.onOpen} />
              )
            ) : null,
          )}
        </div>
      ))}
    </>
  );
}

/**
 * Home, an inbox across every project: "Precisa de você" (each with its one action),
 * "Pronto para revisar" (the agent's last answer and its diff), "Trabalhando" (one line
 * each), grouped by project; the dispatch form at the bottom. The rail keeps projects,
 * agents, skills, memory and settings. `scroll` / `onScroll`: where the list was (restored
 * once the tasks are there).
 */
export function Home(props: {
  projects: Project[];
  project: string | null;
  onProject(path: string): void;
  onReposChanged(repos: string[]): void;
  /** `tab: "Terminal"` opens the task's detail on its Terminal tab (Claude answered there). */
  onOpen(taskId: string, tab?: "Terminal"): void;
  /** Opens the memory review queue of a project. */
  onReview?(path: string): void;
  /** Opens the Skills view (its review queue when proposals wait). */
  onSkills?(): void;
  status: DaemonStatus | null;
  draft: string;
  onDraft(v: string): void;
  scroll?: number;
  onScroll?(top: number): void;
}) {
  const env = useEnv();
  const sections = useSelect(selectSections, sameSections);
  const inlineAnswers = props.status?.claude_inline_answers ?? false;
  const count = (k: Section) => sections[k].reduce((n, g) => n + g.ids.length, 0);
  const total = count("needs_you") + count("review") + count("working") + count("idle");
  // Every task's column (for the announcement below): renders when one changes, not per event.
  const columns = useSelect(selectColumns, shallow);
  const titles = useSelect((s) => s.tasks);
  const composer = useRef<HTMLTextAreaElement>(null);
  const [agent, setAgent] = useState<AgentKind>(() => loadAgent(env.storage));
  const pickAgent = (a: AgentKind) => {
    setAgent(a);
    saveAgent(env.storage, a);
  };
  const mention = (a: AgentKind) => {
    pickAgent(a);
    composer.current?.focus();
  };
  // One polite region says when a task changes state (not every new line of every task).
  const [announce, setAnnounce] = useState("");
  const seen = useRef<Record<string, Column> | null>(null);
  useEffect(() => {
    const before = seen.current;
    seen.current = columns;
    if (!before) return;
    const changed = Object.keys(columns).filter((id) => id in before && before[id] !== columns[id] && titles[id]);
    if (changed.length > 0) setAnnounce(changed.map((id) => `${titles[id].title}: ${COLUMN_LABEL[columns[id]]}`).join(". "));
  }, [columns, titles]);
  // Back where the list was, once there is something to scroll.
  const list = useRef<HTMLDivElement>(null);
  const restored = useRef(false);
  useLayoutEffect(() => {
    if (restored.current || total === 0 || !list.current) return;
    restored.current = true;
    if (props.scroll) list.current.scrollTop = props.scroll;
  }, [total, props.scroll]);
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
            {SECTION_TITLE[k]} ({n})
          </h2>
        </summary>
        {body}
      </details>
    ) : (
      <section key={k} className={`inbox-section inbox-${k}`} aria-labelledby={id}>
        <h2 id={id}>
          {SECTION_TITLE[k]} ({n})
        </h2>
        {body}
      </section>
    );
  };
  return (
    <div className="home">
      <LeftRail
        projects={props.projects}
        selected={props.project}
        onSelect={props.onProject}
        onReposChanged={props.onReposChanged}
        onMention={mention}
        onOpen={props.onOpen}
        onReview={props.onReview}
        onSkills={props.onSkills}
        status={props.status}
      />
      <main className="feed inbox" aria-label="Inbox">
        <div className="cards" ref={list} onScroll={(e) => scrolled(e.currentTarget.scrollTop)}>
          <Nudge />
          {total === 0 && (
            <p className="muted empty">
              {props.projects.length > 0 ? "No agents yet. Dispatch one below." : "Add a repository on the left to start."}
            </p>
          )}
          {section("needs_you")}
          {section("review")}
          {section("working")}
          {section("idle")}
        </div>
        <p className="sr-only" role="status" aria-label="Agent updates">
          {announce}
        </p>
        <DispatchComposer
          projects={props.projects}
          project={props.project}
          value={props.draft}
          onChange={props.onDraft}
          agent={agent}
          onAgent={pickAgent}
          inputRef={composer}
          paused={props.status?.memory?.paused ?? false}
        />
      </main>
    </div>
  );
}
