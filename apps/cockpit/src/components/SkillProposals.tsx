// The skills review queue: proposals the miner, an agent (MCP `skill_propose`) or a person
// (`skill.add`) made, waiting for a person. Nothing is ever accepted automatically. A card
// shows everything an Accept would approve: the whole body and every bundled file (as text
// nodes, long lines wrapped, nothing cut), where it applies, how it is given to the agent
// (always on goes into the system prompt) and any cross-client warning. Long content may be
// collapsed to scan the queue, but Accept is off while anything is hidden.
import { useEffect, useMemo, useRef, useState } from "react";
import { useEnv, useSelect } from "../env";
import { DaemonError } from "../lib/daemon";
import { lineDiff, splitLines } from "../lib/lineDiff";
import type { Finding, SkillEdits, SkillProposalView, SkillView } from "../lib/protocol";
import { M, RpcCode } from "../lib/protocol";
import {
  activationLabel,
  findingText,
  hasLintErrors,
  scopeLabel,
  skillBody,
  skillErrorText,
  skillGet,
  skillLint,
  skillProposals,
  skillReview,
  taskSkills,
  wideningWarnings,
} from "../lib/skills";
import { useSkillsVersion } from "../lib/useSkills";

/** Wait after the last edit before linting it again. */
export const LINT_DEBOUNCE_MS = 300;
/** At most this many examples of what the operator said. */
export const MAX_EXAMPLES = 5;
/** Content past this many lines (or any bundled file) can be collapsed. */
const LONG_LINES = 40;

/** Lint fields the daemon re-checks on `skill.lint` (bundled files keep their findings). */
const SKILL_MD_FIELDS = new Set(["SKILL.md", "description", "name", "frontmatter"]);

function originText(p: SkillProposalView, branch: string | null): string {
  if (p.origin === "miner") {
    const tasks = new Set(p.evidence.map((e) => e.task)).size;
    return `Found in your messages: repeated in ${tasks} ${tasks === 1 ? "task" : "tasks"}`;
  }
  if (p.origin === "agent") return branch ? `from task ${branch}` : "from an agent";
  if (p.origin === "import") return "imported by you";
  return "written by you (CLI)";
}

/** The scopes a person can pick for a proposal: as proposed, the client's org, personal. */
function scopeChoices(p: SkillProposalView): string[] {
  const out = [p.scope];
  if (p.tenant && p.tenant !== "personal") out.push(`org:${p.tenant}`);
  out.push("personal");
  return [...new Set(out)];
}

/** The message "Refine with agent" queues: the proposal as data, and what to do with it. */
export function refineRequest(p: SkillProposalView, body: string): string {
  const how = p.kind === "edit" ? `with base "${p.name}"` : `named "${p.name}"`;
  return [
    `Operator request: refine the proposed skill "${p.name}" (proposal ${p.id}). Improve its description and body so it`,
    `reads as a clear, reusable instruction for future tasks in this repository, then call skill_propose ${how}.`,
    "A person reviews every proposal; do not change anything else.",
    "",
    `Description: ${p.description}`,
    "Body:",
    body,
  ].join("\n");
}

function Diff({ before, after, version }: { before: string; after: string; version: number }) {
  const ops = useMemo(() => lineDiff(before, after), [before, after]);
  return (
    <ol className="skill-diff" aria-label={`Changes from v${version}`}>
      {ops.map((o, i) =>
        "same" in o ? (
          <li key={i} className="diff-same">{`  ${o.same}`}</li>
        ) : "del" in o ? (
          <li key={i} className="diff-del">{`- ${o.del}`}</li>
        ) : (
          <li key={i} className="diff-add">{`+ ${o.add}`}</li>
        ),
      )}
    </ol>
  );
}

