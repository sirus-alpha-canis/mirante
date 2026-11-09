import { type ReactNode, type RefObject, useCallback, useEffect, useId, useRef, useState } from "react";
import { useEnv, useSelect } from "../env";
import { type ArchiveFailure, archiveTasks } from "../lib/archive";
import { tn, useT } from "../lib/i18n";
import type { RepoScan, TaskView } from "../lib/protocol";
import { forgetRepos, hideRepos, type Registry, setCollapsed, showRepos } from "../lib/repos";
import { useDismiss } from "../lib/useDismiss";
import { type Project, repoName } from "../store/select";
import { same, type State } from "../store/state";

/** Past this many projects the list gets a filter box. */
const FILTER_FROM = 8;

/** What a × is about to remove: one project, or a folder's group. */
interface Target {
  key: string;
  name: string;
  paths: string[];
}

// Whether an added path is a git checkout, asked once per path (per scanner: a test's own).
const checked = new WeakMap<object, Map<string, Promise<RepoScan | null>>>();

function checkOnce(scan: (p: string) => Promise<RepoScan>, path: string): Promise<RepoScan | null> {
  let m = checked.get(scan);
  if (!m) checked.set(scan, (m = new Map()));
  let p = m.get(path);
  if (!p) {
    // A path that cannot be looked at (gone, unreadable) is not called a folder here.
    p = scan(path).catch(() => null);
    m.set(path, p);
  }
  return p;
}

/** Forgets what was found at `path` (it changes: scanned again, removed, re-added). */
function uncheck(scan: object, path: string) {
  checked.get(scan)?.delete(path);
}

/**
 * The rail's Projects: each project (with its × and memory proposals), a folder's projects
 * under the folder's name, hidden ones behind a toggle, a filter once there are many, and a
 * warning row for a folder added before that is no repository. A × removes a project with no
 * tasks at once; with tasks it asks first, inline (archive them, or hide the project).
 */
export function ProjectList(props: {
  projects: Project[];
  registry: Registry;
  selected: string | null;
  onSelect(path: string): void;
  onReposChanged(r: Registry): void;
  onOpen(taskId: string): void;
  onScan(folder: string): void;
  pending: { path: string; n: number } | null;
  onReview?(path: string): void;
}) {
  const { storage, scanRepos } = useEnv();
  const tr = useT();
  const headingId = useId();
  const heading = useRef<HTMLHeadingElement>(null);
  const [confirm, setConfirm] = useState<Target | null>(null);
  const [showHidden, setShowHidden] = useState(false);
  const [filter, setFilter] = useState("");
  const { onReposChanged } = props;

  // Added paths with no tasks that are folders, not checkouts (added before the scan).
  const [folders, setFolders] = useState<Set<string>>(new Set());
  const added = new Set(props.registry.repos);
  const toCheck = props.projects
    .filter((p) => p.tasks === 0 && added.has(p.path))
    .map((p) => p.path)
    .join("\n");
  useEffect(() => {
    let live = true;
    for (const path of toCheck ? toCheck.split("\n") : []) {
      void checkOnce(scanRepos, path).then((r) => {
        if (!live || !r || r.root !== null) return;
        setFolders((cur) => (cur.has(path) ? cur : new Set(cur).add(path)));
      });
    }
    return () => {
      live = false;
    };
  }, [scanRepos, toCheck]);

  const hiddenCount = props.projects.filter((p) => p.hidden).length;
  const shown = props.projects.filter((p) => showHidden || !p.hidden);
  const needle = filter.trim().toLowerCase();
  const matches = (p: Project) => !needle || p.name.toLowerCase().includes(needle) || p.path.toLowerCase().includes(needle);
  const collapsed = new Set(props.registry.collapsed);

  // Top-level entries by name: a project on its own, or a folder's group.
  const entries = (() => {
    const groups = new Map<string, Project[]>();
    const out: ({ kind: "project"; name: string; p: Project } | { kind: "group"; name: string; folder: string; members: Project[] })[] = [];
    for (const p of shown) {
      if (!matches(p)) continue;
      if (p.group) {
        const g = groups.get(p.group);
        if (g) g.push(p);
        else groups.set(p.group, [p]);
      } else out.push({ kind: "project", name: p.name, p });
    }
    for (const [folder, members] of groups) out.push({ kind: "group", name: repoName(folder), folder, members });
    return out.sort((a, b) => a.name.localeCompare(b.name));
  })();

  const changed = (r: Registry) => onReposChanged(r);
  /** Removes `t` at once when none of its projects has tasks; else asks. */
  const remove = (t: Target) => {
    const tasks = props.projects.filter((p) => t.paths.includes(p.path)).reduce((n, p) => n + p.tasks, 0);
    if (tasks > 0) return setConfirm(t);
    for (const p of t.paths) uncheck(scanRepos, p);
    setFolders((cur) => new Set([...cur].filter((p) => !t.paths.includes(p))));
    changed(forgetRepos(storage, t.paths));
  };
  const unhide = (p: Project) => changed(showRepos(storage, [p.path]));
  const fold = (folder: string) => changed(setCollapsed(storage, folder, !collapsed.has(folder)));

  const confirmFor = (t: Target, wrap: RefObject<HTMLElement | null>) =>
    confirm?.key === t.key && (
      <RemoveConfirm
        target={confirm}
        wrap={wrap}
        onOpen={props.onOpen}
        onClose={(back) => {
          setConfirm(null);
          if (back) wrap.current?.querySelector<HTMLButtonElement>("[data-remove]")?.focus();
        }}
        onDone={(r, gone) => {
          changed(r);
          if (gone) {
            setConfirm(null);
            heading.current?.focus();
          }
        }}
        onHide={(paths) => {
          setConfirm(null);
          changed(hideRepos(storage, paths));
          heading.current?.focus();
        }}
      />
    );

  const row = (p: Project) => (
    <ProjectRow
      key={p.path}
      p={p}
      folder={folders.has(p.path) && p.tasks === 0}
      selected={props.selected === p.path}
      pending={props.pending?.path === p.path ? props.pending.n : 0}
      onSelect={props.onSelect}
      onReview={props.onReview}
      onRemove={() => remove({ key: `p:${p.path}`, name: p.name, paths: [p.path] })}
      onUnhide={() => unhide(p)}
      onScan={() => {
        uncheck(scanRepos, p.path);
        props.onScan(p.path);
      }}
      confirm={(wrap) => confirmFor({ key: `p:${p.path}`, name: p.name, paths: [p.path] }, wrap)}
    />
  );

  return (
    <>
      <h2 id={headingId} ref={heading} tabIndex={-1}>
        {tr("rail.projects")}
      </h2>
      {props.projects.filter((p) => !p.hidden).length > FILTER_FROM && (
        <input type="search" className="rail-filter" aria-label={tr("rail.filter")} placeholder={tr("rail.filter")} value={filter} onChange={(e) => setFilter(e.target.value)} />
      )}
      <ul aria-labelledby={headingId}>
        {entries.map((e) =>
          e.kind === "project" ? (
            row(e.p)
          ) : (
            <GroupEntry
              key={`g:${e.folder}`}
              folder={e.folder}
              name={e.name}
              members={e.members}
              open={!collapsed.has(e.folder) || !!needle}
              onToggle={() => fold(e.folder)}
              onRemove={() => remove({ key: `g:${e.folder}`, name: e.name, paths: e.members.map((m) => m.path) })}
              confirm={(wrap) => confirmFor({ key: `g:${e.folder}`, name: e.name, paths: e.members.map((m) => m.path) }, wrap)}
              row={row}
            />
          ),
        )}
      </ul>
      {hiddenCount > 0 && (
        <button className="btn btn-small btn-ghost rail-hidden-toggle" aria-pressed={showHidden} onClick={() => setShowHidden((v) => !v)}>
          {tr(showHidden ? "rail.hideHidden" : "rail.showHidden", { n: hiddenCount })}
        </button>
      )}
    </>
  );
}

