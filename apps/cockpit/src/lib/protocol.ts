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
} as const;
