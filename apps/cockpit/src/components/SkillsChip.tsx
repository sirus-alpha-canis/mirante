// Which skills a task's sessions get. The detail's chip asks the daemon (`skill.task_skills`:
// what the next session would get, resolved now) and refetches on the task's skill events;
// its popover names the tenant, the origin, each skill and what the live session did with
// it, the refused ones with why, and, while the task's origin waits for a person, the two
// origins to choose from. The agent card's count needs no call: it reads the session's
// `skill.injected` / `skill.refused` events.
import { useEffect, useRef, useState } from "react";
import { useEnv, useSelect } from "../env";
import type { RepoOrigin, TaskSkills } from "../lib/protocol";
import { confirmOrigin, originTenant, skillErrorText, skillRepoOrigin, taskSkills } from "../lib/skills";
import { useTaskSkillsVersion } from "../lib/useSkills";

function chipText(ts: TaskSkills): string {
  if (!ts.confirmed) return "Skills off: confirm origin";
  const refused = ts.refused.length > 0 ? ` · ${ts.refused.length} refused` : "";
  return `Skills ${ts.active.length}${refused}`;
}

/** What a confirmation said: kept by the chip, so it survives the refetch that follows. */
type Notes = { text: string; warn: boolean }[];

function ConfirmOrigin({ task, ts, onDone }: { task: string; ts: TaskSkills; onDone(notes: Notes): void }) {
  const { rpc } = useEnv();
  const repo = useSelect((s) => s.tasks[task]?.repo_path ?? "");
  const [busy, setBusy] = useState(false);
  const [moving, setMoving] = useState(false);
  const [origins, setOrigins] = useState<RepoOrigin | null>(null);
  const inFlight = useRef(false);
  const live = ts.observed && ts.observed !== ts.origin ? ts.observed : null;
  useEffect(() => {
    if (!repo) return;
    let on = true;
    // The checkout's pinned origin and its remote now, each with its client, as the daemon
    // reads them (an older daemon: the task's own two, clients derived here).
    skillRepoOrigin(rpc, repo)
      .then((o) => on && o && setOrigins(o))
      .catch(() => {});
    return () => {
      on = false;
    };
  }, [rpc, repo]);
  const pinned = origins?.pinned ?? ts.origin;
  const pinnedTenant = origins?.pinned_tenant ?? originTenant(pinned);
  const now = origins?.live ?? live ?? ts.origin;
  const nowTenant = origins?.live_tenant ?? originTenant(now);
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
      onDone([{ text: head, warn: false }, ...(r.warning ? [{ text: r.warning, warn: true }] : [])]);
    } catch (e) {
      onDone([{ text: skillErrorText(e), warn: true }]);
    } finally {
      inFlight.current = false;
      setBusy(false);
      setMoving(false);
    }
  };
  return (
    <div className="skill-confirm">
      <p className="warn small">
        This task gets no skills: its checkout's origin is not confirmed. A changed remote can move a task into another client's
        skills, so a person decides.
      </p>
      <ul className="dna-list" aria-label="Origins of this checkout">
        <li className="small">{`Pinned: ${pinned} (client ${pinnedTenant})`}</li>
        <li className="small">{`Remote now: ${now} (client ${nowTenant})`}</li>
      </ul>
      {moving && live ? (
        <>
          <p className="warn small">
            {`This moves new tasks of this checkout from client ${originTenant(ts.origin)} to ${originTenant(live)}: they get ${originTenant(live)}'s skills and learning. This task stays on ${originTenant(ts.origin)}, without skills.`}
          </p>
          <div className="dna-actions">
            <button className="btn btn-small btn-warn" disabled={busy} onClick={() => confirm(live)}>
              Move new tasks to {originTenant(live)}
            </button>
            <button className="btn btn-small" disabled={busy} onClick={() => setMoving(false)}>
              Cancel
            </button>
          </div>
        </>
      ) : (
        <div className="dna-actions">
          <button className="btn btn-small" disabled={busy} onClick={() => confirm(ts.origin)}>
            Keep {ts.origin}
          </button>
          {live && (
            <button className="btn btn-small" disabled={busy} onClick={() => setMoving(true)}>
              Use {live} for new tasks…
            </button>
          )}
        </div>
      )}
    </div>
  );
}

/** The detail header's chip and its popover (a dialog: focus goes in when it opens, Escape
 * anywhere closes it and focus goes back to the chip). Nothing without a daemon that has
 * skills. */
export function SkillsChip({ task }: { task: string }) {
  const { rpc } = useEnv();
  const session = useSelect((s) => s.sessionSkills[task]);
  const version = useTaskSkillsVersion(task);
  const [ts, setTs] = useState<TaskSkills | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [nonce, setNonce] = useState(0);
  const [notes, setNotes] = useState<Notes>([]);
  const chip = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);
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
  useEffect(() => {
    if (!open) return;
    pop.current?.querySelector<HTMLElement>("button, [tabindex]")?.focus();
    // Escape anywhere closes it first (capture): it is not the app's "leave the detail".
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
      chip.current?.focus();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);
  if (!ts) return error ? <span className="skill-chip skill-chip-warn" title={error}>Skills unavailable</span> : null;
  const warn = !ts.confirmed || ts.refused.length > 0;
  const state = (name: string) => {
    if (!session?.names.includes(name)) return "next session";
    return session.used.includes(name) ? "injected, used" : "injected";
  };
  const close = () => {
    setOpen(false);
    chip.current?.focus();
  };
  return (
    <span className="skills-chip">
      <button
        ref={chip}
        className={`skill-chip ${warn ? "skill-chip-warn" : ""}`}
        aria-expanded={open}
        aria-haspopup="dialog"
        onClick={() => setOpen((o) => !o)}
      >
        {chipText(ts)}
      </button>
      {open && (
        <div className="skills-pop" role="dialog" aria-label="Skills of this task" ref={pop} tabIndex={-1}>
          <p className="small">
            Tenant: {ts.tenant || "none"} · learning {ts.learning ? "on" : "off"}
          </p>
          <p className="muted small">Origin: {ts.origin || "none recorded"}</p>
          {ts.unavailable && <p className="warn small">{ts.unavailable}</p>}
          {!ts.confirmed && (
            <ConfirmOrigin
              task={task}
              ts={ts}
              onDone={(n) => {
                setNotes(n);
                setNonce((x) => x + 1);
              }}
            />
          )}
          {notes.map((n) => (
            <p key={n.text} className={`small ${n.warn ? "warn" : ""}`} role="status">
              {n.text}
            </p>
          ))}
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
          <button className="btn btn-small" onClick={close}>
            Close
          </button>
        </div>
      )}
    </span>
  );
}

/** The agent card's count: what the task's latest session was given (from its events). */
export function SkillsCount({ task }: { task: string }) {
  const got = useSelect((s) => s.sessionSkills[task]);
  if (!got) return null;
  const refused = got.refused.length > 0 ? ` · ${got.refused.length} refused` : "";
  return (
    <span className={`skill-chip ${got.refused.length > 0 ? "skill-chip-warn" : ""}`} title="Skills given to this task's session">
      {`Skills ${got.names.length}${refused}`}
    </span>
  );
}
