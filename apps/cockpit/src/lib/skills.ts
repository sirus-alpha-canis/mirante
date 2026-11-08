// Skills: the daemon's `skill.*` calls as the cockpit makes them, and the helpers the views
// share. Everything here returns data; nothing builds HTML. Skill text comes from agents,
// the miner or disk: components render it as text nodes only.
import { DaemonError } from "./daemon";
import type { Rpc } from "./dna";
import type {
  Finding,
  OriginConfirmed,
  SkillActionKind,
  SkillEdits,
  SkillMined,
  SkillProposalView,
  SkillSummary,
  SkillView,
  TaskSkills,
  TenantView,
} from "./protocol";
import { M, RpcCode } from "./protocol";

function isOlderDaemon(e: unknown): boolean {
  return e instanceof DaemonError && e.code === RpcCode.METHOD_NOT_FOUND;
}

/** `available: false`: the daemon predates skills (METHOD_NOT_FOUND). */
export type SkillsAvailability = { available: false } | { available: true; tenants: TenantView[] };

export async function skillTenants(rpc: Rpc): Promise<SkillsAvailability> {
  try {
    return { available: true, tenants: (await rpc<TenantView[]>(M.skillTenants, {})) ?? [] };
  } catch (e) {
    if (isOlderDaemon(e)) return { available: false };
    throw e;
  }
}

/** What a task's next session gets; null from a daemon without skills. */
export async function taskSkills(rpc: Rpc, task: string): Promise<TaskSkills | null> {
  try {
    return await rpc<TaskSkills>(M.skillTaskSkills, { task });
  } catch (e) {
    if (isOlderDaemon(e)) return null;
    throw e;
  }
}

export const skillLearn = (rpc: Rpc, tenant: string, enabled: boolean) => rpc<TenantView>(M.skillLearn, { tenant, enabled });
export const skillList = (rpc: Rpc, q: { tenant?: string; status?: "active" | "retired" } = {}) =>
  rpc<SkillSummary[]>(M.skillList, q);
export const skillGet = (rpc: Rpc, name: string, version?: number) =>
  rpc<SkillView>(M.skillGet, version === undefined ? { name } : { name, version });
export const confirmOrigin = (rpc: Rpc, task: string, origin: string) =>
  rpc<OriginConfirmed>(M.skillConfirmOrigin, { task, origin });
export const skillLint = (rpc: Rpc, p: { name: string; description: string; body: string; scope: string }) =>
  rpc<Finding[]>(M.skillLint, p);
export const skillMine = (rpc: Rpc, tenant: string) => rpc<SkillMined>(M.skillMine, { tenant });

export type SkillAddParams = {
  scope: string;
  activation?: string;
} & ({ dir: string } | { inline: { name: string; description: string; body: string } });
export const skillAdd = (rpc: Rpc, p: SkillAddParams) => rpc<SkillProposalView>(M.skillAdd, p);

export interface SkillProposalQuery {
  tenant?: string;
  task?: string;
  id?: string;
  status?: "pending" | "accepted" | "rejected" | "all";
  after?: string;
  limit?: number;
}
export const skillProposals = (rpc: Rpc, q: SkillProposalQuery = {}) => rpc<SkillProposalView[]>(M.skillProposals, q);

export interface SkillReviewParams {
  id: string;
  accept: boolean;
  edits?: SkillEdits;
  note?: string;
}
export const skillReview = (rpc: Rpc, p: SkillReviewParams) => rpc<SkillProposalView>(M.skillReview, p);

/** `expectHash`: the `manifest_hash` of the version the person was shown. */
export const skillAction = (
  rpc: Rpc,
  name: string,
  action: SkillActionKind,
  expectHash: string,
  extra: { scope?: string; version?: number } = {},
) => rpc<SkillView>(M.skillAction, { name, action, expect_hash: expectHash, ...extra });

export type ScopeKind = "personal" | "org" | "repo";

export function scopeKind(scope: string): ScopeKind {
  if (scope.startsWith("org:")) return "org";
  if (scope.startsWith("repo:")) return "repo";
  return "personal";
}

const scopeValue = (scope: string) => scope.slice(scope.indexOf(":") + 1);

export function scopeLabel(scope: string): string {
  switch (scopeKind(scope)) {
    case "org":
      return `Org ${scopeValue(scope)}`;
    case "repo":
      return `Repo ${scopeValue(scope)}`;
    default:
      return "Personal (every repo)";
  }
}

/** Where a skill goes in the agent's session. */
export function activationLabel(a: string): string {
  return a === "always"
    ? "Always on: added to the agent's system prompt in every matching session"
    : "On demand: listed for the agent, which loads it when relevant";
}

/** The body of a rendered `SKILL.md` (Mirante writes only name and description above it). */
export function skillBody(skillMd: string): string {
  const m = /^---\r?\n[\s\S]*?\r?\n---\r?\n/.exec(skillMd);
  return m ? skillMd.slice(m[0].length) : skillMd;
}

/** The tenant a rendered scope belongs to (`host/owner`), or null for personal. */
export function scopeTenant(scope: string): string | null {
  if (scopeKind(scope) === "personal") return null;
  const key = scope.slice(scope.indexOf(":") + 1);
  return key.split("/").slice(0, 2).join("/");
}

/** The cross-client warnings for putting text learned in `learnedIn` under `scope`. */
export function wideningWarnings(scope: string, learnedIn: string | null | undefined): string[] {
  const kind = scopeKind(scope);
  if (kind === "personal") {
    return [
      learnedIn
        ? `Learned in ${learnedIn}; personal skills reach every repo, including other clients'.`
        : "Personal skills reach every repo, including other clients'.",
    ];
  }
  const t = scopeTenant(scope);
  return learnedIn && t && t !== learnedIn ? [`Learned in ${learnedIn}; scoped to ${t}, another client's.`] : [];
}

export const hasLintErrors = (f: readonly Finding[]) => f.some((x) => x.severity === "error");

/** `rule (error, SKILL.md line 3): message`. */
export function findingText(f: Finding): string {
  const where = f.line > 0 ? `${f.file} line ${f.line}` : f.file;
  return `${f.rule} (${f.severity}, ${where}): ${f.message}`;
}

/** Text for a person when a skills call is refused. */
export function skillErrorText(e: unknown): string {
  const err = e instanceof DaemonError ? e : null;
  if (err?.code === RpcCode.FORBIDDEN) {
    return "Reviewing skills is a person's decision and cannot be made from an agent session. Mirante also refuses a caller it cannot check: use the cockpit or `mirante skills` in a terminal you opened yourself.";
  }
  return e instanceof Error ? e.message : String(e);
}
