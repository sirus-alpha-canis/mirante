// The Memory tab of a task: notes this task's branch puts at risk, notes relevant to it,
// and what it proposed. Everything shown is the daemon's person view, rendered as text.
import { useCallback, useEffect, useState } from "react";
import { useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import { citation, dnaEnable, dnaErrorText, taskContext } from "../lib/dna";
import type { NoteSummary, ProposalView, TaskMemoryContext, TaskView } from "../lib/protocol";
import { M, RpcCode } from "../lib/protocol";
import { NoteView, StatePill } from "./NoteView";

export type TaskMemory =
  | { state: "loading" }
  | { state: "ready"; ctx: TaskMemoryContext }
  /** The daemon predates repo memory. */
  | { state: "unavailable" }
  | { state: "error"; error: string };

/** The fixed operator message behind "Ask agent to propose memory" (no extra model call:
 * the task's own agent proposes, and a person reviews). */
export const PROPOSE_REQUEST =
  "Operator request: if you learned durable facts about this repo in this task, propose them with the memory_propose tool, citing committed code (path and line range). Do not propose secrets or task-specific details.";

export function redactedText(n: number): string {
  return n === 1 ? "1 secret was redacted" : `${n} secrets were redacted`;
}

/** `dna.task_context` for `taskId`, refetched when `version` changes or on `reload()`. */
export function useTaskMemory(taskId: string, version: number): { mem: TaskMemory; reload(): void } {
  const { rpc } = useEnv();
  const [s, setS] = useState<{ taskId: string; mem: TaskMemory }>({ taskId, mem: { state: "loading" } });
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let live = true;
    taskContext(rpc, taskId)
      .then((c) => {
        if (!live) return;
        const ctx: TaskMemoryContext = {
          enabled: c?.enabled === true,
          relevant: c?.relevant ?? [],
          at_risk: c?.at_risk ?? [],
          proposals: c?.proposals ?? [],
        };
        setS({ taskId, mem: { state: "ready", ctx } });
      })
      .catch((e) => {
        if (!live) return;
        const older = e instanceof DaemonError && e.code === RpcCode.METHOD_NOT_FOUND;
        setS({ taskId, mem: older ? { state: "unavailable" } : { state: "error", error: dnaErrorText(e) } });
      });
    return () => {
      live = false;
    };
  }, [rpc, taskId, version, nonce]);
  const reload = useCallback(() => setNonce((n) => n + 1), []);
  return { mem: s.taskId === taskId ? s.mem : { state: "loading" }, reload };
}

export function atRiskCount(mem: TaskMemory): number {
  return mem.state === "ready" ? mem.ctx.at_risk.length : 0;
}

function NoteRow({ n, reason, onOpen }: { n: NoteSummary; reason?: string; onOpen(slug: string): void }) {
  return (
    <li className="dna-row">
      <button className="link" onClick={() => onOpen(n.slug)}>
        {n.title}
      </button>{" "}
      <StatePill n={n} /> <span className="tag tag-muted">{n.kind}</span>
      {reason && <div className="small dna-reason">{reason}</div>}
      <div className="small muted">
        {n.anchors.map((a, i) => (
          <code key={i} className="dna-cite">
            {citation(a)}
          </code>
        ))}
      </div>
    </li>
  );
}

function ProposalRow({ p }: { p: ProposalView }) {
  return (
    <li className="dna-row">
      <span className="dna-title">{p.title}</span> <span className="tag">{p.status}</span> <span className="tag tag-muted">{p.kind}</span>
      {p.redactions > 0 && <span className="tag tag-warn">{redactedText(p.redactions)}</span>}
      {p.slug && <span className="muted small"> saved as {p.slug}</span>}
    </li>
  );
}

