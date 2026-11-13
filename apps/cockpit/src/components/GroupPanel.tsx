// A fan-out group inside a member's Detail (founder decision 2): the group has no worktree or
// session of its own, so every member's Detail shows this panel above its tabs. A card per
// task (status stripe, title, agent, last finished answer as text, diff stat), each with
// Steer, Open, Terminal and Diff beside; a sibling's pending permission shows inline (the
// same PermissionCard, answered by the person). "Synthesize…" previews the request the daemon
// builds (the siblings' answers fenced as data) read-only before anything is created or sent.
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { shallow, useEnv, useLive, useSelect } from "../env";
import { rollupOf, rollupText, kindLabel, membersOf } from "../lib/groups";
import { inboxOf } from "../lib/inbox";
import { useT } from "../lib/i18n";
import { DaemonError } from "../lib/daemon";
import { type DiffStat, type GroupMember, type GroupView, M, RpcCode, type SendResult, type StartResult, type Synthesis } from "../lib/protocol";
import type { State } from "../store/state";
import { pendingPerms } from "../store/select";
import { StatusPill } from "./Badges";
import { PermissionCard } from "./PermissionCard";
import { usePane } from "./paneContext";

const selectColumns = (s: State) => inboxOf(s).columns;
const NO_IDS: string[] = [];
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** "3 files +10 −2 · 1 untracked". */
export function diffText(d: DiffStat, t: ReturnType<typeof useT>): string {
  const base = t("group.diff", { files: d.files, added: d.added, removed: d.removed });
  return d.untracked ? `${base} · ${t("group.untracked", { n: d.untracked })}` : base;
}

/** One task of the group. `self`: the task whose Detail this is. */
const ChildCard = memo(function ChildCard(props: { id: string; self: boolean; extra?: GroupMember; onSelect(id: string): void }) {
  const { rpc } = useEnv();
  const live = useLive();
  const t = useT();
  const pane = usePane();
  const task = useSelect(useCallback((s: State) => s.tasks[props.id], [props.id]));
  const col = useSelect(useCallback((s: State) => selectColumns(s)[props.id], [props.id]));
  const perms = useSelect(useCallback((s: State) => pendingPerms(s.perms, props.id), [props.id]), shallow);
  const [text, setText] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const inFlight = useRef(false);
  if (!task || !col) return null;
  const steer = async () => {
    const msg = text;
    if (!msg.trim() || inFlight.current || !live) return;
    inFlight.current = true;
    try {
      const r = await rpc<SendResult>(M.messageSend, { task: props.id, text: msg, mode: "steer" });
      setText((cur) => (cur === msg ? "" : cur));
      setNote(r.delivered === "steer" ? t("composer.sentNow") : t("composer.queued", { n: r.queued ?? "?" }));
    } catch (e) {
      const err = e instanceof DaemonError ? e : null;
      setNote(err?.code === RpcCode.NEEDS_YOU ? t("composer.answerFirst") : message(e));
    } finally {
      inFlight.current = false;
    }
  };
  const answer = props.extra?.last_answer;
  const diff = props.extra?.diff;
  return (
    <article className={`card child-card card-${col}`} aria-label={t("card.label", { agent: task.agent, branch: task.branch })} data-task={task.id}>
      <header className="card-head">
        <span className="child-title">{task.title}</span>
        <StatusPill column={col} />
      </header>
      <div className="card-meta">
        <span className={`agent agent-${task.agent}`}>@{task.agent}</span>
        <code className="small muted">{task.branch}</code>
        {task.group_role === "synth" && <span className="tag">{t("group.synth")}</span>}
        {props.self && <span className="tag">{t("group.self")}</span>}
        {diff && <span className="small muted">{diffText(diff, t)}</span>}
      </div>
      {/* Agent text: a text node, clamped to three lines. */}
      {answer && (
        <p className="answer-text answer-clamp3">
          <span className="who">agent</span> {answer}
        </p>
      )}
      {!props.self &&
        perms.map((p) => <PermissionCard key={p.id} perm={p} agent={task.agent} inlineAnswers={false} compact onAnswerInTerminal={() => props.onSelect(task.id)} />)}
      <div className="child-actions">
        {!props.self && (
          <button type="button" className="btn btn-small" onClick={() => props.onSelect(task.id)}>
            {t("group.open")}
          </button>
        )}
        {!props.self && pane && (
          <>
            <button type="button" className="btn btn-small btn-ghost" onClick={() => pane.openTarget({ k: "terminal", task: task.id })}>
              {t("group.terminal")}
            </button>
            <button type="button" className="btn btn-small btn-ghost" title={t("group.diffBesideTitle")} onClick={() => pane.openTarget({ k: "diff", task: task.id })}>
              {t("group.diffBeside")}
            </button>
          </>
        )}
      </div>
      {!props.self && task.live && (
        <form
          className="child-steer"
          onSubmit={(e) => {
            e.preventDefault();
            void steer();
          }}
        >
          <label className="sr-only" htmlFor={`steer-${task.id}`}>
            {t("group.steerLabel")}
          </label>
          <input id={`steer-${task.id}`} value={text} onChange={(e) => setText(e.target.value)} placeholder={t("group.steerPlaceholder")} />
          <button type="submit" className="btn btn-small" disabled={!text.trim() || !live}>
            {t("group.steer")}
          </button>
          {note && (
            <span className="small muted" role="status">
              {note}
            </span>
          )}
        </form>
      )}
    </article>
  );
});

/** "Synthesize…": who reads the siblings' results (a new task, or one of the children), the
 * request shown read-only, then one confirm that acts only on that preview (its hash). */
