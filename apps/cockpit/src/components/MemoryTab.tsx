// The Memory tab of a task: notes this task's branch puts at risk, notes relevant to it,
// and what it proposed. Everything shown is the daemon's person view, rendered as text.
import { useCallback, useEffect, useRef, useState } from "react";
import { useBoot, useEnv, useSelect } from "../env";
import { DaemonError } from "../lib/daemon";
import { citation, dnaEnable, dnaErrorText, taskContext } from "../lib/dna";
import type { NoteSummary, ProposalView, TaskMemoryContext, TaskView } from "../lib/protocol";
import { M, RpcCode } from "../lib/protocol";
import { turnsEnded } from "../store/select";
import { NoteView, StatePill } from "./NoteView";
import { ErrorRetry } from "./ErrorRetry";
import { t as tr, tn, useT } from "../lib/i18n";
import { Unsupported } from "./Restart";

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
  return tn(n, "memtab.redacted.one", "memtab.redacted.many");
}

/** `dna.task_context` for `taskId`, refetched when `version` changes or on `reload()`. */
export function useTaskMemory(taskId: string, version: number): { mem: TaskMemory; reload(): void } {
  const { rpc } = useEnv();
  const boot = useBoot();
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
  }, [rpc, taskId, version, nonce, boot]);
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

/** A proposal's status (`pending`, `accepted`, `rejected`) in the current language. */
export function proposalStatus(status: string): string {
  return status === "pending" ? tr("memtab.status.pending") : status === "accepted" ? tr("memtab.status.accepted") : status === "rejected" ? tr("memtab.status.rejected") : status;
}

function ProposalRow({ p }: { p: ProposalView }) {
  const t = useT();
  return (
    <li className="dna-row">
      <span className="dna-title">{p.title}</span> <span className="tag">{proposalStatus(p.status)}</span> <span className="tag tag-muted">{p.kind}</span>
      {p.redactions > 0 && <span className="tag tag-warn">{redactedText(p.redactions)}</span>}
      {p.slug && <span className="muted small"> {t("memtab.savedAs", { slug: p.slug })}</span>}
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

/** Asks at most once per turn: after a request is queued the button waits until one of the
 * task's turns ends (the agent has had its chance to propose). */
function AskToPropose({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const tt = useT();
  const ended = useSelect((s) => turnsEnded(s.feed[t.id]));
  const [busy, setBusy] = useState(false);
  const [sentAt, setSentAt] = useState<number | null>(null);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const inFlight = useRef(false);
  const waiting = sentAt !== null && sentAt === ended;
  const ask = async () => {
    if (inFlight.current || waiting) return;
    inFlight.current = true;
    setBusy(true);
    setResult(null);
    try {
      await rpc(M.messageSend, { task: t.id, text: PROPOSE_REQUEST, mode: "queue" });
      setSentAt(ended);
      setResult({ ok: true, text: tr("memtab.asked") });
    } catch (e) {
      setResult({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return (
    <p>
      <button
        className="btn btn-small"
        disabled={busy || waiting || !t.live}
        onClick={ask}
        title={!t.live ? tt("memtab.resumeFirst") : waiting ? tt("memtab.askedWaiting") : undefined}
      >
        {tt("memtab.ask")}
      </button>{" "}
      {result && <span className={result.ok ? "muted small" : "error"}>{result.text}</span>}
    </p>
  );
}

/** What enabling repo memory does, and the explicit button that does it (`dna.enable`). */
export function EnableMemory({ repo, onEnabled }: { repo: string; onEnabled(): void }) {
  const { rpc } = useEnv();
  const t = useT();
  const [before, after] = t("memtab.off", { repo: "\u0000" }).split("\u0000");
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
        {before}
        <code>{repo}</code>
        {after}
      </p>
      <button className="btn btn-primary" disabled={enabling} onClick={enable}>
        {t("memtab.enable")}
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
  const tt = useT();
  const [open, setOpen] = useState<string | null>(null);
  if (mem.state === "loading") return <p className="muted">{tt("brain.loading")}</p>;
  if (mem.state === "unavailable") return <Unsupported text={tt("brain.unavailable")} />;
  if (mem.state === "error") return <ErrorRetry text={tt("brain.error", { error: mem.error })} onRetry={reload} />;
  const { ctx } = mem;
  if (!ctx.enabled) return <EnableMemory repo={t.repo_path} onEnabled={reload} />;
  if (open) {
    return (
      <div className="dna-open">
        <button className="btn btn-small" onClick={() => setOpen(null)}>
          {tt("memtab.back")}
        </button>
        <NoteView at={{ task: t.id }} repo={t.repo_path} slug={open} version={version} onOpen={setOpen} />
      </div>
    );
  }
  return (
    <div className="dna-tab">
      <Section title={tt("memtab.atRisk")} count={ctx.at_risk.length} empty={tt("memtab.atRiskEmpty")}>
        {ctx.at_risk.map((n) => (
          <NoteRow key={n.slug} n={n} reason={n.reason} onOpen={setOpen} />
        ))}
      </Section>
      <Section title={tt("memtab.relevant")} count={ctx.relevant.length} empty={tt("memtab.relevantEmpty")}>
        {ctx.relevant.map((n) => (
          <NoteRow key={n.slug} n={n} onOpen={setOpen} />
        ))}
      </Section>
      <Section title={tt("memtab.proposals")} count={ctx.proposals.length} empty={tt("memtab.proposalsEmpty")}>
        {ctx.proposals.map((p) => (
          <ProposalRow key={p.id} p={p} />
        ))}
      </Section>
      <AskToPropose t={t} />
    </div>
  );
}
