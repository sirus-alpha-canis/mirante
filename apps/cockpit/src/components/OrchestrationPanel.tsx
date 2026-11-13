// Agent-initiated fan-out inside a task's Detail. On a task with no parent: the person's
// "Orchestration" switch (off by default; MCP config is fixed at launch, so it applies from
// the next session), the sub-tasks its agent proposed (one card each, decided by the
// person) and the ones the person approved. On an approved sub-task: where it came from.
// A thread shows nothing here (it has its own header).
import { memo, useCallback, useEffect, useState } from "react";
import { shallow, useEnv, useSelect } from "../env";
import { inboxOf } from "../lib/inbox";
import { useT } from "../lib/i18n";
import { M, type SpawnProposalView, type TaskView } from "../lib/protocol";
import type { State } from "../store/state";
import { StatusPill } from "./Badges";
import { SpawnProposalCard } from "./SpawnProposalCard";

const NO_IDS: string[] = [];
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
const selectColumns = (s: State) => inboxOf(s).columns;

/** One approved sub-task: title, status, open. */
const SubTaskRow = memo(function SubTaskRow({ id, onSelect }: { id: string; onSelect(id: string): void }) {
  const task = useSelect(useCallback((s: State) => s.tasks[id], [id]));
  const col = useSelect(useCallback((s: State) => selectColumns(s)[id], [id]));
  if (!task || !col) return null;
  return (
    <li className="subtask-row">
      <button type="button" className="link" onClick={() => onSelect(id)}>
        {task.title}
      </button>{" "}
      <span className={`agent agent-${task.agent}`}>@{task.agent}</span> <StatusPill column={col} />
    </li>
  );
});

function Proposals({ taskId, pending, onSelect }: { taskId: string; pending: number; onSelect(id: string): void }) {
  const { rpc } = useEnv();
  const t = useT();
  const [list, setList] = useState<SpawnProposalView[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [rev, setRev] = useState(0);
  useEffect(() => {
    if (pending === 0) {
      setList([]);
      return;
    }
    let cancel = false;
    rpc<SpawnProposalView[]>(M.taskSpawnProposals, { task: taskId, status: "pending" })
      .then((l) => {
        if (!cancel) {
          setList(Array.isArray(l) ? l : []);
          setError(null);
        }
      })
      .catch((e) => !cancel && setError(message(e)));
    return () => {
      cancel = true;
    };
  }, [rpc, taskId, pending, rev]);
  const done = useCallback(() => setRev((r) => r + 1), []);
  if (error) {
    return (
      <p className="error small" role="alert">
        {t("spawn.loadFailed", { error })}
      </p>
    );
  }
  return (
    <>
      {list.map((p) => (
        <SpawnProposalCard key={p.id} p={p} onDone={done} onSelect={onSelect} />
      ))}
    </>
  );
}

export function OrchestrationPanel({ t: task, onSelect }: { t: TaskView; onSelect(id: string): void }) {
  const { rpc, cockpit } = useEnv();
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const parent = useSelect(useCallback((s: State) => (task.parent_id ? s.tasks[task.parent_id] : undefined), [task.parent_id]));
  const children = useSelect(
    useCallback(
      (s: State) => {
        const ids = Object.values(s.tasks)
          .filter((x) => x.parent_id === task.id && x.child_kind === "spawn" && x.lifecycle !== "archived")
          .sort((a, b) => a.created_at - b.created_at)
          .map((x) => x.id);
        return ids.length ? ids : NO_IDS;
      },
      [task.id],
    ),
    shallow,
  );
  if (task.parent_id) {
    if (task.child_kind !== "spawn") return null;
    return (
      <section className="orch orch-child" aria-label={t("spawn.childHeader")}>
        <span className="tag">{t("spawn.childTag")}</span>{" "}
        <button type="button" className="link small" disabled={!parent} onClick={() => parent && onSelect(parent.id)}>
          ↳ {parent ? t("spawn.childOf", { title: parent.title }) : t("spawn.childOfGone")}
        </button>{" "}
        <span className="small muted">{t("spawn.childNote")}</span>
      </section>
    );
  }
  const on = !!task.orchestration;
  const pending = task.spawn_pending ?? 0;
  const stale = task.live && task.orchestrator_live != null && task.orchestrator_live !== on;
  const toggle = async () => {
    if (busy || task.lifecycle === "archived") return;
    setBusy(true);
    setError(null);
    try {
      const view = await rpc<TaskView>(M.taskOrchestrate, { task: task.id, enabled: !on });
      cockpit.dispatch({ type: "task", view });
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="orch" aria-label={t("spawn.header")}>
      <label className="orch-switch small">
        <input type="checkbox" role="switch" checked={on} disabled={busy} onChange={() => void toggle()} /> {t("spawn.switch")}
      </label>{" "}
      <span className="small muted">{t(on ? "spawn.onHint" : "spawn.offHint")}</span>
      {stale && (
        <p className="small muted" role="status">
          {t("spawn.nextSession")}
        </p>
      )}
      {error && (
        <p className="error small" role="alert">
          {error}
        </p>
      )}
      {pending > 0 && <Proposals taskId={task.id} pending={pending} onSelect={onSelect} />}
      {children.length > 0 && (
        <ul className="subtasks" aria-label={t("spawn.children")}>
          {children.map((id) => (
            <SubTaskRow key={id} id={id} onSelect={onSelect} />
          ))}
        </ul>
      )}
    </section>
  );
}
