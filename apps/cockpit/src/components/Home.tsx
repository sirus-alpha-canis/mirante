import { useMemo } from "react";
import { useStore } from "../env";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import { type Project, repoName } from "../store/select";
import { AgentCard } from "./AgentCard";
import { Board } from "./Board";
import { DispatchComposer } from "./DispatchComposer";
import { LeftRail } from "./LeftRail";

/** V3, the group chat: projects and agents | the project's feed | the board. */
export function Home(props: {
  projects: Project[];
  project: string | null;
  onProject(path: string): void;
  onReposChanged(repos: string[]): void;
  /** `tab: "Terminal"` opens V1 on that task's Terminal tab (Claude answered there). */
  onOpen(taskId: string, tab?: "Terminal"): void;
  status: DaemonStatus | null;
  draft: string;
  onDraft(v: string): void;
}) {
  const s = useStore();
  const cards = useMemo(
    () =>
      Object.values(s.tasks)
        .filter((t) => t.repo_path === props.project && t.lifecycle !== "archived")
        .sort((a, b) => a.created_at - b.created_at),
    [s.tasks, props.project],
  );
  const mention = (agent: AgentKind) => {
    const repo = props.project ? `${repoName(props.project)}/` : "";
    props.onDraft(`@${agent} ${repo}`);
    setTimeout(() => document.getElementById("dispatch")?.focus(), 0);
  };
  return (
    <div className="home">
      <LeftRail
        projects={props.projects}
        selected={props.project}
        onSelect={props.onProject}
        onReposChanged={props.onReposChanged}
        onMention={mention}
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
        <DispatchComposer projects={props.projects} value={props.draft} onChange={props.onDraft} />
      </main>
      <Board onOpen={props.onOpen} />
    </div>
  );
}
