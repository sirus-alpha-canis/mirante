// One memory note for a person: title, kind, state, reviewed badge, citations, and the body
// as plain text with [[links]] as buttons. Never HTML: the body is untrusted (an agent may
// have proposed it, a teammate may have pushed it) and is rendered as React text nodes.
import { useEffect, useRef, useState } from "react";
import { useBoot, useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import {
  citation,
  type DnaAt,
  dnaErrorText,
  dnaGet,
  linkParts,
  noteAction,
  type NoteActionKind,
  shortCommit,
  stateLabel,
} from "../lib/dna";
import type { AnchorView, NoteSummary, NoteView as Note } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { ErrorRetry } from "./ErrorRetry";

/** "stale", "unreviewed", "retired"…: the class that colors a note's state. */
export function stateClass(n: Pick<NoteSummary, "state" | "reviewed" | "status">): string {
  if (n.status === "retired") return "dna-retired";
  if (n.state === "stale") return "dna-stale";
  if (n.state === "unverifiable" || n.state === "unchecked") return "dna-unverifiable";
  return n.reviewed ? "dna-fresh" : "dna-unreviewed";
}

export function StatePill({ n }: { n: Pick<NoteSummary, "state" | "reviewed" | "status"> }) {
  return <span className={`dna-pill ${stateClass(n)}`}>{stateLabel(n)}</span>;
}

export function AnchorLine({ a }: { a: AnchorView }) {
  return (
    <li className="dna-anchor">
      <code>{citation(a)}</code> <span className="muted">@{shortCommit(a.commit)}</span>{" "}
      <span className={`dna-anchor-state dna-a-${a.state}`}>{a.state}</span>
      {a.state === "relocated" && a.now_start != null && (
        <span className="muted">
          {" "}
          now L{a.now_start}-{a.now_end}
        </span>
      )}
      {a.reason && <span className="muted small"> {a.reason}</span>}
    </li>
  );
}

/** The body as text nodes and link buttons (`white-space: pre-wrap` keeps its lines). */
export function NoteBody({ body, onOpen }: { body: string; onOpen(slug: string): void }) {
  return (
    <div className="dna-body">
      {linkParts(body).map((p, i) =>
        "link" in p ? (
          <button key={i} type="button" className="dna-link" title={`Open ${p.link}`} onClick={() => onOpen(p.link)}>
            {p.label}
          </button>
        ) : (
          <span key={i}>{p.text}</span>
        ),
      )}
    </div>
  );
}

type Loaded =
  | { state: "loading" }
  /** `stale`: the last refetch failed; the note shown is the one loaded before. */
  | { state: "ready"; note: Note; stale?: string }
  | { state: "error"; error: string };

/**
 * Loads `slug` (from `at`: a repo or a task's repo) and refetches when `version` changes.
 * Confirm and Retire send the `content_hash` of the note shown; if the file changed since,
 * the daemon refuses and the person reloads.
 */
export function NoteView({
  at,
  repo,
  slug,
  version,
  onOpen,
}: {
  at: DnaAt;
  /** The checkout `dna.note_action` names. */
  repo: string;
  slug: string;
  version: number;
  onOpen(slug: string): void;
}) {
  const { rpc } = useEnv();
  const boot = useBoot();
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [reload, setReload] = useState(0);
  const [error, setError] = useState<{ text: string; reload: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const atKey = JSON.stringify(at);
  // Another note (or place): never show the previous one while it loads.
  const shownFor = useRef(`${atKey} ${slug}`);
  if (shownFor.current !== `${atKey} ${slug}`) {
    shownFor.current = `${atKey} ${slug}`;
    if (loaded.state !== "loading") setLoaded({ state: "loading" });
  }
  useEffect(() => {
    let live = true;
    dnaGet(rpc, JSON.parse(atKey) as DnaAt, slug)
      .then((note) => {
        if (!live) return;
        setLoaded({ state: "ready", note });
        setError(null);
      })
      .catch((e) => {
        if (!live) return;
        const missing = e instanceof DaemonError && e.code === RpcCode.NOT_FOUND;
        const error = missing ? `No note named ${slug} (a link to a missing note).` : dnaErrorText(e);
        // A failed refetch of the same note keeps it on screen, with why it may be out of date.
        setLoaded((cur) => (cur.state === "ready" && cur.note.slug === slug && !missing ? { ...cur, stale: error } : { state: "error", error }));
      });
    return () => {
      live = false;
    };
  }, [rpc, atKey, slug, version, reload, boot]);

  if (loaded.state === "loading") return <p className="muted">Reading the note…</p>;
  if (loaded.state === "error") return <ErrorRetry text={loaded.error} onRetry={() => setReload((n) => n + 1)} />;
  const n = loaded.note;
  const act = async (action: NoteActionKind) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      const note = await noteAction(rpc, repo, n.slug, action, n.content_hash);
      setLoaded({ state: "ready", note });
    } catch (e) {
      const conflict = e instanceof DaemonError && e.code === RpcCode.CONFLICT;
      setError({ text: dnaErrorText(e), reload: conflict });
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return (
    <article className="dna-note" aria-label={n.title}>
      <header className="dna-note-head">
        <h3>{n.title}</h3>
        <span className="tag">{n.kind}</span>
        <StatePill n={n} />
        <span className={`tag ${n.reviewed ? "" : "tag-warn"}`}>{n.reviewed ? "reviewed" : "not reviewed by a person"}</span>
      </header>
      <ul className="dna-anchors" aria-label="Cites">
        {n.anchors.map((a, i) => (
          <AnchorLine key={i} a={a} />
        ))}
      </ul>
      <NoteBody body={n.body} onOpen={onOpen} />
      {n.backlinks.length > 0 && (
        <p className="small">
          <span className="muted">Linked from </span>
          {n.backlinks.map((b) => (
            <button key={b} type="button" className="dna-link" onClick={() => onOpen(b)}>
              {b}
            </button>
          ))}
        </p>
      )}
      <p className="muted small">
        <code>{n.path}</code>
      </p>
      {n.status !== "retired" && (
        <div className="dna-actions">
          <button className="btn btn-small" disabled={busy} onClick={() => act("confirm")} title="Re-anchor at the checkout's HEAD and mark this content reviewed">
            Confirm
          </button>
          <button className="btn btn-small" disabled={busy} onClick={() => act("retire")} title="Set status: retired; the file stays">
            Retire
          </button>
        </div>
      )}
      {loaded.stale && (
        <p className="error small" role="status">
          Could not refresh this note: {loaded.stale}
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error.text}{" "}
          {error.reload && (
            <button className="btn btn-small" onClick={() => setReload((r) => r + 1)}>
              Reload
            </button>
          )}
        </p>
      )}
    </article>
  );
}
