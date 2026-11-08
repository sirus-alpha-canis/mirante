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
  swap_used: number;
  swap_total: number;
  paused: boolean;
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
} as const;
