// The brain: a project's repo memory as a graph of notes and their [[links]], with the
// selected note (or the review queue) beside it. SVG with a small deterministic layout
// (lib/graphLayout), keyboard-reachable nodes, and a list view as the accessible fallback
// (the only view past MAX_GRAPH_NODES). Every label is text; nothing is rendered as HTML.
import { type KeyboardEvent, useEffect, useMemo, useState } from "react";
import { useEnv, useStore } from "../env";
import { type DnaAvailability, dnaCheck, dnaErrorText, dnaGraph, dnaSearch, dnaStatus } from "../lib/dna";
import { layout, MAX_GRAPH_NODES } from "../lib/graphLayout";
import type { DnaGraph, NoteSummary } from "../lib/protocol";
import { repoName } from "../store/select";
import { dnaRev } from "../store/state";
import { EnableMemory } from "./MemoryTab";
import { NoteView, StatePill } from "./NoteView";
import { ReviewQueue } from "./ReviewQueue";

const W = 820;
const H = 540;

type Filter = "stale" | "unreviewed" | "retired";
const FILTERS: Filter[] = ["stale", "unreviewed", "retired"];

const isRetired = (n: NoteSummary) => n.status === "retired";

function matches(n: NoteSummary, f: Filter): boolean {
  if (f === "stale") return n.state === "stale";
  if (f === "unreviewed") return !n.reviewed;
  return isRetired(n);
}

/** "Hook spool, stale, reviewed" (", retired" when retired). */
export function nodeName(n: NoteSummary): string {
  return `${n.title}, ${n.state}, ${n.reviewed ? "reviewed" : "unreviewed"}${isRetired(n) ? ", retired" : ""}`;
}

function nodeClass(n: NoteSummary): string {
  const state = n.state === "stale" ? "node-stale" : n.state === "fresh" ? "node-fresh" : "node-unverifiable";
  return [state, n.reviewed ? "" : "node-unreviewed", isRetired(n) ? "node-retired" : ""].filter(Boolean).join(" ");
}

const ghostId = (slug: string) => `?${slug}`;

function Legend() {
  const items: [string, string][] = [
    ["node-fresh", "fresh"],
    ["node-stale", "stale: cited code changed"],
    ["node-unverifiable", "unverifiable or unchecked"],
    ["node-fresh node-unreviewed", "unreviewed (ring)"],
    ["node-fresh node-retired", "retired (faded)"],
    ["node-ghost", "missing note (dashed link)"],
  ];
  return (
    <ul className="brain-legend" aria-label="Legend">
      {items.map(([cls, label]) => (
        <li key={label}>
          <svg width="14" height="14" aria-hidden="true">
            <circle className={cls} cx="7" cy="7" r="5" />
          </svg>{" "}
          {label}
        </li>
      ))}
    </ul>
  );
}

