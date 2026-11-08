// The memory review queue of one repo: proposals an agent (or `mirante memory propose`)
// made, waiting for a person. Nothing is ever accepted automatically. Title and body are
// editable before Accept (the daemon redacts them again); Reject takes an optional note.
import { useCallback, useEffect, useRef, useState } from "react";
import { useEnv, useStore } from "../env";
import { DaemonError } from "../lib/daemon";
import { citation, dnaErrorText, proposals, review, shortCommit } from "../lib/dna";
import type { ProposalView } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { dnaRev } from "../store/state";
import { redactedText } from "./MemoryTab";

export const PAGE = 50;

function ProposalCard({ p, onDecided }: { p: ProposalView; onDecided(conflict: boolean): void }) {
  const { rpc } = useEnv();
  const s = useStore();
  const [title, setTitle] = useState(p.title);
  const [body, setBody] = useState(p.body);
  const [rejecting, setRejecting] = useState(false);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);
  const branch = p.task_id ? (s.tasks[p.task_id]?.branch ?? p.task_id) : null;
  const decide = async (accept: boolean) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      const params: Parameters<typeof review>[1] = { id: p.id, accept };
      if (accept && title !== p.title) params.title = title;
      if (accept && body !== p.body) params.body = body;
      if (!accept && note.trim()) params.note = note.trim();
      await review(rpc, params);
      onDecided(false);
    } catch (e) {
      if (e instanceof DaemonError && e.code === RpcCode.CONFLICT) onDecided(true);
      else setError(dnaErrorText(e));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  const ids = `prop-${p.id}`;
  return (
    <article className="dna-proposal" aria-label={p.title}>
      <div className="dna-note-head">
        <span className="tag">{p.kind}</span>
        <span className="muted small">{branch ? `from task ${branch}` : p.origin === "cli" ? "from the CLI" : `from ${p.origin}`}</span>
        {p.redactions > 0 && <span className="tag tag-warn">{redactedText(p.redactions)}</span>}
      </div>
      <label htmlFor={`${ids}-title`} className="small muted">
        Title
      </label>
      <input id={`${ids}-title`} className="dna-input" value={title} maxLength={120} onChange={(e) => setTitle(e.target.value)} />
      <label htmlFor={`${ids}-body`} className="small muted">
        Body
      </label>
      <textarea id={`${ids}-body`} className="dna-input" rows={5} value={body} onChange={(e) => setBody(e.target.value)} />
      <ul className="dna-anchors" aria-label="Cites">
        {p.anchors.map((a, i) => (
          <li key={i} className="dna-anchor">
            <code>{citation(a)}</code> <span className="muted">@{shortCommit(a.commit)}</span>
          </li>
        ))}
      </ul>
      {rejecting ? (
        <div className="dna-actions">
          <label htmlFor={`${ids}-note`} className="sr-only">
            Why (optional)
          </label>
          <input
            id={`${ids}-note`}
            className="dna-input"
            placeholder="Why (optional)"
            value={note}
            onChange={(e) => setNote(e.target.value)}
          />
          <button className="btn btn-small btn-warn" disabled={busy} onClick={() => decide(false)}>
            Reject proposal
          </button>
          <button className="btn btn-small" disabled={busy} onClick={() => setRejecting(false)}>
            Cancel
          </button>
        </div>
      ) : (
        <div className="dna-actions">
          <button className="btn btn-small btn-primary" disabled={busy} onClick={() => decide(true)}>
            Accept
          </button>
          <button className="btn btn-small" disabled={busy} onClick={() => setRejecting(true)}>
            Reject
          </button>
        </div>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </article>
  );
}

type Loaded = { state: "loading" } | { state: "ready"; list: ProposalView[]; more: boolean } | { state: "error"; error: string };

/** The pending proposals of `repo`, oldest first, `PAGE` at a time. */
export function ReviewQueue({ repo }: { repo: string }) {
  const { rpc } = useEnv();
  const version = dnaRev(useStore(), repo);
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [nonce, setNonce] = useState(0);
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    proposals(rpc, { repo, limit: PAGE })
      .then((list) => live && setLoaded({ state: "ready", list: list ?? [], more: (list ?? []).length === PAGE }))
      .catch((e) => live && setLoaded({ state: "error", error: dnaErrorText(e) }));
    return () => {
      live = false;
    };
  }, [rpc, repo, version, nonce]);
  const refresh = useCallback(() => setNonce((n) => n + 1), []);
  const more = async () => {
    if (loaded.state !== "ready" || loaded.list.length === 0) return;
    const after = loaded.list[loaded.list.length - 1].id;
    try {
      const next = (await proposals(rpc, { repo, limit: PAGE, after })) ?? [];
      setLoaded({ state: "ready", list: [...loaded.list, ...next.filter((n) => !loaded.list.some((o) => o.id === n.id))], more: next.length === PAGE });
    } catch (e) {
      setLoaded({ state: "error", error: dnaErrorText(e) });
    }
  };
  const decided = (id: string, conflict: boolean) => {
    setNotice(conflict ? "That proposal was already decided elsewhere; the queue was refreshed." : null);
    if (loaded.state === "ready") setLoaded({ ...loaded, list: loaded.list.filter((p) => p.id !== id) });
    refresh();
  };
  return (
    <section className="dna-review" aria-label="Memory review queue">
      <h2>Review memory</h2>
      <p className="muted small">
        Proposals become notes in <code>.mirante/memory/</code> only when you accept them. Edit the title or body first if
        needed.
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
          <>
            {loaded.list.map((p) => (
              <ProposalCard key={p.id} p={p} onDecided={(c) => decided(p.id, c)} />
            ))}
            {loaded.more && (
              <button className="btn btn-small" onClick={more}>
                Load more
              </button>
            )}
          </>
        ))}
    </section>
  );
}
