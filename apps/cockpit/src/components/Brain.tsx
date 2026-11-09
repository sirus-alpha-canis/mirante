// The brain: a project's repo memory as a graph of notes and their [[links]], with the
// selected note (or the review queue) beside it. SVG with a small deterministic layout
// (lib/graphLayout), keyboard-reachable nodes, and a list view as the accessible fallback
// (the only view past MAX_GRAPH_NODES). Every label is text; nothing is rendered as HTML.
import { type KeyboardEvent, memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useBoot, useEnv } from "../env";
import { type DnaAvailability, dnaCheck, dnaErrorText, dnaGraph, dnaSearch, dnaStatus } from "../lib/dna";
import { layout, MAX_GRAPH_NODES } from "../lib/graphLayout";
import type { DnaGraph, NoteSummary } from "../lib/protocol";
import { learnDnaKey, useDnaRev } from "../lib/useDna";
import { repoName } from "../store/select";
import { EnableMemory } from "./MemoryTab";
import { NoteView, StatePill } from "./NoteView";
import { ReviewQueue } from "./ReviewQueue";
import { ErrorRetry } from "./ErrorRetry";
import { type Key, t as tr, tn, useT } from "../lib/i18n";
import { Unsupported } from "./Restart";

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

const FILTER_KEY: Record<Filter, Key> = { stale: "brain.filter.stale", unreviewed: "brain.filter.unreviewed", retired: "brain.filter.retired" };

/** A note's freshness (`fresh`, `stale`, `unverifiable`…) in the current language. */
export function stateText(state: string): string {
  return state === "fresh" ? tr("note.state.fresh") : state === "stale" ? tr("note.state.stale") : state === "unverifiable" ? tr("note.state.unverifiable") : state;
}

/** "Hook spool, stale, reviewed" (", retired" when retired). */
export function nodeName(n: NoteSummary): string {
  return tr(isRetired(n) ? "brain.node.retired" : "brain.node", {
    title: n.title,
    state: stateText(n.state),
    reviewed: tr(n.reviewed ? "brain.reviewed" : "brain.unreviewed"),
  });
}

function nodeClass(n: NoteSummary): string {
  const state = n.state === "stale" ? "node-stale" : n.state === "fresh" ? "node-fresh" : "node-unverifiable";
  return [state, n.reviewed ? "" : "node-unreviewed", isRetired(n) ? "node-retired" : ""].filter(Boolean).join(" ");
}

const ghostId = (slug: string) => `?${slug}`;

/** The distinct missing notes `graph` links to (drawn as ghost nodes). */
const ghostsOf = (graph: DnaGraph) => [...new Set(graph.links.filter((l) => l.dangling).map((l) => l.dst))];

/** No search hits (one shared value, so the memoized graph is not redrawn for nothing). */
const NO_HITS: ReadonlySet<string> = new Set();

