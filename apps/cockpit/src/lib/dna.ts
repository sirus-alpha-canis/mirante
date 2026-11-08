// Repo memory (DNA): the daemon's `dna.*` calls as the cockpit makes them, and the helpers
// views share. Everything here returns data; nothing builds HTML. Note bodies are split
// into text and [[link]] parts that components render as text nodes and buttons.
import { DaemonError } from "./daemon";
import { t } from "./i18n";
import type {
  CheckReport,
  DnaGraph,
  DnaStatus,
  NoteSummary,
  NoteView,
  ProposalView,
  ReviewedView,
  TaskMemoryContext,
} from "./protocol";
import { M, RpcCode } from "./protocol";

export type Rpc = <T>(method: string, params?: object) => Promise<T>;

/** A repo's memory, named by its checkout or by a task (its repo). */
export type DnaAt = { repo: string } | { task: string };

/** `available: false`: the daemon predates repo memory (METHOD_NOT_FOUND). */
export type DnaAvailability = { available: false } | { available: true; status: DnaStatus };

function isOlderDaemon(e: unknown): boolean {
  return e instanceof DaemonError && e.code === RpcCode.METHOD_NOT_FOUND;
}

export async function dnaStatus(rpc: Rpc, repo: string): Promise<DnaAvailability> {
  try {
    return { available: true, status: await rpc<DnaStatus>(M.dnaStatus, { repo }) };
  } catch (e) {
    if (isOlderDaemon(e)) return { available: false };
    throw e;
  }
}

export const dnaEnable = (rpc: Rpc, repo: string) => rpc<DnaStatus>(M.dnaEnable, { repo });
export const dnaGraph = (rpc: Rpc, repo: string) => rpc<DnaGraph>(M.dnaGraph, { repo });
export const dnaGet = (rpc: Rpc, at: DnaAt, slug: string) => rpc<NoteView>(M.dnaGet, { ...at, slug });
export const dnaSearch = (rpc: Rpc, repo: string, query: string, limit?: number) =>
  rpc<NoteSummary[]>(M.dnaSearch, limit === undefined ? { repo, query } : { repo, query, limit });
export const dnaCheck = (rpc: Rpc, repo: string) => rpc<CheckReport>(M.dnaCheck, { repo });
/** The person view (the cockpit never asks for `for_agent`). */
export const taskContext = (rpc: Rpc, task: string) => rpc<TaskMemoryContext>(M.dnaTaskContext, { task });

export interface ProposalQuery {
  repo?: string;
  task?: string;
  id?: string;
  /** Pending by default; "all" for every status. */
  status?: "pending" | "accepted" | "rejected" | "all";
  /** The last id of the previous page. */
  after?: string;
  /** Default 50, at most 200. */
  limit?: number;
}

/** One page of the review queue, oldest first. */
export const proposals = (rpc: Rpc, q: ProposalQuery) => rpc<ProposalView[]>(M.dnaProposals, q);

export interface ReviewParams {
  id: string;
  accept: boolean;
  title?: string;
  body?: string;
  note?: string;
}

export const review = (rpc: Rpc, p: ReviewParams) => rpc<ReviewedView>(M.dnaReview, p);

export type NoteActionKind = "confirm" | "retire";

/** `expect_hash`: the `content_hash` of the note the person was shown. */
export const noteAction = (rpc: Rpc, repo: string, slug: string, action: NoteActionKind, expectHash: string) =>
  rpc<NoteView>(M.dnaNoteAction, { repo, slug, action, expect_hash: expectHash });

/** Text for a person when a memory call is refused. */
export function dnaErrorText(e: unknown): string {
  const err = e instanceof DaemonError ? e : null;
  if (err?.code === RpcCode.FORBIDDEN) {
    return t("dna.forbidden");
  }
  if (err?.code === RpcCode.CONFLICT && /changed on disk/.test(err.message)) {
    return t("dna.changed");
  }
  return e instanceof Error ? e.message : String(e);
}

export type LinkPart = { text: string } | { link: string; label: string };

