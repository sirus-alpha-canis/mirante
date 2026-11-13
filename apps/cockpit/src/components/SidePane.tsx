// The Detail's right-hand pane: one target at a time (a worktree file, the Diff, Steps,
// another task's terminal, a memory note, a skill, one of Claude's own agents). Its header names the target and offers
// "Open in editor" (files), Swap and Close. Bodies reuse the views that exist elsewhere
// (FileView, DiffTab, NoteView, SkillPanel); a target a later batch fills says so.
import type { ReactNode } from "react";
import { PaneContext } from "./paneContext";
import { useSelect } from "../env";
import { useT } from "../lib/i18n";
import { hintsFor, onMac } from "../lib/shortcuts";
import type { PaneTarget } from "../lib/sidepane";
import type { TaskView } from "../lib/protocol";
import { DiffTab } from "./DiffTab";
import { EditorButton } from "./EditorButton";
import { FileView } from "./FileView";
import { NoteView } from "./NoteView";
import { SkillPanel } from "./Skills";
import { SubagentPane } from "./SubagentLane";
import { TerminalTab } from "./TerminalTab";
import { ThreadNotes } from "./ThreadHeader";

export { type PaneApi, PaneContext, usePane } from "./paneContext";

function useTitle(target: PaneTarget | null): string {
  const t = useT();
  const other = useSelect((s) => (target?.k === "terminal" || (target?.k === "diff" && target.task) ? s.tasks[target.task!]?.title : undefined));
  if (!target) return t("pane.label");
  switch (target.k) {
    case "file":
      return target.line ? `${target.path}:${target.line}` : target.path;
    case "diff":
      return target.task ? t("pane.diffOf", { title: other ?? target.task }) : t("pane.diff");
    case "steps":
      return t("pane.steps");
    case "notes":
      return t("pane.notes");
    case "terminal":
      return t("pane.terminal", { title: other ?? target.task });
    case "note":
      return t("pane.note", { slug: target.slug });
    case "skill":
      return t("pane.skill", { name: target.name });
    case "subagent":
      return t("pane.subagent", { id: target.id });
  }
}

/** Another task's terminal (a child's): attached while shown. */
function OtherTerminal({ id }: { id: string }) {
  const other = useSelect((s) => s.tasks[id]);
  const t = useT();
  if (!other) return <p className="muted small">{t("pane.unavailable")}</p>;
  return (
    <div className="pane-terminal">
      {/* Its paths are another task's files: not opened in this task's pane. */}
      <PaneContext.Provider value={null}>
        <TerminalTab key={other.id} t={other} active />
      </PaneContext.Provider>
    </div>
  );
}

export function SidePane({
  task,
  target,
  version,
  steps,
  onOpenFile,
  onSwap,
  onClose,
}: {
  task: TaskView;
  /** Null: an open pane with nothing in it yet (says how to fill it). */
  target: PaneTarget | null;
  /** Turns ended (the Diff refetches on each). */
  version: number;
  /** The Steps view, as the main panel builds it. */
  steps: ReactNode;
  onOpenFile(path: string, line?: number): void;
  onSwap(): void;
  onClose(): void;
}) {
  const t = useT();
  const title = useTitle(target);
  let body: ReactNode;
  switch (target?.k) {
    case undefined:
      body = <p className="muted small">{t("pane.empty", { key: hintsFor(onMac()).open })}</p>;
      break;
    case "file":
      if (target?.k !== "file") break;
      body = <FileView key={`${task.id}:${target.path}`} taskId={task.id} path={target.path} line={target.line} onOpenPath={onOpenFile} />;
      break;
    case "diff": {
      if (target?.k !== "diff") break;
      // Another task's Diff (a sibling's): its own changes, refetched on this task's turns too.
      const id = target.task ?? task.id;
      body = <DiffTab key={`pane-diff-${id}`} taskId={id} version={version} />;
      break;
    }
    case "steps":
      body = steps;
      break;
    case "notes":
      body = <ThreadNotes key={`notes-${task.id}`} taskId={task.id} />;
      break;
    case "terminal":
      if (target?.k !== "terminal") break;
      body = target.task === task.id ? <p className="muted small">{t("pane.sameTerminal")}</p> : <OtherTerminal id={target.task} />;
      break;
    case "note":
      if (target?.k !== "note") break;
      body = <NoteView at={{ task: task.id }} repo={target.repo} slug={target.slug} version={0} onOpen={() => {}} />;
      break;
    case "skill":
      if (target?.k !== "skill") break;
      body = <SkillPanel name={target.name} version={0} onChanged={() => {}} />;
      break;
    case "subagent":
      if (target?.k !== "subagent") break;
      body = <SubagentPane key={`${task.id}:${target.id}`} taskId={task.id} id={target.id} />;
      break;
    default:
      body = <p className="muted small">{t("pane.unavailable")}</p>;
  }
  return (
    <aside className="side-pane" aria-label={t("pane.label")}>
      <header className="side-pane-head">
        <span className="side-pane-title" title={title}>
          {title}
        </span>
        {target?.k === "file" && <EditorButton taskId={task.id} path={target.path} line={target.line} />}
        <button type="button" className="btn btn-small btn-ghost" onClick={onSwap}>
          {t("pane.swap")}
        </button>
        <button type="button" className="btn btn-small btn-ghost" aria-label={t("pane.close")} title={t("pane.close")} onClick={onClose}>
          ×
        </button>
      </header>
      <div className="side-pane-body">{body}</div>
    </aside>
  );
}