function Legend() {
  const t = useT();
  const items: [string, string][] = [
    ["node-fresh", t("brain.legend.fresh")],
    ["node-stale", t("brain.legend.stale")],
    ["node-unverifiable", t("brain.legend.unverifiable")],
    ["node-fresh node-unreviewed", t("brain.legend.unreviewed")],
    ["node-fresh node-retired", t("brain.legend.retired")],
    ["node-ghost", t("brain.legend.missing")],
  ];
  return (
    <ul className="brain-legend" aria-label={t("brain.legend")}>
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

const Graph = memo(function Graph({
  graph,
  selected,
  hits,
  onSelect,
}: {
  graph: DnaGraph;
  selected: string | null;
  hits: ReadonlySet<string>;
  onSelect(slug: string): void;
}) {
  const t = useT();
  const ghosts = useMemo(() => ghostsOf(graph), [graph]);
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
    // A group, not an image: an image's children are presentational, which would hide the
    // node buttons from assistive technology.
    <svg className="brain-svg" viewBox={`0 0 ${W} ${H}`} role="group" aria-label={t("brain.graph", { n: graph.nodes.length })}>
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
});

type Loaded =
  | { state: "loading" }
  | { state: "unavailable" }
  | { state: "error"; error: string }
  | { state: "off" }
  | { state: "ready"; graph: DnaGraph; pending: number };

type Panel = { kind: "note"; slug: string } | { kind: "review" } | null;

/** The brain of `repo` (the selected project, or the open task's). Each new `reviewNonce`
 * (above 0) switches the panel to the review queue, without starting the brain over. */
export function Brain({ repo, reviewNonce = 0 }: { repo: string | null; reviewNonce?: number }) {
  const { rpc, cockpit } = useEnv();
  const t = useT();
  const boot = useBoot();
  const version = useDnaRev(repo);
  const [loaded, setLoaded] = useState<Loaded>({ state: "loading" });
  const [nonce, setNonce] = useState(0);
  const [panel, setPanel] = useState<Panel>(reviewNonce > 0 ? { kind: "review" } : null);
  // Stable, so the memoized graph redraws only when what it shows changes.
  const select = useCallback((slug: string) => setPanel({ kind: "note", slug }), []);
  const seenReview = useRef(reviewNonce);
  useEffect(() => {
    if (reviewNonce === seenReview.current) return;
    seenReview.current = reviewNonce;
    if (reviewNonce > 0) setPanel({ kind: "review" });
  }, [reviewNonce]);
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
      learnDnaKey(cockpit, repo, st.status);
      if (!st.status?.enabled) return live && setLoaded({ state: "off" });
      const graph = await dnaGraph(rpc, repo);
      if (live) setLoaded({ state: "ready", graph: { nodes: graph?.nodes ?? [], links: graph?.links ?? [] }, pending: st.status.pending ?? 0 });
    })().catch((e) => live && setLoaded({ state: "error", error: dnaErrorText(e) }));
    return () => {
      live = false;
    };
  }, [rpc, cockpit, repo, version, nonce, boot]);

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
      <main className="brain" aria-label={t("brain.aria")}>
        <p className="muted empty">{t("brain.noRepo")}</p>
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
      setHits({ slugs: new Set(found.map((n) => n.slug)), text: tn(found.length, "brain.match.one", "brain.match.many") });
    } catch (e) {
      setHits({ slugs: new Set(), text: dnaErrorText(e) });
    }
  };
  const recheck = async () => {
    setChecking(true);
    try {
      const r = await dnaCheck(rpc, repo);
      setChecked(t("brain.checked", { notes: r.notes, stale: r.stale, unreviewed: r.unreviewed, invalid: r.invalid?.length ?? 0 }));
      setNonce((n) => n + 1);
    } catch (e) {
      setChecked(dnaErrorText(e));
    } finally {
      setChecking(false);
    }
  };
  // A decision in the panel counts at once (the refetch its event causes confirms it).
  const decided = () => setLoaded((l) => (l.state === "ready" ? { ...l, pending: Math.max(0, l.pending - 1) } : l));
  const selected = panel?.kind === "note" ? panel.slug : null;

  let body: React.ReactNode;
  if (loaded.state === "loading") body = <p className="muted">{t("brain.loading")}</p>;
  else if (loaded.state === "unavailable") body = <Unsupported text={t("brain.unavailable")} />;
  else if (loaded.state === "error") body = <ErrorRetry text={t("brain.error", { error: loaded.error })} onRetry={() => setNonce((n) => n + 1)} />;
  else if (loaded.state === "off") body = <EnableMemory repo={repo} onEnabled={() => setNonce((n) => n + 1)} />;
  else if (shown) {
    // Missing notes are drawn too: they count toward what the layout has to place.
    const drawn = shown.nodes.length + ghostsOf(shown).length;
    const tooLarge = drawn > MAX_GRAPH_NODES;
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
              aria-label={t("brain.search")}
              placeholder={t("brain.search")}
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
          <span role="group" aria-label={t("brain.filters")} className="brain-pills">
            {FILTERS.map((f) => (
              <button key={f} className="btn btn-small" aria-pressed={filters.has(f)} onClick={() => toggle(f)}>
                {t(FILTER_KEY[f])}
              </button>
            ))}
          </span>
          <button className="btn btn-small" aria-pressed={asList || tooLarge} disabled={tooLarge} onClick={() => setAsList((v) => !v)}>
            {t("brain.list")}
          </button>
          <button className="btn btn-small" disabled={checking} onClick={recheck}>
            {t("brain.recheck")}
          </button>
          <button className="btn btn-small" aria-pressed={panel?.kind === "review"} onClick={() => setPanel({ kind: "review" })}>
            {t("brain.review", { n: loaded.pending })}
          </button>
        </div>
        {checked && (
          <p className="muted small" role="status">
            {checked}
          </p>
        )}
        {tooLarge && (
          <p className="muted small">
            {t("brain.tooLarge", { notes: shown.nodes.length, missing: drawn - shown.nodes.length })}
          </p>
        )}
        {loaded.graph.nodes.length === 0 ? (
          <p className="muted">{t("brain.empty")}</p>
        ) : asList || tooLarge ? (
          <ul className="dna-list brain-list" aria-label={t("brain.notes")}>
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
            <Graph graph={shown} selected={selected} hits={hits?.slugs ?? NO_HITS} onSelect={select} />
            <Legend />
          </>
        )}
      </>
    );
  }

  return (
    <main className="brain" aria-label={t("brain.aria")}>
      <section className="brain-main">
        <header className="feed-head">
          <h1>{t("brain.title", { name: repoName(repo) })}</h1>
          <span className="muted small">{repo}</span>
        </header>
        {body}
      </section>
      <aside className="brain-panel" aria-label={t("brain.selected")}>
        {panel?.kind === "review" ? (
          <ReviewQueue repo={repo} onDecided={decided} />
        ) : panel?.kind === "note" ? (
          <NoteView at={{ repo }} repo={repo} slug={panel.slug} version={version} onOpen={select} />
        ) : (
          <p className="muted">{t("brain.pick")}</p>
        )}
      </aside>
    </main>
  );
}