function GroupEntry(props: {
  folder: string;
  name: string;
  members: Project[];
  open: boolean;
  onToggle(): void;
  onRemove(): void;
  confirm(wrap: RefObject<HTMLElement | null>): ReactNode;
  row(p: Project): ReactNode;
}) {
  const tr = useT();
  const wrap = useRef<HTMLLIElement>(null);
  return (
    <li ref={wrap} className="rail-entry rail-group">
      <div className="rail-row">
        <button className="rail-item rail-group-head" aria-expanded={props.open} title={props.folder} onClick={props.onToggle}>
          <span>
            <span aria-hidden="true" className="muted">
              {props.open ? "▾ " : "▸ "}
            </span>
            {props.name} ({props.members.length})
          </span>
        </button>
        <button data-remove className="btn btn-small btn-ghost" aria-label={tr("rail.removeGroup", { name: props.name })} title={tr("rail.removeTitle", { path: props.folder })} onClick={props.onRemove}>
          ×
        </button>
      </div>
      {props.confirm(wrap)}
      {props.open && (
        <ul className="rail-group-list" aria-label={tr("rail.groupLabel", { path: props.folder })}>
          {props.members.map(props.row)}
        </ul>
      )}
    </li>
  );
}

function ProjectRow(props: {
  p: Project;
  /** An added folder that is no repository (the warning row). */
  folder: boolean;
  selected: boolean;
  pending: number;
  onSelect(path: string): void;
  onReview?(path: string): void;
  onRemove(): void;
  onUnhide(): void;
  onScan(): void;
  confirm(wrap: RefObject<HTMLElement | null>): ReactNode;
}) {
  const tr = useT();
  const { p } = props;
  const wrap = useRef<HTMLLIElement>(null);
  return (
    <li ref={wrap} className={`rail-entry ${p.hidden ? "rail-hidden" : ""}`}>
      <div className="rail-row">
        {props.folder ? (
          <span className="rail-warn small" title={p.path}>
            <span>{tr("rail.notRepo", { name: p.name })}</span>
            <button className="btn btn-small" onClick={props.onScan}>
              {tr("rail.scanIt")}
            </button>
          </span>
        ) : (
          <button
            className={`rail-item ${props.selected ? "selected" : ""}`}
            aria-current={props.selected ? "page" : undefined}
            title={p.path}
            onClick={() => props.onSelect(p.path)}
          >
            <span>{p.name}</span>
            {p.hidden && <span className="muted small">{tr("rail.hidden")}</span>}
            {p.needsYou > 0 && (
              <>
                <span className="badge" aria-hidden="true">
                  {p.needsYou}
                </span>
                <span className="sr-only">, {tn(p.needsYou, "rail.needsOne", "rail.needsMany")}</span>
              </>
            )}
          </button>
        )}
        {props.pending > 0 && (
          <button className="btn btn-small btn-ghost dna-pending" title={tr("rail.proposalsTitle", { path: p.path })} disabled={!props.onReview} onClick={() => props.onReview?.(p.path)}>
            {tr("rail.toReview", { n: props.pending })}
          </button>
        )}
        {p.hidden && (
          <button className="btn btn-small btn-ghost" aria-label={tr("rail.unhideLabel", { name: p.name })} onClick={props.onUnhide}>
            {tr("rail.unhide")}
          </button>
        )}
        <button data-remove className="btn btn-small btn-ghost" aria-label={tr("rail.remove", { name: p.name })} title={tr("rail.removeTitle", { path: p.path })} onClick={props.onRemove}>
          ×
        </button>
      </div>
      {props.confirm(wrap)}
    </li>
  );
}

