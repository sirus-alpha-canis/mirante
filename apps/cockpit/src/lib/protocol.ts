// Wire types of the daemon protocol (crates/mirante-ipc/src/types.rs and
// crates/mirante-core/src/model.rs), as the bridge forwards them. Keep in sync by hand.

export type AgentKind = "claude" | "codex";
export type Status = "idle" | "working" | "needs_you" | "ended";
export type Lifecycle = "created" | "active" | "archiving" | "archived";
export type BootState = "booting" | "trust_prompt" | "ready" | "exited";
export type SendMode = "steer" | "queue";
/** `mirante_core::model::PermissionStatus`; `allowed_session` is a Codex `grantRoot` allowed
 * for the rest of the session. */
export type PermissionStatus = "pending" | "allowed_once" | "allowed_session" | "denied";

export interface TaskView {
  id: string;
  title: string;
  repo_path: string;
  worktree_path: string;
  branch: string;
  agent: AgentKind;
  goal: string;
  notes: string;
  lifecycle: Lifecycle;
  status: Status;
  created_at: number;
  updated_at: number;
  live: boolean;
  boot: BootState | null;
  needs_you: number;
  queued: number;
  detail: string | null;
  /** Codex quota / token usage while live, e.g. "5h 42% · week 9% · 31k tokens". */
  usage?: string | null;
  /** Memory of the live session's process tree, once sampled (absent from older daemons). */
  memory?: MemView | null;
  /** Hibernated to free memory; `session.wake` resumes it. */
  hibernated?: boolean;
  /** Claude, while live: its permission mode (`default`, `plan`, `acceptEdits`, `auto`…). */
  permission_mode?: string | null;
  /** `task.create`'s answer only: the origin fixed for the task's skills. */
  origin?: CreatedOrigin | null;
}

/** `mirante_ipc::types::CreatedOrigin`. */
export interface CreatedOrigin {
  /** The normalized origin key (`host/path`, or `local/<hash>`). */
  origin: string;
  tenant: string;
  /** The live origin when it is not confirmed: no skills until a person confirms it. */
  observed?: string | null;
  /** This task pinned the checkout's origin (its first, a person's). */
  pinned?: boolean;
}

export interface MemView {
  bytes: number;
  peak: number;
  budget: number;
  /** "ok", "soft" or "hard". */
  state: string;
}

/** `mirante_ipc::types::SystemView`: the machine's memory as the daemon's guard sees it. */
export interface SystemView {
  /** "normal", "elevated" or "critical". */
  level: string;
  total: number;
  free: number;
  /** What the OS says is available (macOS `kern.memorystatus_level` × RAM, Linux
   * MemAvailable); absent from older daemons. */
  available?: number | null;
  swap_used: number;
  swap_total: number;
  paused: boolean;
  /** The OS's own pressure measure when it set `level`: "normal" | "warn" | "critical"
   * (absent when judged from swap and free memory, and from older daemons). */
  kernel_level?: string | null;
  /** Where `kernel_level` came from: "macos" or "linux-psi". */
  kernel_source?: string | null;
}

export interface DaemonEvent {
  seq: number;
  boot_id: string;
  task_id: string | null;
  kind: string;
  payload: Record<string, unknown>;
  ts_ms: number;
}

export interface Snapshot {
  boot_id: string;
  seq: number;
  tasks: TaskView[];
  resumed: boolean;
}

export interface PermissionView {
  id: string;
  task_id: string;
  session_id: string;
  tool: string;
  detail: string;
  status: PermissionStatus;
  created_at: number;
  answered_at: number | null;
  task_title: string;
  branch: string;
}

export interface DiffFile {
  path: string;
  added: number | null;
  removed: number | null;
}

export interface TaskDiff {
  base: string;
  merge_base: string;
  files: DiffFile[];
  untracked: string[];
  patch: string;
  truncated: boolean;
  compare_url: string | null;
  pushed: boolean;
}

export interface DaemonStatus {
  pid: number;
  boot_id: string;
  version: string;
  live_sessions: number;
  /** The memory guard's view (absent from older daemons and before the first sample). */
  memory?: SystemView | null;
  /** Mirante types Claude permission answers (`MIRANTE_CLAUDE_INLINE_ANSWERS=1`). Absent or
   * false: Claude cards are answered in the Terminal tab. */
  claude_inline_answers?: boolean;
  /** Sessions the daemon ended come back when it starts again (absent from older daemons). */
  auto_resume?: boolean | null;
  /** `MIRANTE_AUTO_RESUME=off` keeps auto-resume off for this daemon (absent when not). */
  auto_resume_forced_off?: boolean | null;
  /** The daemon's build (`<commit>[-dirty].<unix seconds>`); absent from older daemons. */
  build?: string | null;
}

