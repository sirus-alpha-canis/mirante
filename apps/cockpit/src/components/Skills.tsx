// The skills view: the user-level library grouped by scope (personal, each org, each repo)
// with verification and 30-day usage, the clients (tenants) with their opt-in learning, and
// the selected skill beside it. Skill text is rendered as text nodes only: a SKILL.md is
// instructions for an agent, never markup for the cockpit.
import { useEffect, useMemo, useRef, useState } from "react";
import { useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import type { SkillSummary, SkillView, TenantView } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import {
  activationLabel,
  scopeKind,
  scopeLabel,
  skillAction,
  skillBody,
  skillErrorText,
  skillGet,
  skillLearn,
  skillList,
  skillMine,
  skillRereview,
  skillTenants,
  scopeTenant,
  wideningWarnings,
} from "../lib/skills";
import { usePendingSkills, useSkillsVersion } from "../lib/useSkills";
import { SkillProposals } from "./SkillProposals";

const KIND_ORDER = { personal: 0, org: 1, repo: 2 } as const;

/** The library as groups of one scope each: personal, then orgs, then repos (by name). */
export function groupByScope(list: SkillSummary[]): { scope: string; skills: SkillSummary[] }[] {
  const by = new Map<string, SkillSummary[]>();
  for (const s of list) by.set(s.scope, [...(by.get(s.scope) ?? []), s]);
  return [...by.entries()]
    .sort(([a], [b]) => KIND_ORDER[scopeKind(a)] - KIND_ORDER[scopeKind(b)] || a.localeCompare(b))
    .map(([scope, skills]) => ({ scope, skills: skills.sort((x, y) => x.name.localeCompare(y.name)) }));
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function TenantRow({ t, onChanged }: { t: TenantView; onChanged(): void }) {
  const { rpc } = useEnv();
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const inFlight = useRef(false);
  const run = async (fn: () => Promise<string | null>) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setNote(null);
    try {
      setNote(await fn());
    } catch (e) {
      setNote(skillErrorText(e));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  const learn = () =>
    run(async () => {
      await skillLearn(rpc, t.tenant, !t.learning);
      onChanged();
      return null;
    });
  const mine = () =>
    run(async () => {
      const r = await skillMine(rpc, t.tenant);
      return `${plural(r.proposed, "new proposal")}, ${r.updated} updated, ${r.skipped} skipped`;
    });
  return (
    <li className="dna-row skill-tenant" role="group" aria-label={t.tenant}>
      <div className="dna-note-head">
        <strong>{t.tenant}</strong>
        <span className={`tag ${t.learning ? "tag-on" : ""}`}>{t.learning ? "Learning on" : "Learning off"}</span>
        <span className="muted small">
          {plural(t.tasks, "task")} · {plural(t.skills, "skill")}
        </span>
      </div>
      <p className="muted small">
        Mirante reads your goals, messages and denied permissions in this client's tasks, on this machine only, to find
        instructions you repeat. Nothing is read while learning is off.
      </p>
      <div className="dna-actions">
        <button className="btn btn-small" disabled={busy} onClick={learn}>
          {t.learning ? `Stop learning from ${t.tenant}` : `Start learning from ${t.tenant}`}
        </button>
        {t.learning && (
          <button className="btn btn-small" disabled={busy} onClick={mine}>
            Find repeated instructions
          </button>
        )}
      </div>
      {note && (
        <p className="muted small" role="status">
          {note}
        </p>
      )}
    </li>
  );
}

function SkillRow({ s, selected, onSelect }: { s: SkillSummary; selected: boolean; onSelect(): void }) {
  return (
    <li className="dna-row">
      <button className="link skill-row" aria-pressed={selected} onClick={onSelect}>
        <span className="dna-title">{s.name}</span>{" "}
        <span className="muted small">
          v{s.version} · {s.activation === "always" ? "always on" : "on demand"} ·{" "}
          <span className={s.verified ? "" : "skill-refused"}>{s.verified ? "verified" : "refused"}</span>
          {s.status === "retired" ? " · retired" : ""} · injected {s.injected_30d} · used {s.used_30d} (30 days)
        </span>
      </button>
    </li>
  );
}

type Loaded = { state: "loading" } | { state: "ready"; view: SkillView } | { state: "error"; error: string };

/** One skill: what it says, where it applies, whether it verifies, and the person's actions
 * (each sends the `manifest_hash` it was shown). */
export function SkillPanel({ name, version, onChanged }: { name: string; version: number; onChanged(): void }) {
  const { rpc } = useEnv();
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [nonce, setNonce] = useState(0);
  const [scope, setScope] = useState("");
  const [back, setBack] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);
  // The version the scope field was filled from: a refetch of the same one keeps what the
  // person typed; a new version (or another skill) refills it.
  const scopeOf = useRef<string | null>(null);
  useEffect(() => {
    let live = true;
    skillGet(rpc, name)
      .then((view) => {
        if (!live) return;
        setLoaded({ state: "ready", view });
        if (scopeOf.current !== view.manifest_hash) {
          scopeOf.current = view.manifest_hash;
          setScope(view.scope);
        }
      })
      .catch((e) => live && setLoaded({ state: "error", error: skillErrorText(e) }));
    return () => {
      live = false;
    };
  }, [rpc, name, version, nonce]);
  if (loaded.state === "loading") return <p className="muted">Reading {name}…</p>;
  if (loaded.state === "error") return <p className="error">Skill unavailable: {loaded.error}</p>;
  const v = loaded.view;
  const run = async (fn: () => Promise<string | null>) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      setNotice(await fn());
    } catch (e) {
      if (e instanceof DaemonError && e.code === RpcCode.CONFLICT) {
        setError("The skill changed since it was shown; it was reloaded. Check it and try again.");
        setNonce((n) => n + 1);
      } else setError(skillErrorText(e));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  const act = (action: "retire" | "restore" | "rescope" | "rollback", extra: { scope?: string; version?: number } = {}) =>
    run(async () => {
      const next = await skillAction(rpc, v.name, action, v.manifest_hash, extra);
      setLoaded({ state: "ready", view: next });
      scopeOf.current = next.manifest_hash;
      setScope(next.scope);
      onChanged();
      return action === "retire" ? "Retired: no new session gets it." : action === "restore" ? "Restored." : `Now v${next.version}.`;
    });
  const reReview = () =>
    run(async () => {
      // The daemon reads this version's own folder; the cockpit never names a path.
      const p = await skillRereview(rpc, v.name, v.shown_version);
      onChanged();
      return `The folder on disk was queued for review${p?.id ? ` (proposal ${p.id})` : ""}: accept it to make it a new version.`;
    });
  const learnedIn = v.manifest?.provenance.learned_in ?? null;
  const scopeChanged = scope.trim() !== "" && scope.trim() !== v.scope;
  const widen = scopeChanged ? wideningWarnings(scope.trim(), learnedIn ?? scopeTenant(v.scope)) : [];
  const older = v.versions.filter((n) => n < v.version);
  const body = skillBody(v.skill_md);
  return (
    <section className="skill-panel" aria-label={`Skill ${v.name}`}>
      <div className="dna-note-head">
        <h3>{v.name}</h3>
        <span className="tag">v{v.shown_version}</span>
        <span className={`tag ${v.verified ? "tag-on" : "tag-warn"}`}>{v.verified ? "verified" : "refused"}</span>
        {v.status === "retired" && <span className="tag">retired</span>}
      </div>
      <p>{v.description}</p>
      <p className="small">
        <strong>{scopeLabel(v.scope)}</strong> · {activationLabel(v.activation)}
      </p>
      <p className="muted small">
        Injected {v.injected_30d} · used {v.used_30d} in the last 30 days
      </p>
      {v.warnings.map((w) => (
        <p key={w} className="warn small">
          {w}
        </p>
      ))}
      {!v.verified ? (
        <div className="skill-refusal">
          <p className="warn">Refused: {v.reason ?? "it does not verify"}.</p>
          <p className="small">
            The folder on disk is not what a person accepted, so it is not given to any agent until you review it again. Re-review
            queues the folder's current text as a proposal: read it there before accepting.
          </p>
          <button className="btn btn-small" disabled={busy} onClick={reReview}>
            Re-review
          </button>
        </div>
      ) : (
        <>
          <pre className="dna-body skill-body" aria-label="SKILL.md">
            {body}
          </pre>
          {v.files.map((f) => (
            <div key={f.path}>
              <p className="small">
                <code>{f.path}</code>
              </p>
              <pre className="dna-body skill-body">{f.text}</pre>
            </div>
          ))}
        </>
      )}
      <div className="dna-actions">
        {v.status === "retired" ? (
          <button className="btn btn-small" disabled={busy} onClick={() => act("restore")}>
            Restore
          </button>
        ) : (
          <button className="btn btn-small btn-warn" disabled={busy} onClick={() => act("retire")}>
            Retire
          </button>
        )}
      </div>
      <form
        className="dna-actions"
        onSubmit={(e) => {
          e.preventDefault();
          if (scopeChanged) void act("rescope", { scope: scope.trim() });
        }}
      >
        <label htmlFor={`scope-${v.name}`} className="small muted">
          New scope
        </label>
        <input
          id={`scope-${v.name}`}
          className="dna-input"
          placeholder="personal, org:host/owner or repo:host/owner/repo"
          value={scope}
          onChange={(e) => setScope(e.target.value)}
        />
        <button className="btn btn-small" type="submit" disabled={busy || !scopeChanged}>
          Rescope
        </button>
      </form>
      {widen.map((w) => (
        <p key={w} className="warn small" role="alert">
          {w}
        </p>
      ))}
      {older.length > 0 && (
        <div className="dna-actions">
          <label htmlFor={`back-${v.name}`} className="small muted">
            Roll back to
          </label>
          <select id={`back-${v.name}`} value={back} onChange={(e) => setBack(e.target.value)}>
            <option value="">version…</option>
            {older.map((n) => (
              <option key={n} value={String(n)}>
                {n}
              </option>
            ))}
          </select>
          <button className="btn btn-small" disabled={busy || !back} onClick={() => act("rollback", { version: Number(back) })}>
            Roll back
          </button>
        </div>
      )}
      {notice && (
        <p className="muted small" role="status">
          {notice}
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

type Library =
  | { state: "loading" }
  | { state: "unavailable" }
  | { state: "error"; error: string }
  | { state: "ready"; tenants: TenantView[]; skills: SkillSummary[] };

/** The skills view. `reviewNonce`: each new value opens the review queue. */
export function Skills({ reviewNonce = 0 }: { reviewNonce?: number }) {
  const { rpc } = useEnv();
  const version = useSkillsVersion();
  const [nonce, setNonce] = useState(0);
  const [lib, setLib] = useState<Library>({ state: "loading" });
  // The panel: the review queue (null) or a selected skill.
  const [selected, setSelected] = useState<string | null>(null);
  const pending = usePendingSkills(nonce);
  const seenReview = useRef(reviewNonce);
  useEffect(() => {
    if (reviewNonce === seenReview.current) return;
    seenReview.current = reviewNonce;
    setSelected(null);
  }, [reviewNonce]);
  useEffect(() => {
    let live = true;
    (async () => {
      const t = await skillTenants(rpc);
      if (!t.available) return live && setLib({ state: "unavailable" });
      const skills = (await skillList(rpc)) ?? [];
      if (live) setLib({ state: "ready", tenants: t.tenants, skills });
    })().catch((e) => live && setLib({ state: "error", error: skillErrorText(e) }));
    return () => {
      live = false;
    };
  }, [rpc, version, nonce]);
  const groups = useMemo(() => (lib.state === "ready" ? groupByScope(lib.skills) : []), [lib]);
  const reload = () => setNonce((n) => n + 1);

  let body: React.ReactNode;
  if (lib.state === "loading") body = <p className="muted">Reading the skill library…</p>;
  else if (lib.state === "unavailable") body = <p className="muted">This daemon predates skills. Update Mirante to use them.</p>;
  else if (lib.state === "error") body = <p className="error">Skills unavailable: {lib.error}</p>;
  else
    body = (
      <>
        <section aria-label="Clients" className="dna-section">
          <h2>Clients</h2>
          {lib.tenants.length === 0 ? (
            <p className="muted">No client yet: a task's repo origin names its client when you create the task.</p>
          ) : (
            <ul className="dna-list">
              {lib.tenants.map((t) => (
                <TenantRow key={t.tenant} t={t} onChanged={reload} />
              ))}
            </ul>
          )}
        </section>
        <section aria-label="Skill library" className="dna-section">
          <h2>Library</h2>
          {groups.length === 0 ? (
            <p className="muted">No skills yet. Accepted proposals become skills here.</p>
          ) : (
            groups.map((g) => (
              <div key={g.scope} className="dna-section">
                <h3>{scopeLabel(g.scope)}</h3>
                <ul className="dna-list">
                  {g.skills.map((s) => (
                    <SkillRow key={s.name} s={s} selected={selected === s.name} onSelect={() => setSelected(s.name)} />
                  ))}
                </ul>
              </div>
            ))
          )}
        </section>
      </>
    );

  return (
    <main className="brain" aria-label="Skills">
      <section className="brain-main">
        <header className="feed-head">
          <h1>Skills</h1>
          <span className="muted small">Reviewed instructions Mirante gives each matching session</span>
          <button className="btn btn-small" aria-pressed={selected === null} onClick={() => setSelected(null)}>
            {pending === null ? "Review" : `Review (${pending})`}
          </button>
        </header>
        {body}
      </section>
      <aside className="brain-panel" aria-label="Selected">
        {selected ? (
          <SkillPanel key={selected} name={selected} version={version} onChanged={reload} />
        ) : (
          <SkillProposals onDecided={reload} pending={pending} />
        )}
      </aside>
    </main>
  );
}