function Refine({ p, body }: { p: SkillProposalView; body: string }) {
  const { rpc } = useEnv();
  const allTasks = useSelect((s) => s.tasks);
  const [open, setOpen] = useState(false);
  const [tasks, setTasks] = useState<{ id: string; label: string }[] | null>(null);
  const [pick, setPick] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const inFlight = useRef(false);
  const candidates = useMemo(
    () => Object.values(allTasks).filter((t) => t.lifecycle !== "archived" && t.live),
    [allTasks],
  );
  useEffect(() => {
    if (!open) return;
    let live = true;
    // Only tasks whose confirmed origin is this proposal's client, with learning on (their
    // agent has skill_propose): another client's agent never sees this text.
    Promise.all(candidates.map((t) => taskSkills(rpc, t.id).then((ts) => ({ t, ts }), () => ({ t, ts: null }))))
      .then((all) => {
        if (!live) return;
        const ok = all
          .filter(({ ts }) => ts && ts.confirmed && ts.learning && ts.tenant === p.tenant)
          .map(({ t }) => ({ id: t.id, label: `${t.branch} (@${t.agent})` }));
        setTasks(ok);
        setPick(ok[0]?.id ?? "");
      });
    return () => {
      live = false;
    };
  }, [open, candidates, rpc, p.tenant]);
  const send = async () => {
    if (inFlight.current || !pick) return;
    inFlight.current = true;
    try {
      await rpc(M.messageSend, { task: pick, text: refineRequest(p, body), mode: "queue" });
      setNote("Queued for the agent. Its answer comes back here as a new proposal.");
    } catch (e) {
      setNote(skillErrorText(e));
    } finally {
      inFlight.current = false;
    }
  };
  if (!open) {
    return (
      <button className="btn btn-small" onClick={() => setOpen(true)}>
        Refine with agent
      </button>
    );
  }
  const id = `refine-${p.id}`;
  return (
    <div className="dna-actions" role="group" aria-label="Refine with agent">
      {tasks === null ? (
        <span className="muted small">Finding tasks of {p.tenant}…</span>
      ) : tasks.length === 0 ? (
        <span className="muted small">
          No live task of {p.tenant} with learning on and a confirmed origin: only such a task's agent may see this text.
        </span>
      ) : (
        <>
          <label htmlFor={id} className="small muted">
            Task to ask
          </label>
          <select id={id} value={pick} onChange={(e) => setPick(e.target.value)}>
            {tasks.map((t) => (
              <option key={t.id} value={t.id}>
                {t.label}
              </option>
            ))}
          </select>
          <button className="btn btn-small" onClick={send}>
            Queue the request
          </button>
        </>
      )}
      <button className="btn btn-small" onClick={() => setOpen(false)}>
        Close
      </button>
      {note && (
        <span className="muted small" role="status">
          {note}
        </span>
      )}
    </div>
  );
}

type Current = { state: "none" } | { state: "loading" } | { state: "ready"; view: SkillView } | { state: "error"; error: string };