function Graph({
  graph,
  selected,
  hits,
  onSelect,
}: {
  graph: DnaGraph;
  selected: string | null;
  hits: Set<string>;
  onSelect(slug: string): void;
}) {
  const ghosts = useMemo(() => [...new Set(graph.links.filter((l) => l.dangling).map((l) => l.dst))], [graph]);
  const pos = useMemo(
    () =>
      layout(
        {
          nodes: [...graph.nodes.map((n) => ({ id: n.slug })), ...ghosts.map((g) => ({ id: ghostId(g) }))],
          links: graph.links.map((l) => ({ src: l.src, dst: l.dangling ? ghostId(l.dst) : l.dst })),
        },
        W,
        H,
        40,
      ),
    [graph, ghosts],
  );
  const onKey = (e: KeyboardEvent, slug: string) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onSelect(slug);
    }
  };
  return (
    <svg className="brain-svg" viewBox={`0 0 ${W} ${H}`} role="img" aria-label={`Graph of ${graph.nodes.length} notes and their links`}>
      <g aria-hidden="true">
        {graph.links.map((l, i) => {
          const a = pos[l.src];
          const b = pos[l.dangling ? ghostId(l.dst) : l.dst];
          if (!a || !b) return null;
          return <line key={i} className={l.dangling ? "link-dangling" : "link"} x1={a.x} y1={a.y} x2={b.x} y2={b.y} />;
        })}
      </g>
      {ghosts.map((g) => {
        const p = pos[ghostId(g)];
        return (
          p && (
            <g key={`ghost-${g}`} className="brain-ghost" transform={`translate(${p.x},${p.y})`}>
              <circle className="node-ghost" r="6" />
              <text y="18" textAnchor="middle">
                {g}
              </text>
            </g>
          )
        );
      })}
      {graph.nodes.map((n) => {
        const p = pos[n.slug];
        if (!p) return null;
        const cls = `brain-node ${nodeClass(n)}${hits.has(n.slug) ? " node-hit" : ""}${selected === n.slug ? " node-selected" : ""}`;
        return (
          <g
            key={n.slug}
            role="button"
            tabIndex={0}
            aria-label={nodeName(n)}
            aria-pressed={selected === n.slug}
            className={cls}
            transform={`translate(${p.x},${p.y})`}
            onClick={() => onSelect(n.slug)}
            onKeyDown={(e) => onKey(e, n.slug)}
          >
            <title>{n.title}</title>
            <circle r={hits.has(n.slug) ? 11 : 8} />
            <text y="22" textAnchor="middle">
              {n.title.length > 28 ? `${n.title.slice(0, 27)}…` : n.title}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

type Loaded =
  | { state: "loading" }
  | { state: "unavailable" }
  | { state: "error"; error: string }
  | { state: "off" }
  | { state: "ready"; graph: DnaGraph; pending: number };

type Panel = { kind: "note"; slug: string } | { kind: "review" } | null;

/** The brain of `repo` (the project selected in the left rail). `review`: open on the
 * review queue. */
export function Brain({ repo, review = false }: { repo: string | null; review?: boolean }) {
  const { rpc } = useEnv();
  const s = useStore();
  const version = repo ? dnaRev(s, repo) : 0;
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [nonce, setNonce] = useState(0);
  const [panel, setPanel] = useState<Panel>(review ? { kind: "review" } : null);
  const [filters, setFilters] = useState<Set<Filter>>(new Set());
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<{ slugs: Set<string>; text: string } | null>(null);
  const [checked, setChecked] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [asList, setAsList] = useState(false);

  useEffect(() => {
    if (!repo) return;
    let live = true;
    (async () => {
      const st: DnaAvailability = await dnaStatus(rpc, repo);
      if (!st.available) return live && setLoaded({ state: "unavailable" });
      if (!st.status?.enabled) return live && setLoaded({ state: "off" });
      const graph = await dnaGraph(rpc, repo);
      if (live) setLoaded({ state: "ready", graph: { nodes: graph?.nodes ?? [], links: graph?.links ?? [] }, pending: st.status.pending ?? 0 });
    })().catch((e) => live && setLoaded({ state: "error", error: dnaErrorText(e) }));
    return () => {
      live = false;
    };
  }, [rpc, repo, version, nonce]);

  const shown = useMemo<DnaGraph | null>(() => {
    if (loaded.state !== "ready") return null;
    const { graph } = loaded;
    if (filters.size === 0) return graph;
    const nodes = graph.nodes.filter((n) => [...filters].some((f) => matches(n, f)));
    const keep = new Set(nodes.map((n) => n.slug));
    return { nodes, links: graph.links.filter((l) => keep.has(l.src) && (l.dangling || keep.has(l.dst))) };
  }, [loaded, filters]);

  if (!repo) {
    return (
      <main className="brain" aria-label="Brain">
        <p className="muted empty">Add a repository on the left (Home) to see its memory.</p>
      </main>
    );
  }

  const toggle = (f: Filter) =>
    setFilters((cur) => {
      const next = new Set(cur);
      if (next.has(f)) next.delete(f);
      else next.add(f);
      return next;
    });
  const search = async () => {
    const q = query.trim();
    if (!q) return setHits(null);
    try {
      const found = (await dnaSearch(rpc, repo, q, 25)) ?? [];
      setHits({ slugs: new Set(found.map((n) => n.slug)), text: found.length === 1 ? "1 match" : `${found.length} matches` });
    } catch (e) {
      setHits({ slugs: new Set(), text: dnaErrorText(e) });
    }
  };
  const recheck = async () => {
    setChecking(true);
    try {
      const r = await dnaCheck(rpc, repo);
      setChecked(`Checked ${r.notes} notes: ${r.stale} stale, ${r.unreviewed} unreviewed, ${r.invalid?.length ?? 0} invalid`);
      setNonce((n) => n + 1);
    } catch (e) {
      setChecked(dnaErrorText(e));
    } finally {
      setChecking(false);
    }
  };
  const select = (slug: string) => setPanel({ kind: "note", slug });
  const selected = panel?.kind === "note" ? panel.slug : null;

  let body: React.ReactNode;
  if (loaded.state === "loading") body = <p className="muted">Reading repo memory…</p>;
  else if (loaded.state === "unavailable") body = <p className="muted">This daemon has no repo memory. Update Mirante to use it.</p>;
  else if (loaded.state === "error") body = <p className="error">Repo memory unavailable: {loaded.error}</p>;
  else if (loaded.state === "off") body = <EnableMemory repo={repo} onEnabled={() => setNonce((n) => n + 1)} />;
  else if (shown) {
    const tooLarge = shown.nodes.length > MAX_GRAPH_NODES;
    body = (
      <>
        <div className="brain-bar">
          <form
            role="search"
            onSubmit={(e) => {
              e.preventDefault();
              void search();
            }}
          >
            <input
              type="search"
              aria-label="Search memory"
              placeholder="Search memory"
              className="dna-input brain-search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </form>
          {hits && (
            <span className="muted small" role="status">
              {hits.text}
            </span>
          )}
          <span role="group" aria-label="Filters" className="brain-pills">
            {FILTERS.map((f) => (
              <button key={f} className="btn btn-small" aria-pressed={filters.has(f)} onClick={() => toggle(f)}>
                {f}
              </button>
            ))}
          </span>
          <button className="btn btn-small" aria-pressed={asList || tooLarge} disabled={tooLarge} onClick={() => setAsList((v) => !v)}>
            List
          </button>
          <button className="btn btn-small" disabled={checking} onClick={recheck}>
            Re-check
          </button>
          <button className="btn btn-small" aria-pressed={panel?.kind === "review"} onClick={() => setPanel({ kind: "review" })}>
            Review ({loaded.pending})
          </button>
        </div>
        {checked && (
          <p className="muted small" role="status">
            {checked}
          </p>
        )}
        {tooLarge && <p className="muted small">The graph is too large to draw ({shown.nodes.length} notes); here is the list.</p>}
        {loaded.graph.nodes.length === 0 ? (
          <p className="muted">No notes yet. Agents propose them (review them here); you can also write them in .mirante/memory/.</p>
        ) : asList || tooLarge ? (
          <ul className="dna-list brain-list" aria-label="Notes">
            {shown.nodes.map((n) => (
              <li key={n.slug} className="dna-row">
                <button className="link" aria-pressed={selected === n.slug} onClick={() => select(n.slug)}>
                  {n.title}
                </button>{" "}
                <StatePill n={n} />
              </li>
            ))}
          </ul>
        ) : (
          <>
            <Graph graph={shown} selected={selected} hits={hits?.slugs ?? new Set()} onSelect={select} />
            <Legend />
          </>
        )}
      </>
    );
  }

  return (
    <main className="brain" aria-label="Brain">
      <section className="brain-main">
        <header className="feed-head">
          <h1>Memory of {repoName(repo)}</h1>
          <span className="muted small">{repo}</span>
        </header>
        {body}
      </section>
      <aside className="brain-panel" aria-label="Selected">
        {panel?.kind === "review" ? (
          <ReviewQueue repo={repo} />
        ) : panel?.kind === "note" ? (
          <NoteView at={{ repo }} repo={repo} slug={panel.slug} version={version} onOpen={select} />
        ) : (
          <p className="muted">Select a note to read it, or open the review queue.</p>
        )}
      </aside>
    </main>
  );
}
