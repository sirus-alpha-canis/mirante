// "@claude shop/feat/login add a login page" -> agent claude, repo "shop", branch
// "feat/login", goal "add a login page". The repo is everything before the first "/";
// the branch is the rest of that word (it may contain "/"; empty, as in "shop/", leaves it to
// be named from the goal); the goal is the rest of the text.
import type { AgentKind } from "./protocol";

export interface Dispatch {
  agent: AgentKind;
  repo: string;
  branch: string;
  goal: string;
}

export type Parsed = { ok: true; value: Dispatch } | { ok: false; error: string };

export const DISPATCH_HELP = "@claude <repo>/<branch> <goal>  or  @codex <repo>/<branch> <goal>";

/** The goal box's hint: plain words, with the one-line syntax as a shortcut. */
export const GOAL_HELP = "What should the agent do? (⌘↵ to dispatch; power users: @claude repo/branch goal)";

const AGENTS: Record<string, AgentKind> = { "@claude": "claude", "@codex": "codex" };

// git check-ref-format, as far as a typed branch name needs it.
export function branchProblem(b: string): string | null {
  if (b.startsWith("-")) return "a branch cannot start with '-'";
  if (b.startsWith("/")) return "a branch cannot start with '/'";
  if (b.includes("..")) return "a branch cannot contain '..'";
  if (b === "@") return `"${b}" is not a valid branch name`;
  if (/[~^:?*[\\\x00-\x20\x7f]/.test(b)) return `"${b}" has a character git does not allow in branch names`;
  if (b.includes("@{")) return `"${b}" is not a valid branch name`;
  if (b.endsWith("/") || b.endsWith(".")) return `"${b}" is not a valid branch name`;
  for (const part of b.split("/")) {
    if (part === "") return `"${b}" has an empty component (doubled '/')`;
    if (part.startsWith(".")) return `"${b}": a component cannot start with '.'`;
    if (part.endsWith(".lock")) return `"${b}": a component cannot end with '.lock'`;
  }
  return null;
}

export function parseDispatch(input: string): Parsed {
  const text = input.trim();
  const m = /^(\S+)(?:\s+(\S+))?(?:\s+([\s\S]*))?$/.exec(text);
  if (!text.startsWith("@") || !m) return { ok: false, error: `start with an agent: ${DISPATCH_HELP}` };
  const agent = AGENTS[m[1].toLowerCase()];
  if (!agent) return { ok: false, error: `unknown agent "${m[1]}": use @claude or @codex` };
  const target = m[2];
  if (!target) return { ok: false, error: `say where: ${DISPATCH_HELP}` };
  const slash = target.indexOf("/");
  if (slash <= 0) {
    return { ok: false, error: `"${target}" must be <repo>/<branch>, e.g. shop/feat/login` };
  }
  const repo = target.slice(0, slash);
  const branch = target.slice(slash + 1);
  const bad = branch === "" ? null : branchProblem(branch);
  if (bad) return { ok: false, error: bad };
  const goal = (m[3] ?? "").trim();
  if (!goal) return { ok: false, error: "say what the agent should do after the branch" };
  return { ok: true, value: { agent, repo, branch, goal } };
}

export type Resolved = { ok: true; path: string } | { ok: false; error: string };

/** The repo path for a name typed in a dispatch, from the known projects. */
export function resolveRepo(name: string, known: { name: string; path: string }[]): Resolved {
  const hits = known.filter((p) => p.name === name);
  if (hits.length === 1) return { ok: true, path: hits[0].path };
  if (hits.length > 1) {
    return { ok: false, error: `"${name}" names several repos (${hits.map((h) => h.path).join(", ")}); rename one folder` };
  }
  return { ok: false, error: `unknown repo "${name}": add it under Projects first` };
}

/** Where auto-named branches live. */
export const BRANCH_PREFIX = "mirante/";
const SLUG_MAX = 40;

/** "Leia este repositório e me explique" -> "mirante/leia-este-repositorio-e-me-explique":
 * lowercase ASCII, accents stripped, words joined by "-", cut near 40 characters at a word
 * boundary. Nothing ASCII left (e.g. Japanese) -> "mirante/task". */
export function branchFromGoal(goal: string): string {
  let slug = goal
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  if (slug.length > SLUG_MAX) {
    const cut = slug.slice(0, SLUG_MAX + 1).lastIndexOf("-");
    slug = cut > 0 ? slug.slice(0, cut) : slug.slice(0, SLUG_MAX);
  }
  return BRANCH_PREFIX + (slug || "task");
}

/** `base`, or `base-2`, `base-3`… the first not in `taken`. */
export function nextFreeBranch(base: string, taken: ReadonlySet<string>): string {
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
}

/** Text that starts with "@claude" or "@codex" is the one-line dispatch syntax. */
export function isPowerSyntax(text: string): boolean {
  return /^\s*@(claude|codex)(\s|$)/i.test(text);
}

/** Projects whose folder name matches `query` (ignoring case): prefix matches first, then
 * the ones that contain it; an empty query lists them all, in order. */
export function matchProjects<P extends { name: string }>(query: string, projects: P[]): P[] {
  const q = query.toLowerCase();
  const name = (p: P) => p.name.toLowerCase();
  return [...projects.filter((p) => name(p).startsWith(q)), ...projects.filter((p) => !name(p).startsWith(q) && name(p).includes(q))];
}

export interface SuggestionItem {
  label: string;
  /** Shown beside the label (a project's path). */
  detail?: string;
  insert: string;
}

/** Completions for the word at the caret of a one-line dispatch: replaces text[from, to). */
export interface Suggestions {
  kind: "agent" | "repo";
  from: number;
  to: number;
  items: SuggestionItem[];
}

/** Agents while the first word ("@cl") is typed; projects while the second ("@claude sh")
 * is, before its "/". Nothing anywhere else: never inside the goal. */
export function suggest(text: string, caret: number, projects: { name: string; path: string }[]): Suggestions | null {
  const before = text.slice(0, caret);
  const word = (re: RegExp) => caret + (re.exec(text.slice(caret))?.[0].length ?? 0);
  const agent = /^(\s*)@(\w*)$/.exec(before);
  if (agent) {
    const q = agent[2].toLowerCase();
    const items = (["claude", "codex"] as const)
      .filter((a) => a.startsWith(q))
      .map((a) => ({ label: `@${a}`, insert: `@${a} ` }));
    return items.length ? { kind: "agent", from: agent[1].length, to: word(/^\S*\s?/), items } : null;
  }
  const repo = /^(\s*@(?:claude|codex)\s+)([^\s/]*)$/i.exec(before);
  if (repo) {
    const to = word(/^[^\s/]*/);
    // A branch typed after it already has its "/".
    const slash = text[to] === "/" ? "" : "/";
    const items = matchProjects(repo[2], projects).map((p) => ({ label: p.name, detail: p.path, insert: p.name + slash }));
    return items.length ? { kind: "repo", from: repo[1].length, to, items } : null;
  }
  return null;
}

/** The text with `item` in place, and where the caret goes (after it, past a kept "/"). */
export function applySuggestion(text: string, s: Suggestions, item: SuggestionItem): { text: string; caret: number } {
  const out = text.slice(0, s.from) + item.insert + text.slice(s.to);
  const caret = s.from + item.insert.length + (out[s.from + item.insert.length] === "/" && !item.insert.endsWith("/") ? 1 : 0);
  return { text: out, caret };
}