/** What the daemon binary on disk says it is (`mirante-daemon --version`). */
export interface DaemonBinary {
  version: string;
  /** Absent from binaries older than build ids. */
  build?: string | null;
}

/** `daemon.settings`. */
export interface DaemonSettings {
  auto_resume: boolean;
  /** `MIRANTE_AUTO_RESUME=off` keeps it off for this daemon (the choice is saved for later). */
  forced_by_env?: boolean;
}

export interface StartResult {
  task: string;
  boot: BootState;
  resumed: boolean;
  screen: string;
}

/** `mirante_ipc::types::QueuedMessage`: a message held until the turn ends (next first). */
export interface QueuedMessage {
  id: string;
  text: string;
}

export interface SendResult {
  delivered: SendMode;
  queued: number;
}

// Repo memory (DNA), `mirante_ipc::types::Dna*`. Text fields come in the person view:
// control and invisible characters stripped, `<` kept as written. Render them as text only.

/** `DnaStatus`. */
export interface DnaStatus {
  repo: string;
  enabled: boolean;
  /** `.mirante/memory/` exists. */
  dir: boolean;
  notes: number;
  stale: number;
  unreviewed: number;
  pending: number;
  invalid: number;
  head: string | null;
}

export type AnchorState = "unchecked" | "fresh" | "relocated" | "stale" | "unverifiable";

export interface AnchorView {
  path: string;
  symbol: string | null;
  start: number;
  end: number;
  commit: string;
  state: AnchorState | string;
  reason: string | null;
  now_start: number | null;
  now_end: number | null;
}

export interface NoteSummary {
  slug: string;
  title: string;
  kind: string;
  /** "active" or "retired". */
  status: string;
  /** The worst anchor's state ("relocated" counts as fresh). */
  state: "fresh" | "stale" | "unverifiable" | "unchecked" | string;
  /** A person accepted or confirmed this exact content. */
  reviewed: boolean;
  anchors: AnchorView[];
}

export interface NoteView extends NoteSummary {
  body: string;
  links: string[];
  backlinks: string[];
  /** Sent back as `expect_hash` by `dna.note_action`. */
  content_hash: string;
  source: string;
  path: string;
}

export interface GraphLink {
  src: string;
  dst: string;
  /** No note named `dst`. */
  dangling: boolean;
}

export interface DnaGraph {
  nodes: NoteSummary[];
  links: GraphLink[];
}

/** `CheckReport` (`mirante_core::dna::index::RefreshReport`). */
export interface CheckReport {
  repo: string;
  head: string | null;
  notes: number;
  invalid: { file: string; error: string }[];
  stale: number;
  relocated: number;
  unverifiable: number;
  unreviewed: number;
}

/** `ReviewedView`: what `dna.review` answers, the decided proposal plus how many secrets
 * were redacted from the text that became the note (the person's edits included). An older
 * daemon sends no `redacted`. */
export type ReviewedView = ProposalView & { redacted?: number };

/** A proposal's citation (`mirante_core::dna::anchor::Anchor`). */
export interface ProposalAnchor {
  path: string;
  symbol?: string | null;
  start: number;
  end: number;
  commit: string;
  fingerprint: string;
}

/** `ProposalView` (`mirante_core::dna::index::Proposal`). */
export interface ProposalView {
  id: string;
  repo: string;
  task_id: string | null;
  /** "agent" or "cli". */
  origin: string;
  title: string;
  kind: string;
  body: string;
  anchors: ProposalAnchor[];
  /** Secrets masked before storage. */
  redactions: number;
  status: "pending" | "accepted" | "rejected" | string;
  slug: string | null;
  decision_note: string;
  created_at: number;
  decided_at: number | null;
}

export interface AtRisk extends NoteSummary {
  /** What changed, per anchor (`path: reason`), joined with `; `. */
  reason: string;
}

/** `DnaTaskContext`. */
export interface TaskMemoryContext {
  enabled: boolean;
  relevant: NoteSummary[];
  at_risk: AtRisk[];
  proposals: ProposalView[];
}

// Skills, `mirante_ipc::types::Skill*` and `mirante_core::skills::*`. Skill and proposal
// text (bodies, descriptions, evidence, bundled files) is data from agents or from disk:
// render it as text nodes only, never as HTML or Markdown.

