// Skills: the daemon's `skill.*` calls as the cockpit makes them, and the helpers the views
// share. Everything here returns data; nothing builds HTML. Skill text comes from agents,
// the miner or disk: components render it as text nodes only.
import { DaemonError } from "./daemon";
import type { Rpc } from "./dna";
import type {
  Finding,
  OriginConfirmed,
  RepoOrigin,
  SkillActionKind,
  SkillEdits,
  SkillMined,
  SkillProposalView,
  SkillSummary,
  SkillView,
  TaskSkills,
  TenantView,
} from "./protocol";
import { t } from "./i18n";
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

/** Queues a skill version's folder, as it is on disk now, for review again. The daemon
 * names the folder (the cockpit never sends a path) and refuses one that now names
 * another skill. */
export const skillRereview = (rpc: Rpc, name: string, version: number) =>
  rpc<SkillProposalView>(M.skillRereview, { name, version });
/** A checkout's pinned origin and its live one. */
export const skillRepoOrigin = (rpc: Rpc, repo: string) => rpc<RepoOrigin>(M.skillRepoOrigin, { repo });

export interface SkillProposalQuery {
  tenant?: string;
  task?: string;
  id?: string;
  status?: "pending" | "accepted" | "rejected" | "all";
  after?: string;
  limit?: number;
}
export const skillProposals = (rpc: Rpc, q: SkillProposalQuery = {}) => rpc<SkillProposalView[]>(M.skillProposals, q);

/** How many proposals wait, every client's; null from a daemon that cannot count them. */
export async function skillPending(rpc: Rpc): Promise<number | null> {
  try {
    const r = await rpc<{ pending: number }>(M.skillPending, {});
    return typeof r?.pending === "number" ? r.pending : null;
  } catch (e) {
    if (isOlderDaemon(e)) return null;
    throw e;
  }
}

/** The tenant (client) of an origin key: its host and first segment. */
export const originTenant = (origin: string) => origin.split("/").slice(0, 2).join("/");

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
      return t("skills.scopeOrg", { v: scopeValue(scope) });
    case "repo":
      return t("skills.scopeRepo", { v: scopeValue(scope) });
    default:
      return t("skills.scopePersonal");
  }
}

/** Where a skill goes in the agent's session. */
export function activationLabel(a: string): string {
  return a === "always" ? t("skills.activationAlways") : t("skills.activationOnDemand");
}

/** An activation in a word or two ("always on", "on demand"), never the raw value. */
export function activationShort(a: string): string {
  return a === "always" ? t("skills.short.always") : t("skills.short.on_demand");
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
      learnedIn ? t("skills.widenPersonalFrom", { from: learnedIn }) : t("skills.widenPersonal"),
    ];
  }
  const to = scopeTenant(scope);
  return learnedIn && to && to !== learnedIn ? [t("skills.widenOther", { from: learnedIn, to })] : [];
}

export const hasLintErrors = (f: readonly Finding[]) => f.some((x) => x.severity === "error");

/** `rule (error, SKILL.md line 3): message`. */
export function findingText(f: Finding): string {
  const where = f.line > 0 ? t("skills.findingWhere", { file: f.file, line: f.line }) : f.file;
  return `${f.rule} (${f.severity}, ${where}): ${f.message}`;
}

/** Text for a person when a skills call is refused. */
export function skillErrorText(e: unknown): string {
  const err = e instanceof DaemonError ? e : null;
  if (err?.code === RpcCode.FORBIDDEN) {
    return t("skillErr.forbidden");
  }
  return e instanceof Error ? e.message : String(e);
}
