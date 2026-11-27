// The Detail's side pane: tabs (a worktree file, the Diff, Steps, another task's terminal,
// a memory note, a skill, one of Claude's own agents, the person's own shell, a Browser on a
// local dev server), one shown,
// or two side by side (Split). Its header names the shown tab and offers "Open in editor"
// (files), Swap and Close; the tab strip below adds tabs ("+") and closes them. Bodies
// reuse the views that exist elsewhere (FileView, DiffTab, NoteView, SkillPanel,
// TerminalTab).
import { type ReactNode, useRef } from "react";
import { PaneContext } from "./paneContext";
import { useEnv, useSelect } from "../env";
import { useT } from "../lib/i18n";
import { hintsFor, onMac } from "../lib/shortcuts";
import { localHost } from "../lib/localurl";
import { activeTarget, clampRatio, closeTab, focusTab, freeShell, openTab, type PaneState, type PaneTarget, retarget, toggleSplit } from "../lib/sidepane";
import { BrowserTab } from "./BrowserTab";
import { type AddSurface, PaneTabs } from "./PaneTabs";
import { closeShell, markFresh, ShellTab } from "./ShellTab";
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
    case "shell":
      return t("pane.shell", { n: target.n });
    case "browser":
      return target.url || t("pane.browser");
  }
}

/** A tab's short name: a file's base name, else its title. */
function TabLabel({ target }: { target: PaneTarget }) {
  const title = useTitle(target);
  if (target.k === "file") return <span title={title}>{target.path.split("/").pop()}</span>;
  if (target.k === "browser" && target.url) return <span title={title}>{localHost(target.url)}</span>;
  return <span title={title}>{title}</span>;
}