function ProposalCard({ p, onDecided }: { p: SkillProposalView; onDecided(text: string, conflict: boolean): void }) {
  const { rpc } = useEnv();
  const branchOf = useSelect((s) => (p.task_id ? s.tasks[p.task_id]?.branch : undefined));
  const [name, setName] = useState(p.name);
  const [description, setDescription] = useState(p.description);
  const [body, setBody] = useState(p.body);
  const [scope, setScope] = useState(p.scope);
  const [activation, setActivation] = useState(p.activation);
  const [editing, setEditing] = useState(false);
  const [collapsed, setCollapsed] = useState(false);
  const [findings, setFindings] = useState<Finding[]>(p.lint);
  const [linting, setLinting] = useState(false);
  const [current, setCurrent] = useState<Current>({ state: p.kind === "edit" ? "loading" : "none" });
  const [currentNonce, setCurrentNonce] = useState(0);
  const [rejecting, setRejecting] = useState(false);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);
  const branch = p.task_id ? (branchOf ?? p.task_id) : null;

  // The version this edit is measured against (reloaded when the daemon says it moved).
  useEffect(() => {
    if (p.kind !== "edit") return;
    let live = true;
    skillGet(rpc, p.name)
      .then((view) => live && setCurrent({ state: "ready", view }))
      .catch((e) => live && setCurrent({ state: "error", error: skillErrorText(e) }));
    return () => {
      live = false;
    };
  }, [rpc, p.kind, p.name, currentNonce]);

  // Edits are linted again after a pause; until the answer, Accept waits.
  const edited = name !== p.name || description !== p.description || body !== p.body || scope !== p.scope;
  useEffect(() => {
    if (!edited) {
      setFindings(p.lint);
      setLinting(false);
      return;
    }
    let live = true;
    setLinting(true);
    const timer = setTimeout(() => {
      skillLint(rpc, { name, description, body, scope })
        .then((f) => {
          if (!live) return;
          // Bundled files are not sent again: their findings stay.
          const files = p.lint.filter((x) => !SKILL_MD_FIELDS.has(x.file));
          setFindings([...(f ?? []), ...files]);
        })
        .catch((e) => live && setFindings([{ rule: "lint", severity: "error", file: "SKILL.md", line: 0, message: skillErrorText(e) }]))
        .finally(() => live && setLinting(false));
    }, LINT_DEBOUNCE_MS);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [rpc, edited, name, description, body, scope, p.lint]);

  const long = splitLines(body).length > LONG_LINES || p.files.length > 0;
  // An edit is accepted only with its diff on screen.
  const blocked = hasLintErrors(findings) || linting || collapsed || (p.kind === "edit" && current.state !== "ready");
  const widen = scope !== p.scope ? wideningWarnings(scope, p.tenant === "personal" ? null : p.tenant) : [];
  const warnings = [...new Set([...p.warnings, ...widen])];

  const decide = async (accept: boolean) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      const params: Parameters<typeof skillReview>[1] = { id: p.id, accept };
      if (accept) {
        const edits: SkillEdits = {};
        if (name !== p.name) edits.name = name;
        if (description !== p.description) edits.description = description;
        if (body !== p.body) edits.body = body;
        if (scope !== p.scope) edits.scope = scope;
        if (activation !== p.activation) edits.activation = activation as SkillEdits["activation"];
        if (Object.keys(edits).length > 0) params.edits = edits;
      } else if (note.trim()) params.note = note.trim();
      const r = await skillReview(rpc, params);
      onDecided(r?.status === "accepted" ? `Accepted as ${r.name} v${r.version ?? "?"}.` : "Rejected.", false);
    } catch (e) {
      if (e instanceof DaemonError && e.code === RpcCode.CONFLICT) {
        setError(
          "The skill changed since this proposal (or it was decided elsewhere): the diff was reloaded. Read it again before accepting.",
        );
        setCurrent({ state: "loading" });
        setCurrentNonce((n) => n + 1);
        onDecided("", true);
      } else setError(skillErrorText(e));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  const ids = `sp-${p.id}`;
  const examples = p.evidence.slice(0, MAX_EXAMPLES);
  return (
    <article className="dna-proposal skill-proposal" aria-label={`Proposal ${p.name}`}>
      <div className="dna-note-head">
        <span className="tag">{p.kind === "edit" ? "edit" : "new skill"}</span>
        <strong>{p.name}</strong>
        <span className="muted small">{originText(p, branch)}</span>
        {p.redactions > 0 && <span className="tag tag-warn">{p.redactions === 1 ? "1 secret was redacted" : `${p.redactions} secrets were redacted`}</span>}
      </div>
      <p className="small">
        <strong>{scopeLabel(scope)}</strong> · Client {p.tenant === "personal" ? "none (personal)" : p.tenant}
      </p>
      <p className="small">{activationLabel(activation)}</p>
      {warnings.length > 0 && (
        <div className="warn skill-warnings" role="alert">
          {warnings.map((w) => (
            <p key={w}>{w}</p>
          ))}
        </div>
      )}
      {examples.length > 0 && (
        <>
          <p className="small muted">What you said (redacted):</p>
          <ul className="skill-examples" aria-label="Examples">
            {examples.map((e, i) => (
              <li key={i}>{e.text}</li>
            ))}
          </ul>
        </>
      )}
      <label htmlFor={`${ids}-name`} className="small muted">
        Name
      </label>
      <input id={`${ids}-name`} className="dna-input" value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
      <label htmlFor={`${ids}-desc`} className="small muted">
        Description
      </label>
      <input id={`${ids}-desc`} className="dna-input" value={description} maxLength={1024} onChange={(e) => setDescription(e.target.value)} />
      <div className="dna-actions">
        <label htmlFor={`${ids}-scope`} className="small muted">
          Scope
        </label>
        <select id={`${ids}-scope`} value={scope} onChange={(e) => setScope(e.target.value)}>
          {scopeChoices(p).map((c) => (
            <option key={c} value={c}>
              {c}
            </option>
          ))}
        </select>
        <label htmlFor={`${ids}-act`} className="small muted">
          Activation
        </label>
        <select id={`${ids}-act`} value={activation} onChange={(e) => setActivation(e.target.value)}>
          <option value="always">always</option>
          <option value="on_demand">on_demand</option>
        </select>
      </div>
      <div className="dna-actions">
        <span className="small muted">SKILL.md body</span>
        <button className="btn btn-small" onClick={() => setEditing((v) => !v)} disabled={collapsed}>
          {editing ? "Done editing" : "Edit"}
        </button>
        {long && (
          <button className="btn btn-small" onClick={() => setCollapsed((v) => !v)}>
            {collapsed ? "Expand" : "Collapse"}
          </button>
        )}
      </div>
      {collapsed ? (
        <p className="warn small">
          {splitLines(body).length} lines and {p.files.length} bundled {p.files.length === 1 ? "file" : "files"} hidden. Expand to read
          everything before accepting.
        </p>
      ) : (
        <>
          {editing ? (
            <>
              <label htmlFor={`${ids}-body`} className="sr-only">
                Body
              </label>
              <textarea
                id={`${ids}-body`}
                className="dna-input skill-body"
                rows={Math.max(5, splitLines(body).length + 1)}
                value={body}
                onChange={(e) => setBody(e.target.value)}
              />
            </>
          ) : (
            <pre className="dna-body skill-body" aria-label="Proposed SKILL.md body">
              {body}
            </pre>
          )}
          {p.files.map((f) => (
            <div key={f.path}>
              <p className="small">
                Bundled file <code>{f.path}</code>
              </p>
              <pre className="dna-body skill-body" aria-label={`File ${f.path}`}>
                {f.text}
              </pre>
            </div>
          ))}
        </>
      )}
      {p.kind === "edit" &&
        (current.state === "ready" ? (
          <>
            {p.base_hash && p.base_hash !== current.view.manifest_hash && (
              <p className="warn small">The skill moved on since this proposal: Accept will be refused. The diff is against the current v{current.view.version}.</p>
            )}
            <Diff before={skillBody(current.view.skill_md)} after={body} version={current.view.version} />
          </>
        ) : current.state === "error" ? (
          <p className="error">Current version unavailable: {current.error}</p>
        ) : (
          <p className="muted small">Reading the current version…</p>
        ))}
      {findings.length > 0 && (
        <ul className="dna-anchors" aria-label="Lint">
          {findings.map((f, i) => (
            <li key={i} className={f.severity === "error" ? "error" : "warn"}>
              {findingText(f)}
            </li>
          ))}
        </ul>
      )}
      {rejecting ? (
        <div className="dna-actions">
          <label htmlFor={`${ids}-note`} className="sr-only">
            Why (optional)
          </label>
          <input id={`${ids}-note`} className="dna-input" placeholder="Why (optional)" value={note} onChange={(e) => setNote(e.target.value)} />
          <button className="btn btn-small btn-warn" disabled={busy} onClick={() => decide(false)}>
            Reject proposal
          </button>
          <button className="btn btn-small" onClick={() => setRejecting(false)}>
            Cancel
          </button>
        </div>
      ) : (
        <div className="dna-actions">
          <button className="btn btn-small btn-primary" disabled={busy || blocked} onClick={() => decide(true)}>
            Accept
          </button>
          <button className="btn btn-small" disabled={busy} onClick={() => setRejecting(true)}>
            Reject
          </button>
          <Refine p={p} body={body} />
        </div>
      )}
      {hasLintErrors(findings) && <p className="small warn">Fix the lint errors to accept.</p>}
      {error && (
        <p className="error" role="status">
          {error}
        </p>
      )}
    </article>
  );
}