function Synthesize(props: { group: string; children: { id: string; label: string }[]; onSelect(id: string): void }) {
  const { rpc, cockpit } = useEnv();
  const t = useT();
  const live = useLive();
  const [open, setOpen] = useState(false);
  const [into, setInto] = useState("");
  const [preview, setPreview] = useState<Synthesis | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ text: string; open?: string } | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!open) return;
    let cancel = false;
    setPreview(null);
    setError(null);
    rpc<Synthesis>(M.groupSynthesize, { group: props.group, ...(into ? { into } : {}), preview: true }).then(
      (p) => !cancel && setPreview(p),
      (e) => !cancel && setError(message(e)),
    );
    return () => {
      cancel = true;
    };
  }, [open, into, props.group, rpc]);
  const act = async () => {
    if (!preview || busy || !live) return;
    setBusy(true);
    setError(null);
    try {
      const r = await rpc<Synthesis>(M.groupSynthesize, { group: props.group, ...(into ? { into } : {}), expect: preview.expect });
      if (r.task) {
        cockpit.dispatch({ type: "task", view: r.task });
        const id = r.task.id;
        setDone({ text: t("group.synthCreated", { branch: r.task.branch }), open: id });
        rpc<StartResult>(M.sessionStart, { task: id, prompt: r.text }).catch((e) =>
          setError(t("dispatch.notStarted", { branch: r.task?.branch ?? id, error: message(e) })),
        );
      } else {
        setDone({ text: t("group.synthSent") });
      }
      setOpen(false);
      setPreview(null);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="group-synth">
      {!open && (
        <button type="button" className="btn btn-small" onClick={() => setOpen(true)}>
          {t("group.synthesize")}
        </button>
      )}
      {open && (
        <div className="synth-preview">
          <label>
            {t("group.synthWho")}{" "}
            <select value={into} onChange={(e) => setInto(e.target.value)}>
              <option value="">{t("group.synthNew")}</option>
              {props.children.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.label}
                </option>
              ))}
            </select>
          </label>
          {preview ? (
            <pre className="synth-text" aria-label={t("group.synthText")}>
              {preview.text}
            </pre>
          ) : (
            !error && <p className="muted small">{t("group.synthLoading")}</p>
          )}
          <div className="child-actions">
            <button type="button" className="btn btn-small" disabled={!preview || busy || !live} onClick={() => void act()}>
              {t(into ? "group.synthSend" : "group.synthCreate")}
            </button>
            <button type="button" className="btn btn-small btn-ghost" onClick={() => setOpen(false)}>
              {t("group.synthCancel")}
            </button>
          </div>
        </div>
      )}
      {error && (
        <p className="error small" role="alert">
          {error}
        </p>
      )}
      {done && (
        <p className="small muted" role="status">
          {done.text}{" "}
          {done.open && (
            <button type="button" className="link" onClick={() => props.onSelect(done.open!)}>
              {t("group.open")}
            </button>
          )}
        </p>
      )}
    </div>
  );
}

/** The panel, for the task `taskId` when it belongs to a group (nothing otherwise). */
export function GroupPanel({ taskId, onSelect }: { taskId: string; onSelect(id: string): void }) {
  const { rpc } = useEnv();
  const t = useT();
  const gid = useSelect(useCallback((s: State) => s.tasks[taskId]?.group_id ?? null, [taskId]));
  const group = useSelect(useCallback((s: State) => s.tasks[taskId]?.group ?? null, [taskId]));
  const ids = useSelect(
    useCallback(
      (s: State) => {
        if (!gid) return NO_IDS;
        const m = membersOf(s.tasks, gid).map((x) => x.id);
        return m.length ? m : NO_IDS;
      },
      [gid],
    ),
    shallow,
  );
  const columns = useSelect(selectColumns, shallow);
  // Refetched (answers, diff stats) when a member's status or turn changes.
  const version = useSelect(
    useCallback(
      (s: State) =>
        ids
          .map((id) => {
            const x = s.tasks[id];
            return x ? `${x.status}:${x.updated_at}` : "";
          })
          .join(","),
      [ids],
    ),
  );
  const [view, setView] = useState<GroupView | null>(null);
  useEffect(() => {
    if (!gid) return;
    let cancel = false;
    rpc<GroupView>(M.groupGet, { group: gid }).then(
      (v) => !cancel && setView(v),
      () => {},
    );
    return () => {
      cancel = true;
    };
  }, [gid, version, rpc]);
  const children = useSelect(
    useCallback(
      (s: State) =>
        ids
          .map((id) => s.tasks[id])
          .filter((x) => x && x.group_role !== "synth")
          .map((x) => ({ id: x.id, label: `${x.branch} (@${x.agent})` })),
      [ids],
    ),
    (a, b) => a.length === b.length && a.every((x, i) => x.id === b[i].id && x.label === b[i].label),
  );
  if (!gid || !group || ids.length === 0) return null;
  const extra = new Map((view?.members ?? []).map((m) => [m.view.id, m]));
  return (
    <section className="group-panel" aria-label={t("group.row", { title: group.title })}>
      <header className="group-panel-head">
        <h2>{group.title}</h2>
        <span className="tag">{kindLabel(group)}</span>
        <span className="muted small">{rollupText(rollupOf(ids, columns))}</span>
      </header>
      <div className="child-cards">
        {ids.map((id) => (
          <ChildCard key={id} id={id} self={id === taskId} extra={extra.get(id)} onSelect={onSelect} />
        ))}
      </div>
      {children.length >= 2 && <Synthesize group={gid} children={children} onSelect={onSelect} />}
    </section>
  );
}
