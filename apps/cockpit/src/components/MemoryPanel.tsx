import { useMemo, useRef, useState } from "react";
import { useEnv, useStore } from "../env";
import { DaemonError } from "../lib/daemon";
import { HIBERNATE, isHibernated, mb, systemMemory, taskMemory, WAKE } from "../lib/memguard";
import type { DaemonStatus, TaskView } from "../lib/protocol";

/** Hibernate (end the process, keep the native session) or wake (resume it). One request at
 * a time; the state shown is the daemon's (the next event flips the button), never assumed. */
export function HibernateButton({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);
  const [busy, setBusy] = useState(false);
  const asleep = isHibernated(t);
  if (!t.live && !asleep) return null;
  const run = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      await rpc(asleep ? WAKE : HIBERNATE, { task: t.id });
    } catch (e) {
      setError(
        e instanceof DaemonError && e.unsupported
          ? "memory guard not available in this daemon"
          : e instanceof Error
            ? e.message
            : String(e),
      );
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return (
    <>
      <button
        className="btn btn-small"
        disabled={busy}
        onClick={run}
        title={asleep ? "Resume the agent's session" : "Stop the process, keep the session to resume later"}
      >
        {asleep ? "Wake" : "Hibernate"}
      </button>
      {error && (
        <span className="error" role="alert">
          {error}
        </span>
      )}
    </>
  );
}

/** The daemon's memory guard: machine pressure, the heaviest sessions, the latest notices. */
export function MemoryPanel({ status }: { status: DaemonStatus | null }) {
  const s = useStore();
  const sys = systemMemory(status);
  const heavy = useMemo(
    () =>
      Object.values(s.tasks)
        .filter((t) => taskMemory(t) || isHibernated(t))
        .sort((a, b) => (taskMemory(b)?.bytes ?? 0) - (taskMemory(a)?.bytes ?? 0))
        .slice(0, 5),
    [s.tasks],
  );
  const notices = s.memory.slice(-3).reverse();
  return (
    <section className="memory" aria-label="Memory guard">
      <h2>Memory</h2>
      {!sys ? (
        <p className="muted small">Memory guard not available in this daemon.</p>
      ) : (
        <p className={`small level-${sys.level}`}>
          System pressure <strong>{sys.level}</strong> · {mb(sys.free)} free
          {sys.paused && <> · new sessions paused</>}
        </p>
      )}
      {heavy.length > 0 && (
        <ul className="mem-list" aria-label="Sessions by memory">
          {heavy.map((t) => {
            const m = taskMemory(t);
            return (
              <li key={t.id} className={m ? `mem-${m.state}` : ""}>
                <span className="mem-name">{t.branch}</span>
                <span className="muted small">{m ? `${mb(m.bytes)} / ${mb(m.budget)}` : "hibernated"}</span>
                <HibernateButton t={t} />
              </li>
            );
          })}
        </ul>
      )}
      {notices.length > 0 && (
        <ul className="mem-notices" aria-label="Memory notices">
          {notices.map((n) => (
            <li key={n.seq} className="small">
              {n.task_id && s.tasks[n.task_id] ? `${s.tasks[n.task_id].branch}: ` : ""}
              {n.text}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