type Loaded = { state: "loading" } | { state: "ready"; list: SkillProposalView[] } | { state: "error"; error: string };

/** The pending skill proposals of every client, oldest first. */
export function SkillProposals({ onDecided }: { onDecided?(): void }) {
  const { rpc } = useEnv();
  const version = useSkillsVersion();
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [nonce, setNonce] = useState(0);
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    skillProposals(rpc, { status: "pending", limit: 200 })
      .then((list) => live && setLoaded({ state: "ready", list: list ?? [] }))
      .catch((e) => live && setLoaded({ state: "error", error: skillErrorText(e) }));
    return () => {
      live = false;
    };
  }, [rpc, version, nonce]);
  const decided = (id: string, text: string, conflict: boolean) => {
    if (conflict) {
      setNonce((n) => n + 1);
      return;
    }
    setNotice(text);
    setLoaded((cur) => (cur.state === "ready" ? { ...cur, list: cur.list.filter((p) => p.id !== id) } : cur));
    onDecided?.();
  };
  return (
    <section className="dna-review" aria-label="Skill review queue">
      <h2>Review skills</h2>
      <p className="muted small">
        A proposal becomes a signed skill only when you accept it. Read all of it: an always-on skill goes into the system prompt of
        every matching session.
      </p>
      {notice && (
        <p className="muted small" role="status">
          {notice}
        </p>
      )}
      {loaded.state === "loading" && <p className="muted">Reading the queue…</p>}
      {loaded.state === "error" && <p className="error">Review queue unavailable: {loaded.error}</p>}
      {loaded.state === "ready" &&
        (loaded.list.length === 0 ? (
          <p className="muted">Nothing waits for review.</p>
        ) : (
          loaded.list.map((p) => <ProposalCard key={p.id} p={p} onDecided={(t, c) => decided(p.id, t, c)} />)
        ))}
    </section>
  );
}
