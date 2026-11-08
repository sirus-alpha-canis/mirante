// Which skills a task's sessions get. The detail's chip asks the daemon (`skill.task_skills`:
// what the next session would get, resolved now) and refetches on the task's skill events;
// its popover names the tenant, the origin, each skill and what the live session did with
// it, the refused ones with why, and, while the task's origin waits for a person, the two
// origins to choose from. The agent card's count needs no call: it reads the session's
// `skill.injected` / `skill.refused` events.
import { type KeyboardEvent, useEffect, useRef, useState } from "react";
import { useEnv, useStore } from "../env";
import type { TaskSkills } from "../lib/protocol";
import { confirmOrigin, skillErrorText, taskSkills } from "../lib/skills";
import { useTaskSkillsVersion } from "../lib/useSkills";

function chipText(ts: TaskSkills): string {
  if (!ts.confirmed) return "Skills off: confirm origin";
  const refused = ts.refused.length > 0 ? ` · ${ts.refused.length} refused` : "";
  return `Skills ${ts.active.length}${refused}`;
}

function ConfirmOrigin({ task, ts, onDone }: { task: string; ts: TaskSkills; onDone(): void }) {
  const { rpc } = useEnv();
  const [busy, setBusy] = useState(false);
  const [notes, setNotes] = useState<string[]>([]);
  const inFlight = useRef(false);
  const confirm = async (origin: string) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    try {
      const r = await confirmOrigin(rpc, task, origin);
      const head =
        origin === ts.origin
          ? `Confirmed ${r.origin}: ${r.cleared} ${r.cleared === 1 ? "task gets" : "tasks get"} skills again.`
          : `New tasks of this checkout now belong to ${r.tenant}. This task stays without skills (it was fixed to ${ts.origin}).`;
      setNotes([head, ...(r.warning ? [r.warning] : [])]);
      onDone();
    } catch (e) {
      setNotes([skillErrorText(e)]);
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  const live = ts.observed && ts.observed !== ts.origin ? ts.observed : null;
  return (
    <div className="skill-confirm">
      <p className="warn small">
        This task gets no skills: its checkout's origin is not confirmed.
        {live ? ` The task was fixed to ${ts.origin}, but the checkout's remote now says ${live}.` : ""} A changed remote can move a
        task into another client's skills, so a person decides.
      </p>
      <div className="dna-actions">
        <button className="btn btn-small" disabled={busy} onClick={() => confirm(ts.origin)}>
          Keep {ts.origin}
        </button>
        {live && (
          <button className="btn btn-small" disabled={busy} onClick={() => confirm(live)}>
            Use {live} for new tasks
          </button>
        )}
      </div>
      {notes.map((n) => (
        <p key={n} className="small" role="status">
          {n}
        </p>
      ))}
    </div>
  );
}

/** The detail header's chip and its popover. Nothing without a daemon that has skills. */
export function SkillsChip({ task }: { task: string }) {
  const { rpc } = useEnv();
  const s = useStore();
  const version = useTaskSkillsVersion(task);
  const [ts, setTs] = useState<TaskSkills | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let live = true;
    taskSkills(rpc, task)
      .then((v) => {
        if (!live) return;
        setTs(v ?? null);
        setError(null);
      })
      .catch((e) => live && setError(skillErrorText(e)));
    return () => {
      live = false;
    };
  }, [rpc, task, version, nonce]);
  if (!ts) return error ? <span className="skill-chip skill-chip-warn" title={error}>Skills unavailable</span> : null;
  const warn = !ts.confirmed || ts.refused.length > 0;
  const session = s.sessionSkills[task];
  const state = (name: string) => {
    if (!session?.names.includes(name)) return "next session";
    return session.used.includes(name) ? "injected, used" : "injected";
  };
  const onKey = (e: KeyboardEvent) => {
    // Escape closes the popover; it is not the app's "leave the detail".
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
    }
  };
  return (
    <span className="skills-chip">
      <button className={`skill-chip ${warn ? "skill-chip-warn" : ""}`} aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        {chipText(ts)}
      </button>
      {open && (
        <div className="skills-pop" role="group" aria-label="Skills of this task" onKeyDown={onKey}>
          <p className="small">
            Tenant: {ts.tenant || "none"} · learning {ts.learning ? "on" : "off"}
          </p>
          <p className="muted small">Origin: {ts.origin || "none recorded"}</p>
          {ts.unavailable && <p className="warn small">{ts.unavailable}</p>}
          {!ts.confirmed && <ConfirmOrigin task={task} ts={ts} onDone={() => setNonce((n) => n + 1)} />}
          {ts.active.length > 0 && (
            <ul className="dna-list" aria-label="Active skills">
              {ts.active.map((a) => (
                <li key={a.name} className="small">
                  {`${a.name} v${a.version} · ${a.activation === "always" ? "always on" : "on demand"} · ${state(a.name)}`}
                </li>
              ))}
            </ul>
          )}
          {ts.confirmed && ts.active.length === 0 && <p className="muted small">No skill applies to this task.</p>}
          {ts.refused.length > 0 && (
            <ul className="dna-list" aria-label="Refused skills">
              {ts.refused.map((r) => (
                <li key={r.name} className="small warn">{`${r.name}: ${r.reason}`}</li>
              ))}
            </ul>
          )}
          <button className="btn btn-small" onClick={() => setOpen(false)}>
            Close
          </button>
        </div>
      )}
    </span>
  );
}

/** The agent card's count: what the task's latest session was given (from its events). */
export function SkillsCount({ task }: { task: string }) {
  const s = useStore();
  const got = s.sessionSkills[task];
  if (!got) return null;
  const refused = got.refused.length > 0 ? ` · ${got.refused.length} refused` : "";
  return (
    <span className={`skill-chip ${got.refused.length > 0 ? "skill-chip-warn" : ""}`} title="Skills given to this task's session">
      {`Skills ${got.names.length}${refused}`}
    </span>
  );
}
