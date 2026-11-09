// The skills view: the user-level library grouped by scope (personal, each org, each repo)
// with verification and 30-day usage, the clients (tenants) with their opt-in learning, and
// the selected skill beside it. Skill text is rendered as text nodes only: a SKILL.md is
// instructions for an agent, never markup for the cockpit.
import { useEffect, useMemo, useRef, useState } from "react";
import { useBoot, useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import type { SkillSummary, SkillView, TenantView } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { t as tr, tn, useT } from "../lib/i18n";
import {
  activationLabel,
  activationShort,
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
import { ErrorRetry } from "./ErrorRetry";
import { Unsupported } from "./Restart";

const KIND_ORDER = { personal: 0, org: 1, repo: 2 } as const;

/** The library as groups of one scope each: personal, then orgs, then repos (by name). */
export function groupByScope(list: SkillSummary[]): { scope: string; skills: SkillSummary[] }[] {
  const by = new Map<string, SkillSummary[]>();
  for (const s of list) by.set(s.scope, [...(by.get(s.scope) ?? []), s]);
  return [...by.entries()]
    .sort(([a], [b]) => KIND_ORDER[scopeKind(a)] - KIND_ORDER[scopeKind(b)] || a.localeCompare(b))
    .map(([scope, skills]) => ({ scope, skills: skills.sort((x, y) => x.name.localeCompare(y.name)) }));
}

function TenantRow({ t, onChanged }: { t: TenantView; onChanged(): void }) {
  const { rpc } = useEnv();
  useT();
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
      return tr("skills.mined", { proposals: tn(r.proposed, "skills.newProposal.one", "skills.newProposal.many"), updated: r.updated, skipped: r.skipped });
    });
  return (
    <li className="dna-row skill-tenant" role="group" aria-label={t.tenant}>
      <div className="dna-note-head">
        <strong>{t.tenant}</strong>
        <span className={`tag ${t.learning ? "tag-on" : ""}`}>{tr(t.learning ? "skills.learningOn" : "skills.learningOff")}</span>
        <span className="muted small">
          {tn(t.tasks, "skills.tasks.one", "skills.tasks.many")} · {tn(t.skills, "skills.skills.one", "skills.skills.many")}
        </span>
      </div>
      <p className="muted small">{tr("skills.learnExplain")}</p>
      <div className="dna-actions">
        <button className="btn btn-small" disabled={busy} onClick={learn}>
          {tr(t.learning ? "skills.stopLearning" : "skills.startLearning", { client: t.tenant })}
        </button>
        {t.learning && (
          <button className="btn btn-small" disabled={busy} onClick={mine}>
            {tr("skills.findRepeated")}
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
  const t = useT();
  return (
    <li className="dna-row">
      <button className="link skill-row" aria-pressed={selected} onClick={onSelect}>
        <span className="dna-title">{s.name}</span>{" "}
        <span className="muted small">
          v{s.version} · {activationShort(s.activation)} ·{" "}
          <span className={s.verified ? "" : "skill-refused"}>{t(s.verified ? "skills.verified" : "skills.refused")}</span>
          {s.status === "retired" ? ` · ${t("skills.retired")}` : ""} · {t("skills.rowUsage", { i: s.injected_30d, u: s.used_30d })}
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
  const t = useT();
  const boot = useBoot();
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
  }, [rpc, name, version, nonce, boot]);
  if (loaded.state === "loading") return <p className="muted">{t("skills.reading", { name })}</p>;
  if (loaded.state === "error") return <ErrorRetry text={t("skills.unavailable", { error: loaded.error })} onRetry={() => setNonce((n) => n + 1)} />;
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
        setError(t("skills.conflict"));
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
      return action === "retire" ? t("skills.retiredDone") : action === "restore" ? t("skills.restoredDone") : t("skills.nowVersion", { v: next.version });
    });
  const reReview = () =>
    run(async () => {
      // The daemon reads this version's own folder; the cockpit never names a path.
      const p = await skillRereview(rpc, v.name, v.shown_version);
      onChanged();
      return t("skills.requeued", { proposal: p?.id ? t("skills.requeuedId", { id: p.id }) : "" });
    });
  const learnedIn = v.manifest?.provenance.learned_in ?? null;
  const scopeChanged = scope.trim() !== "" && scope.trim() !== v.scope;
  const widen = scopeChanged ? wideningWarnings(scope.trim(), learnedIn ?? scopeTenant(v.scope)) : [];
  const older = v.versions.filter((n) => n < v.version);
  const body = skillBody(v.skill_md);
  return (
    <section className="skill-panel" aria-label={t("skills.panelAria", { name: v.name })}>
      <div className="dna-note-head">
        <h3>{v.name}</h3>
        <span className="tag">v{v.shown_version}</span>
        <span className={`tag ${v.verified ? "tag-on" : "tag-warn"}`}>{t(v.verified ? "skills.verified" : "skills.refused")}</span>
        {v.status === "retired" && <span className="tag">{t("skills.retired")}</span>}
      </div>
      <p>{v.description}</p>
      <p className="small">
        <strong>{scopeLabel(v.scope)}</strong> · {activationLabel(v.activation)}
      </p>
      <p className="muted small">{t("skills.usage30", { i: v.injected_30d, u: v.used_30d })}</p>
      {v.warnings.map((w) => (
        <p key={w} className="warn small">
          {w}
        </p>
      ))}
      {!v.verified ? (
        <div className="skill-refusal">
          <p className="warn">{t("skills.refusedWhy", { why: v.reason ?? t("skills.notVerify") })}</p>
          <p className="small">{t("skills.refusalExplain")}</p>
          <button className="btn btn-small" disabled={busy} onClick={reReview}>
            {t("skills.rereview")}
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
            {t("skills.restore")}
          </button>
        ) : (
          <button className="btn btn-small btn-warn" disabled={busy} onClick={() => act("retire")}>
            {t("skills.retire")}
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
          {t("skills.newScope")}
        </label>
        <input
          id={`scope-${v.name}`}
          className="dna-input"
          placeholder={t("skills.scopePlaceholder")}
          value={scope}
          onChange={(e) => setScope(e.target.value)}
        />
        <button className="btn btn-small" type="submit" disabled={busy || !scopeChanged}>
          {t("skills.rescope")}
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
            {t("skills.rollBackTo")}
          </label>
          <select id={`back-${v.name}`} value={back} onChange={(e) => setBack(e.target.value)}>
            <option value="">{t("skills.versionPick")}</option>
            {older.map((n) => (
              <option key={n} value={String(n)}>
                {n}
              </option>
            ))}
          </select>
          <button className="btn btn-small" disabled={busy || !back} onClick={() => act("rollback", { version: Number(back) })}>
            {t("skills.rollBack")}
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
  const t = useT();
  const boot = useBoot();
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
      const got = await skillTenants(rpc);
      if (!got.available) return live && setLib({ state: "unavailable" });
      const skills = (await skillList(rpc)) ?? [];
      if (live) setLib({ state: "ready", tenants: got.tenants, skills });
    })().catch((e) => live && setLib({ state: "error", error: skillErrorText(e) }));
    return () => {
      live = false;
    };
  }, [rpc, version, nonce, boot]);
  const groups = useMemo(() => (lib.state === "ready" ? groupByScope(lib.skills) : []), [lib]);
  const reload = () => setNonce((n) => n + 1);

  let body: React.ReactNode;
  if (lib.state === "loading") body = <p className="muted">{t("skills.loadingLibrary")}</p>;
  else if (lib.state === "unavailable") body = <Unsupported text={t("skills.predates")} />;
  else if (lib.state === "error") body = <ErrorRetry text={t("skills.libUnavailable", { error: lib.error })} onRetry={reload} />;
  else
    body = (
      <>
        <section aria-label={t("skills.clients")} className="dna-section">
          <h2>{t("skills.clients")}</h2>
          {lib.tenants.length === 0 ? (
            <p className="muted">{t("skills.noClient")}</p>
          ) : (
            <ul className="dna-list">
              {lib.tenants.map((x) => (
                <TenantRow key={x.tenant} t={x} onChanged={reload} />
              ))}
            </ul>
          )}
        </section>
        <section aria-label={t("skills.libraryAria")} className="dna-section">
          <h2>{t("skills.library")}</h2>
          {groups.length === 0 ? (
            <p className="muted">{t("skills.noSkills")}</p>
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
    <main className="brain" aria-label={t("skills.title")}>
      <section className="brain-main">
        <header className="feed-head">
          <h1>{t("skills.title")}</h1>
          <span className="muted small">{t("skills.subtitle")}</span>
          <button className="btn btn-small" aria-pressed={selected === null} onClick={() => setSelected(null)}>
            {pending === null ? t("skills.review") : t("skills.reviewN", { n: pending })}
          </button>
        </header>
        {body}
      </section>
      <aside className="brain-panel" aria-label={t("skills.selected")}>
        {selected ? (
          <SkillPanel key={selected} name={selected} version={version} onChanged={reload} />
        ) : (
          <SkillProposals onDecided={reload} pending={pending} />
        )}
      </aside>
    </main>
  );
}