/** Common Latin letters with diacritics, folded to ASCII (the daemon's `fold`). */
const FOLD: Record<string, string> = {};
for (const [to, from] of [
  ["a", "àáâãäåā"],
  ["c", "çćč"],
  ["e", "èéêëēě"],
  ["i", "ìíîïī"],
  ["n", "ñń"],
  ["o", "òóôõöøō"],
  ["u", "ùúûüūů"],
  ["y", "ýÿ"],
  ["s", "šś"],
  ["z", "žźż"],
  ["l", "ł"],
  ["ss", "ß"],
  ["ae", "æ"],
  ["oe", "œ"],
] as const) {
  for (const c of from) FOLD[c] = to;
}

/** The ASCII kebab slug a `[[target]]` names: a port of the daemon's `note::slugify`
 * (both check crates/mirante-core/src/dna/link-vectors.json). */
export function slugify(s: string): string {
  let out = "";
  let dash = false;
  outer: for (const ch of s) {
    for (const c of ch.toLowerCase()) {
      const piece = /^[a-z0-9]$/.test(c) ? c : FOLD[c];
      if (piece === undefined) dash = true;
      else {
        if (dash && out) out += "-";
        dash = false;
        out += piece;
      }
      if (out.length >= 64) break outer;
    }
  }
  out = out.slice(0, 64).replace(/-+$/, "");
  return out || "note";
}

const ALNUM = /[\p{Alphabetic}\p{N}]/u;

/** A note body as text and [[link]] parts: `[[slug]]`, `[[slug|alias]]` and
 * `[[slug#heading]]` link to `slug`. Everything else stays text, markup included. The
 * daemon's `note::links` rules: a link never spans a line or holds a `[`, and its target
 * has a letter or digit. One pass: each character is looked at a bounded number of times. */
export function linkParts(body: string): LinkPart[] {
  const out: LinkPart[] = [];
  let text = "";
  let i = 0;
  let from = 0;
  for (;;) {
    const open = body.indexOf("[[", from);
    if (open < 0) break;
    // The inner text runs to the first "]]"; a "[" or newline before it means no link here
    // (the search for the next "[[" resumes there: nothing before it can start one).
    let j = open + 2;
    let close = -1;
    while (j < body.length) {
      if (body.startsWith("]]", j)) {
        close = j;
        break;
      }
      if (body[j] === "[" || body[j] === "\n") break;
      j++;
    }
    if (close < 0) {
      if (j >= body.length) break;
      from = j;
      continue;
    }
    const inner = body.slice(open + 2, close);
    const bar = inner.indexOf("|");
    const target = inner.split(/[|#]/, 1)[0];
    from = close + 2;
    if (!ALNUM.test(target)) continue;
    text += body.slice(i, open);
    if (text) out.push({ text });
    text = "";
    const label = (bar >= 0 ? inner.slice(bar + 1) : inner).trim();
    out.push({ link: slugify(target), label: label || target.trim() });
    i = from;
  }
  text += body.slice(i);
  if (text) out.push({ text });
  return out;
}

/** A note's state in a person's words (the reviewed badge is shown separately, but a
 * fresh note nobody reviewed is labeled unreviewed). */
export function stateLabel(n: Pick<NoteSummary, "state" | "reviewed"> & { status?: string }): string {
  if (n.status === "retired") return t("dna.state.retired");
  switch (n.state) {
    case "stale":
      return t("dna.state.stale");
    case "unverifiable":
      return t("dna.state.unverifiable");
    case "unchecked":
      return t("dna.state.unchecked");
    default:
      return t(n.reviewed ? "dna.state.fresh" : "dna.state.unreviewed");
  }
}

export const shortCommit = (sha: string) => sha.slice(0, 7);

/** `path:start-end`, with the symbol when there is one. */
export function citation(a: { path: string; symbol?: string | null; start: number; end: number }): string {
  return `${a.path}${a.symbol ? `#${a.symbol}` : ""}:${a.start}-${a.end}`;
}
