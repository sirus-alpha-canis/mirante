import { useEffect, useMemo, useRef, useState } from "react";
import { useEnv, useStore } from "../env";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import { columnOf, COLUMN_LABEL, type Column, type Project, repoName } from "../store/select";
import { AgentCard } from "./AgentCard";
import { Board } from "./Board";
import { DispatchComposer, loadAgent, saveAgent } from "./DispatchComposer";
import { LeftRail } from "./LeftRail";

/** V3, the group chat: projects and agents | the project's feed | the board. */
export function Home(props: {
  projects: Project[];
  project: string | null;
  onProject(path: string): void;
  onReposChanged(repos: string[]): void;
  /** `tab: "Terminal"` opens V1 on that task's Terminal tab (Claude answered there). */
  onOpen(taskId: string, tab?: "Terminal"): void;
  /** Opens the memory review queue of a project. */
  onReview?(path: string): void;
  /** Opens the Skills view (its review queue when proposals wait). */
  onSkills?(): void;
  status: DaemonStatus | null;
  draft: string;
  onDraft(v: string): void;
}) {
  const s = useStore();
  const env = useEnv();
  const cards = useMemo(
    () =>
      Object.values(s.tasks)
        .filter((t) => t.repo_path === props.project && t.lifecycle !== "archived")
        .sort((a, b) => a.created_at - b.created_at),
    [s.tasks, props.project],
  );
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
  // One polite region says when a card changes state (not every new line on every card).
  const [announce, setAnnounce] = useState("");
  const columns = useRef<Map<string, Column> | null>(null);
  useEffect(() => {
    const now = new Map(cards.map((t) => [t.id, columnOf(t, s.feed[t.id])]));
    const before = columns.current;
    columns.current = now;
    if (!before) return;
    const changed = cards.filter((t) => before.has(t.id) && before.get(t.id) !== now.get(t.id));
    if (changed.length > 0) setAnnounce(changed.map((t) => `${t.title}: ${COLUMN_LABEL[now.get(t.id)!]}`).join(". "));
  }, [cards, s.feed]);
  return (
    <div className="home">
      <LeftRail
        projects={props.projects}
        selected={props.project}
        onSelect={props.onProject}
        onReposChanged={props.onReposChanged}
        onMention={mention}
        onReview={props.onReview}
        onSkills={props.onSkills}
        status={props.status}
      />
      <main className="feed" aria-label="Project feed">
        <header className="feed-head">
          <h1>{props.project ? repoName(props.project) : "No project yet"}</h1>
          {props.project && <span className="muted small">{props.project}</span>}
        </header>
        <div className="cards">
          {cards.length === 0 && (
            <p className="muted empty">
              {props.project ? "No agents here yet. Dispatch one below." : "Add a repository on the left to start."}
            </p>
          )}
          {cards.map((t) => (
            <AgentCard key={t.id} t={t} onOpen={props.onOpen} inlineAnswers={props.status?.claude_inline_answers ?? false} />
          ))}
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
        />
      </main>
      <Board onOpen={props.onOpen} />
    </div>
  );
}
