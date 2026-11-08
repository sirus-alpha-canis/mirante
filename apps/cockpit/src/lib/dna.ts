// Repo memory (DNA): the daemon's `dna.*` calls as the cockpit makes them, and the helpers
// views share. Everything here returns data; nothing builds HTML. Note bodies are split
// into text and [[link]] parts that components render as text nodes and buttons.
import { DaemonError } from "./daemon";
import type {
  CheckReport,
  DnaGraph,
  DnaStatus,
  NoteSummary,
  NoteView,
  ProposalView,
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

export const review = (rpc: Rpc, p: ReviewParams) => rpc<ProposalView>(M.dnaReview, p);

export type NoteActionKind = "confirm" | "retire";

/** `expect_hash`: the `content_hash` of the note the person was shown. */
export const noteAction = (rpc: Rpc, repo: string, slug: string, action: NoteActionKind, expectHash: string) =>
  rpc<NoteView>(M.dnaNoteAction, { repo, slug, action, expect_hash: expectHash });

/** Text for a person when a memory call is refused. */
export function dnaErrorText(e: unknown): string {
  const err = e instanceof DaemonError ? e : null;
  if (err?.code === RpcCode.FORBIDDEN) {
    return "Memory review is a person's decision and cannot be made from an agent session. Mirante also refuses a caller it cannot check: use the cockpit or `mirante memory` in a terminal you opened yourself.";
  }
  if (err?.code === RpcCode.CONFLICT && /changed on disk/.test(err.message)) {
    return "The note changed since it was shown. Reload it and try again.";
  }
  return e instanceof Error ? e.message : String(e);
}

export type LinkPart = { text: string } | { link: string; label: string };

/** The ASCII kebab slug a `[[target]]` names (the daemon's `slugify`). */
export function slugify(s: string): string {
  const ascii = s.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();
  const slug = ascii.replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 64).replace(/-+$/, "");
  return slug || "note";
}

/** A note body as text and [[link]] parts: `[[slug]]`, `[[slug|alias]]` and
 * `[[slug#heading]]` link to `slug`. Everything else stays text, markup included. */
export function linkParts(body: string): LinkPart[] {
  const out: LinkPart[] = [];
  let text = "";
  let i = 0;
  while (i < body.length) {
    const open = body.indexOf("[[", i);
    if (open < 0) break;
    const close = body.indexOf("]]", open + 2);
    if (close < 0) break;
    const inner = body.slice(open + 2, close);
    const [target, alias] = inner.split("|", 2);
    const slugPart = target.split("#", 1)[0].trim();
    if (!slugPart || inner.includes("\n")) {
      text += body.slice(i, open + 2);
      i = open + 2;
      continue;
    }
    text += body.slice(i, open);
    if (text) out.push({ text });
    text = "";
    out.push({ link: slugify(slugPart), label: (alias ?? target).trim() || slugPart });
    i = close + 2;
  }
  text += body.slice(i);
  if (text) out.push({ text });
  return out;
}

/** A note's state in a person's words (the reviewed badge is shown separately, but a
 * fresh note nobody reviewed is labeled unreviewed). */
export function stateLabel(n: Pick<NoteSummary, "state" | "reviewed"> & { status?: string }): string {
  if (n.status === "retired") return "retired";
  switch (n.state) {
    case "stale":
      return "stale: cited code changed";
    case "unverifiable":
      return "unverifiable: cited commit not found";
    case "unchecked":
      return "unchecked";
    default:
      return n.reviewed ? "fresh" : "unreviewed";
  }
}

export const shortCommit = (sha: string) => sha.slice(0, 7);

/** `path:start-end`, with the symbol when there is one. */
export function citation(a: { path: string; symbol?: string | null; start: number; end: number }): string {
  return `${a.path}${a.symbol ? `#${a.symbol}` : ""}:${a.start}-${a.end}`;
}
