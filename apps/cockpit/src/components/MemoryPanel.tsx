import { useRef, useState } from "react";
import { shallow, useEnv, useLive, useSelect } from "../env";
import { feedText, type FeedMsg, same, type State } from "../store/state";
import { useT } from "../lib/i18n";
import { DaemonError } from "../lib/daemon";
import { HIBERNATE, isHibernated, mb, pressureText, systemMemory, taskMemory, WAKE } from "../lib/memguard";
import type { DaemonStatus, TaskView } from "../lib/protocol";

/** Hibernate (end the process, keep the native session) or wake (resume it). One request at
 * a time; the state shown is the daemon's (the next event flips the button), never assumed.
 * Hibernate asks first (like Stop), and says so when the agent is mid-turn. */
export function HibernateButton({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const live = useLive();
  const tt = useT();
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
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
          ? tt("mem.unavailableGuard")
          : e instanceof Error
            ? e.message
            : String(e),
      );
    } finally {
      inFlight.current = false;
      setBusy(false);
      setConfirm(false);
    }
  };
  const alert = error && (
    <span className="error" role="alert">
      {error}
    </span>
  );
  if (confirm && !asleep) {
    return (
      <>
        <span role="group" aria-label={tt("mem.confirmHibernate")}>
          <span className="small">
            {tt(t.status === "working" ? "mem.midTurn" : "mem.hibernateQuestion")}
          </span>{" "}
          <button className="btn btn-small btn-warn" disabled={busy} onClick={run}>
            {tt("mem.hibernateNow")}
          </button>
          <button className="btn btn-small" onClick={() => setConfirm(false)}>
            {tt("ui.cancel")}
          </button>
        </span>
        {alert}
      </>
    );
  }
  return (
    <>
      <button
        className="btn btn-small"
        disabled={busy || !live}
        onClick={asleep ? run : () => setConfirm(true)}
        title={tt(asleep ? "mem.wakeTitle" : "mem.hibernateTitle")}
      >
        {tt(asleep ? "mem.wake" : "mem.hibernate")}
      </button>
      {alert}
    </>
  );
}

/** The sessions using the most memory (and the hibernated ones), heaviest first, at most 5. */
function selectHeavy(s: State): TaskView[] {
  return Object.values(s.tasks)
    .filter((t) => taskMemory(t) || isHibernated(t))
    .sort((a, b) => (taskMemory(b)?.bytes ?? 0) - (taskMemory(a)?.bytes ?? 0))
    .slice(0, 5);
}

/** The latest three memory-guard notices, newest first, with their task's branch. */
function selectNotices(s: State): { seq: number; text: string; msg?: FeedMsg; branch: string | null }[] {
  return s.memory
    .slice(-3)
    .reverse()
    .map((n) => ({ seq: n.seq, text: n.text, msg: n.msg, branch: n.task_id ? (s.tasks[n.task_id]?.branch ?? null) : null }));
}

/** The daemon's memory guard: machine pressure, the heaviest sessions, the latest notices. */
export function MemoryPanel({ status, onOpen }: { status: DaemonStatus | null; onOpen?(taskId: string): void }) {
  const tt = useT();
  const t = tt;
  const heavy = useSelect(selectHeavy, shallow);
  const notices = useSelect(selectNotices, same);
  const sys = systemMemory(status);
  return (
    <section className="memory" aria-label={t("mem.guard")}>
      <h2>{t("mem.heading")}</h2>
      {!status ? (
        <p className="muted small">{t("mem.unknown")}</p>
      ) : !sys ? (
        <p className="muted small">{t("mem.notAvailable")}</p>
      ) : (
        <>
          <p className={`small level-${sys.level}`}>{pressureText(sys).main}</p>
          <p className="small muted" title={t("mem.availableTitle")}>
            {pressureText(sys).secondary}
          </p>
        </>
      )}
      {heavy.length > 0 && (
        <ul className="mem-list" aria-label={t("mem.byMemory")}>
          {heavy.map((t) => {
            const m = taskMemory(t);
            return (
              <li key={t.id} className={m ? `mem-${m.state}` : ""}>
                {onOpen ? (
                  <button className="mem-name link" title={tt("mem.open", { title: t.title })} onClick={() => onOpen(t.id)}>
                    {t.branch}
                  </button>
                ) : (
                  <span className="mem-name">{t.branch}</span>
                )}
                <span className="muted small">{m ? `${mb(m.bytes)} / ${mb(m.budget)}` : tt("mem.hibernated")}</span>
                <HibernateButton t={t} />
              </li>
            );
          })}
        </ul>
      )}
      {notices.length > 0 && (
        <ul className="mem-notices" aria-label={t("mem.notices")}>
          {notices.map((n) => (
            <li key={n.seq} className="small">
              {n.branch ? `${n.branch}: ` : ""}
              {feedText(n)}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