/**
 * "shop has 3 tasks (1 live). Remove it from Projects?…": archive its tasks (live sessions
 * stopped first) and remove it, or hide it, or cancel. The keyboard goes here when it opens
 * and back to the × after Escape or Cancel; a press elsewhere closes it (not while archiving).
 * Tasks the daemon would not archive (a dirty worktree) are listed with why; their project
 * stays.
 */
function RemoveConfirm(props: {
  target: Target;
  wrap: RefObject<HTMLElement | null>;
  onOpen(taskId: string): void;
  /** `back`: the keyboard returns to the ×. */
  onClose(back: boolean): void;
  /** The registry after archiving; `gone` when every task was archived. */
  onDone(r: Registry, gone: boolean): void;
  onHide(paths: string[]): void;
}) {
  const env = useEnv();
  const tr = useT();
  const id = useId();
  const box = useRef<HTMLDivElement>(null);
  const { paths, name } = props.target;
  const tasks = useSelect(
    useCallback((s: State) => Object.values(s.tasks).filter((t) => paths.includes(t.repo_path) && t.lifecycle !== "archived"), [paths]),
    same,
  );
  const [busy, setBusy] = useState<number | null>(null);
  const [failed, setFailed] = useState<ArchiveFailure[]>([]);
  // Counted when it opened: archiving changes them under the question.
  const [asked] = useState(() => ({ n: tasks.length, live: tasks.filter((t) => t.live).length }));
  useEffect(() => box.current?.focus(), []);
  const { onClose } = props;
  useDismiss(props.wrap, (why) => onClose(why === "escape"), { enabled: busy === null });

  const archive = async () => {
    const list: TaskView[] = tasks;
    setBusy(list.length);
    setFailed([]);
    const failures = await archiveTasks(env, list);
    const stays = new Set(failures.map((f) => list.find((t) => t.id === f.id)?.repo_path));
    props.onDone(
      forgetRepos(
        env.storage,
        paths.filter((p) => !stays.has(p)),
      ),
      failures.length === 0,
    );
    setFailed(failures);
    setBusy(null);
  };

  return (
    <div ref={box} className="rail-confirm" role="group" aria-label={tr("rail.confirmLabel", { name })} aria-describedby={id} tabIndex={-1}>
      <p id={id} className="small">
        {tn(asked.n, "rail.confirmOne", "rail.confirmMany", { name, live: asked.live })}
      </p>
      <div className="scan-actions">
        <button className="btn btn-small btn-warn" disabled={busy !== null || tasks.length === 0} onClick={() => void archive()}>
          {tr("rail.archiveRemove")}
        </button>
        <button className="btn btn-small btn-ghost" disabled={busy !== null} title={tr("rail.hideTitle")} onClick={() => props.onHide(paths)}>
          {tr("rail.hide")}
        </button>
        <button className="btn btn-small btn-ghost" disabled={busy !== null} onClick={() => props.onClose(true)}>
          {tr("ui.cancel")}
        </button>
      </div>
      {busy !== null && (
        <p className="small muted" role="status">
          {tr("rail.archiving", { n: busy })}
        </p>
      )}
      {failed.length > 0 && (
        <div role="alert" className="small">
          <p className="warn">{tr("rail.archiveFailed", { n: failed.length, name })}</p>
          <ul className="rail-failed">
            {failed.map((f) => (
              <li key={f.id}>
                <span>
                  {f.title} ({f.branch}): {f.reason}
                </span>{" "}
                <button className="btn btn-small" onClick={() => props.onOpen(f.id)}>
                  {tr("rail.openTask")}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