/** `TenantView`. */
export interface TenantView {
  tenant: string;
  learning: boolean;
  tasks: number;
  /** Skills scoped to this tenant (org or repo). */
  skills: number;
}

/** `personal`, `org:<prefix>` or `repo:<key>`. */
export type ScopeText = string;
export type Activation = "always" | "on_demand";

/** `SkillSummary`. `verified`: the current version on disk is the one a person accepted. */
export interface SkillSummary {
  name: string;
  version: number;
  scope: ScopeText;
  activation: Activation | string;
  /** "active" or "retired". */
  status: string;
  description: string;
  /** Sent back as `expect_hash` by `skill.action`. */
  manifest_hash: string;
  verified: boolean;
  reason?: string | null;
  injected_30d: number;
  used_30d: number;
  updated_at: number;
}

/** `mirante_core::skills::format::Manifest`. */
export interface SkillManifest {
  format: string;
  name: string;
  version: number;
  description: string;
  activation: Activation | string;
  agents: string[];
  scope: { kind: string; value: string };
  files: { path: string; sha256: string; bytes: number }[];
  parent: string | null;
  provenance: { source: string; task: string | null; proposal: string | null; learned_in?: string | null };
  signer: { principal: string; key: string };
  created: string;
}

export interface SkillFileView {
  path: string;
  text: string;
}

/** `SkillView`: one version; text only when that version verifies. */
export interface SkillView extends SkillSummary {
  shown_version: number;
  manifest: SkillManifest | null;
  skill_md: string;
  files: SkillFileView[];
  versions: number[];
  warnings: string[];
  /** The version folder on disk (shown only; a re-review is `skill.rereview`, whose folder
   * the daemon names). Absent from older daemons. */
  dir?: string | null;
}

/** `mirante_core::skills::lint::Finding`. */
export interface Finding {
  rule: string;
  severity: "error" | "warn";
  file: string;
  /** 1-based; 0 for the whole field. */
  line: number;
  message: string;
}

export interface EvidenceItem {
  task: string;
  text: string;
}

export interface ProposalFile {
  path: string;
  text: string;
}

/** `SkillProposalView` (`mirante_core::skills::index::SkillProposal` + warnings). */
export interface SkillProposalView {
  id: string;
  /** "miner", "agent", "cli" or "import". */
  origin: string;
  task_id: string | null;
  tenant: string;
  /** "new" or "edit". */
  kind: string;
  name: string;
  /** For an edit: the manifest hash of the version it edits. */
  base_hash: string | null;
  scope: ScopeText;
  activation: Activation | string;
  description: string;
  body: string;
  files: ProposalFile[];
  evidence: EvidenceItem[];
  fingerprint: string | null;
  lint: Finding[];
  redactions: number;
  status: "pending" | "accepted" | "rejected" | string;
  version: number | null;
  decision_note: string;
  created_at: number;
  decided_at: number | null;
  warnings: string[];
  /** For a pending edit in the queue: the version it changes, as a person may trust it. */
  base?: SkillBase | null;
}

/** `SkillBase`: the version an edit changes. Text only when it verifies (a folder changed
 * outside Mirante is never a diff base). */
export interface SkillBase {
  version: number | null;
  verified: boolean;
  reason?: string | null;
  /** The skill's current version is another one: an accept will be refused. */
  moved: boolean;
  description: string;
  activation: Activation | string;
  body: string;
  files: SkillFileView[];
}

/** `RepoOrigin`: a checkout's pinned origin and its live one (normalized). */
export interface RepoOrigin {
  live: string;
  live_tenant: string;
  pinned?: string | null;
  pinned_tenant?: string | null;
}

export interface SkillEdits {
  name?: string;
  description?: string;
  body?: string;
  scope?: ScopeText;
  activation?: Activation;
}

export type SkillActionKind = "retire" | "restore" | "rescope" | "rollback";

/** `SkillMined`. */
export interface SkillMined {
  tenant: string;
  proposed: number;
  updated: number;
  skipped: number;
}

export interface TaskSkill {
  name: string;
  version: number;
  activation: Activation | string;
  scope: ScopeText;
}

/** `TaskSkills`: what the task's next session would get, resolved now. */
export interface TaskSkills {
  tenant: string;
  origin: string;
  learning: boolean;
  /** False while the task's origin waits for a person's confirmation (no skills). */
  confirmed: boolean;
  /** The checkout's live origin while it differs from the pinned one (unconfirmed). */
  observed?: string | null;
  active: TaskSkill[];
  refused: { name: string; reason: string }[];
  unavailable?: string | null;
}