function Section({ title, empty, children, count }: { title: string; empty: string; count: number; children: React.ReactNode }) {
  return (
    <section aria-label={title} className="dna-section">
      <h3>
        {title} <span className="muted count">({count})</span>
      </h3>
      {count === 0 ? <p className="muted small">{empty}</p> : <ul className="dna-list">{children}</ul>}
    </section>
  );
}

function AskToPropose({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const ask = async () => {
    setBusy(true);
    setResult(null);
    try {
      await rpc(M.messageSend, { task: t.id, text: PROPOSE_REQUEST, mode: "queue" });
      setResult({ ok: true, text: "Request queued for the agent (delivered when its turn ends). Its proposals arrive in the review queue." });
    } catch (e) {
      setResult({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };
  return (
    <p>
      <button className="btn btn-small" disabled={busy || !t.live} onClick={ask} title={t.live ? undefined : "Resume the session first"}>
        Ask agent to propose memory
      </button>{" "}
      {result && <span className={result.ok ? "muted small" : "error"}>{result.text}</span>}
    </p>
  );
}

/** What enabling repo memory does, and the explicit button that does it (`dna.enable`). */
export function EnableMemory({ repo, onEnabled }: { repo: string; onEnabled(): void }) {
  const { rpc } = useEnv();
  const [enabling, setEnabling] = useState(false);
  const [enableError, setEnableError] = useState<string | null>(null);
  const enable = async () => {
    setEnabling(true);
    setEnableError(null);
    try {
      await dnaEnable(rpc, repo);
      onEnabled();
    } catch (e) {
      setEnableError(dnaErrorText(e));
    } finally {
      setEnabling(false);
    }
  };
  return (
    <div className="dna-off">
      <p>
        Repo memory is off for <code>{repo}</code>. Enabling it creates .mirante/memory/ in the repo (Markdown notes that
        cite code, committed with it like any file) and gives this repo&apos;s new agent sessions a memory tool. Nothing
        changes in your global Claude or Codex settings, and agents only propose: a person reviews every note.
      </p>
      <button className="btn btn-primary" disabled={enabling} onClick={enable}>
        Enable repo memory
      </button>
      {enableError && (
        <p className="error" role="alert">
          {enableError}
        </p>
      )}
    </div>
  );
}

export function MemoryTab({ t, mem, reload, version }: { t: TaskView; mem: TaskMemory; reload(): void; version: number }) {
  const [open, setOpen] = useState<string | null>(null);
  if (mem.state === "loading") return <p className="muted">Reading repo memory…</p>;
  if (mem.state === "unavailable") return <p className="muted">This daemon has no repo memory. Update Mirante to use it.</p>;
  if (mem.state === "error") return <p className="error">Repo memory unavailable: {mem.error}</p>;
  const { ctx } = mem;
  if (!ctx.enabled) return <EnableMemory repo={t.repo_path} onEnabled={reload} />;
  if (open) {
    return (
      <div className="dna-open">
        <button className="btn btn-small" onClick={() => setOpen(null)}>
          ← Task memory
        </button>
        <NoteView at={{ task: t.id }} repo={t.repo_path} slug={open} version={version} onOpen={setOpen} />
      </div>
    );
  }
  return (
    <div className="dna-tab">
      <Section title="At risk from this task" count={ctx.at_risk.length} empty="This task's branch changes no code a note cites.">
        {ctx.at_risk.map((n) => (
          <NoteRow key={n.slug} n={n} reason={n.reason} onOpen={setOpen} />
        ))}
      </Section>
      <Section title="Relevant notes" count={ctx.relevant.length} empty="No notes cite this task's files or match its goal yet.">
        {ctx.relevant.map((n) => (
          <NoteRow key={n.slug} n={n} onOpen={setOpen} />
        ))}
      </Section>
      <Section title="Proposals from this task" count={ctx.proposals.length} empty="This task has proposed no memory.">
        {ctx.proposals.map((p) => (
          <ProposalRow key={p.id} p={p} />
        ))}
      </Section>
      <AskToPropose t={t} />
    </div>
  );
}
