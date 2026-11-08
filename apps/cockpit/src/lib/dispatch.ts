// "@claude shop/feat/login add a login page" -> agent claude, repo "shop", branch
// "feat/login", goal "add a login page". The repo is everything before the first "/";
// the branch is the rest of that word (it may contain "/"); the goal is the rest of the text.
import type { AgentKind } from "./protocol";

export interface Dispatch {
  agent: AgentKind;
  repo: string;
  branch: string;
  goal: string;
}

export type Parsed = { ok: true; value: Dispatch } | { ok: false; error: string };

export const DISPATCH_HELP = "@claude <repo>/<branch> <goal>  or  @codex <repo>/<branch> <goal>";

const AGENTS: Record<string, AgentKind> = { "@claude": "claude", "@codex": "codex" };

function branchProblem(b: string): string | null {
  if (b.startsWith("-")) return "a branch cannot start with '-'";
  if (b.includes("..")) return "a branch cannot contain '..'";
  if (b.endsWith("/") || b.endsWith(".lock") || b.endsWith(".")) return `"${b}" is not a valid branch name`;
  if (/[~^:?*[\\\x00-\x20\x7f]/.test(b)) return `"${b}" has a character git does not allow in branch names`;
  if (b.includes("//") || b.includes("@{")) return `"${b}" is not a valid branch name`;
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
  if (slash <= 0 || slash === target.length - 1) {
    return { ok: false, error: `"${target}" must be <repo>/<branch>, e.g. shop/feat/login` };
  }
  const repo = target.slice(0, slash);
  const branch = target.slice(slash + 1);
  const bad = branchProblem(branch);
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