/** `OriginConfirmed`. */
export interface OriginConfirmed {
  origin: string;
  tenant: string;
  cleared: number;
  warning?: string | null;
}

export type StreamMsg =
  | { type: "connected"; snapshot: Snapshot }
  | { type: "event"; event: DaemonEvent }
  | { type: "disconnected"; error: string; retry_ms: number };

export type BridgeError =
  | { kind: "disconnected"; message: string }
  | { kind: "rpc"; code: number; message: string }
  | { kind: "protocol"; message: string }
  | { kind: "forbidden"; method: string }
  /** A write to the daemon did not complete in time (stalled daemon). */
  | { kind: "timeout"; message: string };

/** `mirante_ipc::rpc::code`. */
export const RpcCode = {
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  NOT_FOUND: 1,
  CONFLICT: 2,
  UNSUPPORTED: 3,
  DRIVER: 4,
  NEEDS_YOU: 5,
  FORBIDDEN: 6,
  /** The daemon is at its connection limit; retry. */
  BUSY: 7,
  /** Mirante typed nothing: answer the Claude dialog in the agent's terminal. */
  ANSWER_IN_TERMINAL: 8,
} as const;

/** `mirante_ipc::types::method` (the ones the cockpit may call). */
export const M = {
  taskCreate: "task.create",
  taskList: "task.list",
  taskGet: "task.get",
  taskArchive: "task.archive",
  taskEvents: "task.events",
  taskDiff: "task.diff",
  sessionStart: "session.start",
  sessionResume: "session.resume",
  sessionTrust: "session.trust",
  sessionStop: "session.stop",
  sessionInterrupt: "session.interrupt",
  sessionResize: "session.resize",
  messageSend: "message.send",
  messageListQueued: "message.list_queued",
  messageEdit: "message.edit",
  messageRemove: "message.remove",
  messageReorder: "message.reorder",
  permissionList: "permission.list",
  permissionAnswer: "permission.answer",
  daemonStatus: "daemon.status",
  daemonSettings: "daemon.settings",
  repoFiles: "repo.files",
  sessionHibernate: "session.hibernate",
  sessionWake: "session.wake",
  dnaStatus: "dna.status",
  dnaEnable: "dna.enable",
  dnaDisable: "dna.disable",
  dnaGraph: "dna.graph",
  dnaSearch: "dna.search",
  dnaGet: "dna.get",
  dnaCheck: "dna.check",
  dnaTaskContext: "dna.task_context",
  dnaProposals: "dna.proposals",
  dnaReview: "dna.review",
  dnaNoteAction: "dna.note_action",
  skillTenants: "skill.tenants",
  skillLearn: "skill.learn",
  skillList: "skill.list",
  skillGet: "skill.get",
  skillTaskSkills: "skill.task_skills",
  skillConfirmOrigin: "skill.confirm_origin",
  skillLint: "skill.lint",
  skillMine: "skill.mine",
  skillRereview: "skill.rereview",
  skillRepoOrigin: "skill.repo_origin",
  skillProposals: "skill.proposals",
  skillPending: "skill.pending",
  skillReview: "skill.review",
  skillAction: "skill.action",
  rulesLayers: "rules.layers",
} as const;

/** `rules.layers`: a file pulled in by an `@path` import (Claude), with its own imports. */
export interface RuleImport {
  spec: string;
  path: string;
  exists: boolean;
  size?: number;
  content?: string;
  truncated: boolean;
  error?: string;
  imports?: RuleImport[];
}

export type RuleScope = "managed" | "user" | "project" | "local" | "nested" | "mirante";
export type RuleKind = "instructions" | "settings" | "skills" | "memory" | "mode";

/** One place an agent session takes instructions, settings, skills, memory or a mode from. */
export interface RuleLayer {
  scope: RuleScope;
  agent: AgentKind;
  kind: RuleKind;
  path: string;
  exists: boolean;
  size?: number;
  mtime?: number;
  content?: string;
  truncated: boolean;
  imports?: RuleImport[];
  note: string;
  loaded: boolean;
  skills: number;
  error?: string;
}

export interface RulesView {
  agent: AgentKind;
  root: string;
  task?: string;
  researched: string;
  layers: RuleLayer[];
  summary: { instruction_files: number; instruction_bytes: number; settings_files: number; skills: number };
}