/** A tab's name as plain text (its × button's label). */
function tabName(t: ReturnType<typeof useT>, target: PaneTarget): string {
  switch (target.k) {
    case "file":
      return target.path.split("/").pop() ?? target.path;
    case "diff":
      return target.task ? t("pane.diffOf", { title: target.task }) : t("pane.diff");
    case "steps":
      return t("pane.steps");
    case "notes":
      return t("pane.notes");
    case "terminal":
      return t("pane.terminal", { title: target.task });
    case "note":
      return t("pane.note", { slug: target.slug });
    case "skill":
      return t("pane.skill", { name: target.name });
    case "subagent":
      return t("pane.subagent", { id: target.id });
    case "shell":
      return t("pane.shell", { n: target.n });
    case "browser":
      return target.url ? localHost(target.url) : t("pane.browser");
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

/** One tab's view. */
function Body({
  task,
  id,
  target,
  version,
  steps,
  onOpenFile,
  onRetarget,
}: {
  task: TaskView;
  /** The tab's id. */
  id: string | null;
  target: PaneTarget | null;
  version: number;
  steps: ReactNode;
  onOpenFile(path: string, line?: number): void;
  onRetarget(id: string, target: PaneTarget): void;
}) {
  const t = useT();
  switch (target?.k) {
    case undefined:
      return <p className="muted small">{t("pane.empty", { key: hintsFor(onMac()).open })}</p>;
    case "file":
      return <FileView key={`${task.id}:${target.path}`} taskId={task.id} path={target.path} line={target.line} onOpenPath={onOpenFile} />;
    case "diff": {
      // Another task's Diff (a sibling's): its own changes, refetched on this task's turns too.
      const id = target.task ?? task.id;
      return <DiffTab key={`pane-diff-${id}`} taskId={id} version={version} />;
    }
    case "steps":
      return <>{steps}</>;
    case "notes":
      return <ThreadNotes key={`notes-${task.id}`} taskId={task.id} />;
    case "terminal":
      return target.task === task.id ? <p className="muted small">{t("pane.sameTerminal")}</p> : <OtherTerminal id={target.task} />;
    case "note":
      return <NoteView at={{ task: task.id }} repo={target.repo} slug={target.slug} version={0} onOpen={() => {}} />;
    case "skill":
      return <SkillPanel name={target.name} version={0} onChanged={() => {}} />;
    case "subagent":
      return <SubagentPane key={`${task.id}:${target.id}`} taskId={task.id} id={target.id} />;
    case "shell":
      return <ShellTab key={`${task.id}:shell:${target.n}`} task={task} n={target.n} />;
    case "browser":
      return <BrowserTab key={`${task.id}:browser:${id}`} url={target.url} onNavigate={(url) => id && onRetarget(id, { k: "browser", url })} />;
    default:
      return <p className="muted small">{t("pane.unavailable")}</p>;
  }
}

/** The line between a split's two tabs: drag it, or arrows by 5%. */
function SplitSeparator({ ratio, dir, box, onRatio }: { ratio: number; dir: "row" | "col"; box: React.RefObject<HTMLDivElement | null>; onRatio(r: number): void }) {
  const t = useT();
  const dragging = useRef(false);
  const back = dir === "row" ? "ArrowLeft" : "ArrowUp";
  const fwd = dir === "row" ? "ArrowRight" : "ArrowDown";
  return (
    <div
      role="separator"
      aria-orientation={dir === "row" ? "vertical" : "horizontal"}
      aria-label={t("pane.splitResize")}
      aria-valuenow={Math.round(ratio * 100)}
      aria-valuemin={20}
      aria-valuemax={80}
      tabIndex={0}
      className={`pane-split-sep pane-split-sep-${dir}`}
      onKeyDown={(e) => {
        const d = e.key === back ? -0.05 : e.key === fwd ? 0.05 : 0;
        if (!d) return;
        e.preventDefault();
        onRatio(clampRatio(ratio + d));
      }}
      onPointerDown={(e) => {
        dragging.current = true;
        e.currentTarget.setPointerCapture?.(e.pointerId);
      }}
      onPointerMove={(e) => {
        const r = box.current?.getBoundingClientRect();
        if (!dragging.current || !r) return;
        const size = dir === "row" ? r.width : r.height;
        if (size <= 0) return;
        onRatio(clampRatio(dir === "row" ? (e.clientX - r.left) / size : (e.clientY - r.top) / size));
      }}
      onPointerUp={() => {
        dragging.current = false;
      }}
    />
  );
}

export function SidePane({
  task,
  pane,
  version,
  steps,
  onOpenFile,
  onPane,
  onAddFile,
  onSwap,
  onClose,
}: {
  task: TaskView;
  /** Its tabs, the shown one and the split (no tabs: an open pane says how to fill it). */
  pane: PaneState;
  /** Turns ended (the Diff refetches on each). */
  version: number;
  /** The Steps view, as the main panel builds it. */
  steps: ReactNode;
  onOpenFile(path: string, line?: number): void;
  onPane(update: (p: PaneState) => PaneState): void;
  /** "+" → File…: quick-open, into a new tab. */
  onAddFile(): void;
  onSwap(): void;
  onClose(): void;
}) {
  const t = useT();
  const { rpc } = useEnv();
  const target = activeTarget(pane);
  const title = useTitle(target);
  const box = useRef<HTMLDivElement>(null);
  const second = pane.split ? (pane.tabs.find((x) => x.id === pane.split!.second)?.t ?? null) : null;
  const add = (what: AddSurface) => {
    if (what === "file") return onAddFile();
    if (what === "browser") return onPane((p) => openTab(p, { k: "browser", url: "" }, true));
    const n = freeShell(pane);
    if (n === null) return;
    markFresh(task.id, n);
    onPane((p) => openTab(p, { k: "shell", n }, true));
  };
  const close = (id: string) => {
    const tab = pane.tabs.find((x) => x.id === id);
    // A shell's tab closed: the shell ends too (nothing keeps running unseen).
    if (tab?.t.k === "shell") closeShell(rpc, task.id, tab.t.n);
    onPane((p) => closeTab(p, id));
  };
  const body = (id: string | null, tg: PaneTarget | null) => (
    <Body task={task} id={id} target={tg} version={version} steps={steps} onOpenFile={onOpenFile} onRetarget={(tab, to) => onPane((p) => retarget(p, tab, to))} />
  );
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
      <PaneTabs
        pane={pane}
        label={(id) => {
          const tg = pane.tabs.find((x) => x.id === id)?.t;
          return tg ? <TabLabel target={tg} /> : null;
        }}
        name={(id) => {
          const tg = pane.tabs.find((x) => x.id === id)?.t;
          return tg ? tabName(t, tg) : id;
        }}
        canAddShell={freeShell(pane) !== null}
        onFocus={(id) => onPane((p) => focusTab(p, id))}
        onCloseTab={close}
        onAdd={add}
        onSplit={() => onPane((p) => toggleSplit(p))}
      />
      {pane.split && second ? (
        <div className={`side-pane-split side-pane-split-${pane.split.dir}`} ref={box}>
          <div className="side-pane-body" role="tabpanel" aria-labelledby={`pane-tab-${pane.active}`} style={{ flexBasis: `${pane.split.ratio * 100}%` }}>
            {body(pane.active, target)}
          </div>
          <SplitSeparator ratio={pane.split.ratio} dir={pane.split.dir} box={box} onRatio={(r) => onPane((p) => (p.split ? { ...p, split: { ...p.split, ratio: r } } : p))} />
          <div className="side-pane-body" role="tabpanel" aria-labelledby={`pane-tab-${pane.split.second}`}>
            {body(pane.split.second, second)}
          </div>
        </div>
      ) : (
        <div className="side-pane-body" role={target ? "tabpanel" : undefined} aria-labelledby={target ? `pane-tab-${pane.active}` : undefined}>
          {body(pane.active, target)}
        </div>
      )}
    </aside>
  );
}
