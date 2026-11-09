// The client store: one reducer over (snapshot | event | refreshed view | history). The
// daemon's event stream is a hint (seq-ordered, may replay); task views fetched with
// `task.get` / `task.list` are the authority and overwrite what events guessed.
import type { DaemonEvent, PermissionView, QueuedMessage, SendMode, Snapshot, TaskView } from "../lib/protocol";
import { describeMemoryEvent, isMemoryEvent, memoryMsg } from "../lib/memguard";
import { type Key, type Params, t } from "../lib/i18n";

export type FeedKind = "tool" | "command" | "agent_message" | "message" | "turn" | "system" | "permission";

export interface FeedItem {
  seq: number;
  ts: number;
  kind: FeedKind;
  text: string;
  ok?: boolean;
  mode?: SendMode;
  phase?: "started" | "ended";
  /** A tool step's tool name (`Read`, `Bash`…). */
  tool?: string;
  /** A line that warns (the agent left manual permission mode, skills may be lost or taken back). */
  warn?: boolean;
  /** A line in our words: how to say it in the current language (see `feedText`). */
  msg?: FeedMsg;
}

export interface FeedMsg {
  key: Key;
  params?: Params;
}

export type PermState = "pending" | "allowed_once" | "allowed_session" | "denied" | "expired";

export interface Perm {
  id: string;
  task_id: string;
  tool: string;
  detail: string;
  status: PermState;
  created_at: number;
  /** seq of the last event that changed it (older replays never win). */
  seq: number;
  /** `State.permRev` when it last changed (a `permission.list` sent earlier never wins). */
  rev: number;
  /** seq of the event that closed it: the only one of its closing events with a feed line. */
  closedBy?: number;
  /** The live stream already lowered the task's `needs_you` for it. */
  counted?: boolean;
}

export interface MemoryNotice {
  seq: number;
  /** When the daemon said it (ms since the epoch). */
  ts: number;
  task_id: string | null;
  kind: string;
  text: string;
  /** How to say it in the current language (see `feedText`). */
  msg?: FeedMsg;
}

export type Connection =
  | { state: "connecting" }
  | { state: "live" }
  | { state: "down"; error: string; retryMs: number };

export interface State {
  connection: Connection;
  bootId: string | null;
  seq: number;
  tasks: Record<string, TaskView>;
  feed: Record<string, FeedItem[]>;
  perms: Record<string, Perm>;
  /** Bumped on every permission change made by an event. */
  permRev: number;
  memory: MemoryNotice[];
  /** Each live task's queued messages, next first, as `message.list_queued` last said
   * (events adjust it meanwhile). Absent: not loaded. */
  queues: Record<string, QueuedMessage[]>;
  /** Tasks whose history (`task.events`) could not be loaded, with why. */
  historyErrors: Record<string, string>;
  /** Repo memory: bumped per repo on `dna.*` events, so memory views refetch (see
   * [`dnaRev`]). `"*"`: an event whose repo is unknown (every repo refetches). */
  dnaVersion: Record<string, number>;
  /** A path a view names a repo by (a project, a task's `repo_path`) → the daemon's key for
   * that repo's memory (`DnaStatus.repo`: git's top level, canonical), learned from
   * `dna.status` replies. `dna.*` events name the key. */
  dnaKeys: Record<string, string>;
  /** Skills: bumped on library, proposal, learning and origin events, so skills views
   * refetch (skills are global; their keys are tenants, not repos). */
  skillsVersion: number;
  /** `skillsVersion` as views see it: the cockpit settles it a moment after a burst of
   * skill events (see `SKILLS_COALESCE_MS`), so a burst refetches once. */
  skillsRev: number;
  /** Per task: bumped on its `skill.injected` / `refused` / `unavailable` / `used`. */
  skillTaskVersion: Record<string, number>;
  /** Per task: what its latest session was given (from its `skill.injected` and
   * `skill.refused` events). */
  sessionSkills: Record<string, SessionSkills>;
}

export interface SessionSkills {
  session: string;
  names: string[];
  refused: { name: string; reason: string }[];
  /** Injected skills the agent loaded (Claude's `Skill` tool), once each. */
  used: string[];
}

export const FEED_CAP = 300;

export const initialState: State = {
  connection: { state: "connecting" },
  bootId: null,
  seq: 0,
  tasks: {},
  feed: {},
  perms: {},
  permRev: 0,
  memory: [],
  queues: {},
  historyErrors: {},
  dnaVersion: {},
  dnaKeys: {},
  skillsVersion: 0,
  skillsRev: 0,
  skillTaskVersion: {},
  sessionSkills: {},
};

/** Events after which skills views (library, proposals, tenants, a task's chip) refetch. */
const SKILLS_REFETCH = new Set([
  "skill.proposed",
  "skill.decided",
  "skill.changed",
  "skill.mined",
  "skill.learning",
  "skill.origin_confirmed",
  "skill.origin_pinned",
]);
/** Events about one task's session skills. */
const SKILL_TASK = new Set(["skill.injected", "skill.refused", "skill.unavailable", "skill.used", "skill.withdrawn"]);

type SkillSlice = Pick<State, "skillsVersion" | "skillTaskVersion" | "sessionSkills">;

function skillEffect(state: SkillSlice, e: DaemonEvent): SkillSlice {
  // Only the slice: callers spread it over the whole state.
  const s: SkillSlice = { skillsVersion: state.skillsVersion, skillTaskVersion: state.skillTaskVersion, sessionSkills: state.sessionSkills };
  if (SKILLS_REFETCH.has(e.kind)) return { ...s, skillsVersion: s.skillsVersion + 1 };
  const id = e.task_id;
  if (!id || !SKILL_TASK.has(e.kind)) return s;
  const p = e.payload ?? {};
  const skillTaskVersion = { ...s.skillTaskVersion, [id]: (s.skillTaskVersion[id] ?? 0) + 1 };
  const session = str(p.session_id);
  const cur = s.sessionSkills[id];
  let sessionSkills = s.sessionSkills;
  if (e.kind === "skill.injected") {
    const names = Array.isArray(p.names) ? p.names.map(str) : [];
    sessionSkills = { ...sessionSkills, [id]: { session, names, refused: [], used: [] } };
  } else if (e.kind === "skill.refused") {
    const base = cur && cur.session === session ? cur : { session, names: [], refused: [], used: [] };
    const refused = [...base.refused.filter((r) => r.name !== str(p.name)), { name: str(p.name), reason: str(p.reason) }];
    sessionSkills = { ...sessionSkills, [id]: { ...base, refused } };
  } else if (e.kind === "skill.used" && cur && cur.session === session && !cur.used.includes(str(p.name))) {
    sessionSkills = { ...sessionSkills, [id]: { ...cur, used: [...cur.used, str(p.name)] } };
  } else if (e.kind === "skill.withdrawn" && cur && cur.session === session) {
    const gone = new Set(Array.isArray(p.names) ? p.names.map(str) : []);
    sessionSkills = { ...sessionSkills, [id]: { ...cur, names: cur.names.filter((n) => !gone.has(n)) } };
  } else if (e.kind === "skill.unavailable" && session && cur && cur.session === session) {
    // The session's bundle failed after it was announced: it holds none of them.
    sessionSkills = { ...sessionSkills, [id]: { ...cur, names: [], used: [] } };
  }
  return { ...s, skillTaskVersion, sessionSkills };
}


/** Events that change what memory views show. */
const DNA_REFETCH = new Set(["dna.indexed", "dna.changed", "dna.proposed", "dna.decided"]);

/** When `repo`'s memory views last had to refetch (a counter; compare for change). `repo`
 * is any path of the repo: events keyed by its canonical key count once that key is known
 * (see `dnaKey`), events keyed by the path itself count always. */
export function dnaRev(s: Pick<State, "dnaVersion" | "dnaKeys">, repo: string): number {
  const key = s.dnaKeys[repo];
  const viaKey = key && key !== repo ? (s.dnaVersion[key] ?? 0) : 0;
  return (s.dnaVersion[repo] ?? 0) + viaKey + (s.dnaVersion["*"] ?? 0);
}

function dnaEffect(v: State["dnaVersion"], tasks: Record<string, TaskView>, e: DaemonEvent): State["dnaVersion"] {
  if (!DNA_REFETCH.has(e.kind)) return v;
  const p = e.payload ?? {};
  const repo = typeof p.repo === "string" ? p.repo : e.task_id ? tasks[e.task_id]?.repo_path : undefined;
  const key = repo ?? "*";
  return { ...v, [key]: (v[key] ?? 0) + 1 };
}

export type Action =
  | { type: "connected"; snapshot: Snapshot }
  | { type: "disconnected"; error: string; retryMs: number }
  | { type: "event"; event: DaemonEvent }
  | { type: "task"; view: TaskView }
  /** `keep`: rows refreshed after this list was asked for (their current view stays). */
  | { type: "tasks"; views: TaskView[]; keep?: string[] }
  /** `rev`: `State.permRev` when the list was asked for (cards changed since are kept). */
  | { type: "permissions"; list: PermissionView[]; rev?: number }
  | { type: "history"; taskId: string; events: DaemonEvent[] }
  | { type: "queue"; taskId: string; list: QueuedMessage[] }
  | { type: "historyFailed"; taskId: string; error: string }
  /** `path`'s memory is the daemon's `key` (a `dna.status` reply said so). */
  | { type: "dnaKey"; path: string; key: string }
  /** Skills views may refetch now (`skillsRev` catches up with `skillsVersion`). */
  | { type: "skillsSettled" };

/** Events that close a card. `permission_answered` always comes first from the daemon; the
 * others refine it (or stand alone when the answer was lost or is outside the page). */
const CLOSING = new Set(["permission_answered", "permission_settled", "permission_expired", "permission_superseded"]);

const ANSWER_KEY: Record<string, Key> = {
  allowed_once: "feed.allowedOnce",
  allowed_session: "feed.allowedSession",
  denied: "feed.denied",
};

const QUEUE_CHANGE_KEY: Record<string, Key> = {
  edited: "feed.queueEdited",
  removed: "feed.queueRemoved",
  reordered: "feed.queueReordered",
};

/** Claude permission modes that approve on their own (the daemon's `self_approves`). */
export const SELF_APPROVING = new Set(["auto", "bypassPermissions", "acceptEdits"]);

const str = (v: unknown) => (typeof v === "string" ? v : v == null ? "" : String(v));
const names = (p: Record<string, unknown>) => (Array.isArray(p.names) ? p.names.map(str) : []);

/** A line in our words: its text now (the language of the moment) and how to say it again
 * in another language (`feedText`). */
const say = (key: Key, params?: Params): { text: string; msg: FeedMsg } => ({ text: t(key, params), msg: params ? { key, params } : { key } });

/** What a feed line shows, in the current language (our words are said again; the agent's
 * and the daemon's stay as they came). */
export function feedText(item: Pick<FeedItem, "text" | "msg">): string {
  return item.msg ? t(item.msg.key, item.msg.params) : item.text;
}

/** The feed line an event adds, if any. */
export function feedItem(e: DaemonEvent): FeedItem | null {
  const p = e.payload ?? {};
  const base = { seq: e.seq, ts: e.ts_ms };
  const sys = (key: Key, params?: Params, warn?: boolean): FeedItem => ({ ...base, kind: "system", ...say(key, params), ...(warn ? { warn } : {}) });
  switch (e.kind) {
    case "step": {
      switch (p.type) {
        case "tool":
          return { ...base, kind: "tool", tool: str(p.tool), text: `${str(p.tool)} ${str(p.summary)}`.trim(), ok: p.ok !== false };
        case "command":
          return { ...base, kind: "command", text: `$ ${str(p.command)}`, ok: p.exit_code === 0 };
        case "agent_message":
          return { ...base, kind: "agent_message", text: str(p.text) };
        case "skills_failed":
          return sys("feed.skillsFailed", { error: str(p.error) });
        case "skills_withdrawn":
          return sys("feed.skillsWithdrawnStep", { names: names(p).join(", ") });
        case "skills_maybe_lost":
          return sys("feed.skillsMaybeLost", { error: str(p.error) }, true);
        default:
          return { ...base, kind: "system", text: [str(p.type), str(p.error)].filter(Boolean).join(": ") };
      }
    }
    case "message_queued":
      return { ...base, kind: "message", mode: "queue", text: str(p.text) };
    case "message_sent":
      return p.mode === "queue" ? sys("feed.queuedDelivered") : { ...base, kind: "message", mode: "steer", text: str(p.text) };
    case "message_delivery":
      return sys("feed.messageDelivery", { delivery: str(p.delivery) });
    case "message_failed":
      return sys("feed.messageFailed", { error: str(p.error) });
    case "queue_dropped":
      return sys("feed.queueDropped");
    case "queue_changed":
      return sys(QUEUE_CHANGE_KEY[str(p.op)] ?? "feed.queueChanged");
    case "turn_started":
      return { ...base, kind: "turn", phase: "started", ...say("feed.turnStarted") };
    case "turn_ended":
      return { ...base, kind: "turn", phase: "ended", ...say("feed.turnEnded") };
    case "permission_requested":
      return { ...base, kind: "permission", ...say("feed.asks", { what: `${str(p.tool)} ${str(p.detail)}`.trim() }) };
    case "permission_answered":
    case "permission_settled":
      return sys(ANSWER_KEY[str(p.answer)] ?? "feed.denied");
    case "permission_expired":
      return sys("feed.permExpired");
    case "permission_superseded":
      return sys("feed.permSuperseded");
    case "boot":
      return p.state === "trust_prompt" || p.state === "exited" ? sys(p.state === "exited" ? "feed.exitedDuringStart" : "feed.asksTrust") : null;
    case "process_exited":
      return str(p.code) ? sys("feed.agentExited", { code: str(p.code) }) : sys("feed.agentExited", { code: t("feed.none") });
    case "interrupt_sent":
      return sys("feed.interrupted");
    case "dna.proposed":
      return sys("feed.dnaProposed", { title: str(p.title) });
    case "dna.decided":
      return p.accepted === true ? sys("feed.dnaAccepted", { slug: str(p.slug) }) : sys("feed.dnaRejected");
    case "dna.unavailable":
      return str(p.reason) ? { ...base, kind: "system", text: str(p.reason) } : sys("feed.dnaOff");
    case "permission_mode": {
      // Mirante starts Claude in manual mode: leaving it is worth a warning, going back a note.
      if (p.self_approves === "commands") return sys("feed.autoOn", undefined, true);
      if (p.self_approves === "edits") return sys("feed.editsOn", undefined, true);
      return SELF_APPROVING.has(str(p.from)) ? sys("feed.manualBack") : null;
    }
    case "session.auto_resumed":
      return sys("feed.autoResumed");
    case "session.auto_resume_skipped":
      return sys("feed.autoResumeSkipped", { reason: str(p.reason) });
    case "session.auto_resume_failed":
      return sys("feed.autoResumeFailed", { error: str(p.error) });
    case "session.auto_resume_pending":
      // The daemon's own bookkeeping (it will resume this session); auto_resumed says the rest.
      return null;
    case "skill.injected": {
      const versions = Array.isArray(p.versions) ? p.versions : [];
      const list = names(p).map((n, i) => (versions[i] == null ? n : `${n} v${str(versions[i])}`));
      return sys("feed.skillsGiven", { list: list.join(", ") });
    }
    case "skill.withdrawn":
      return sys("feed.skillsWithdrawn", { names: names(p).join(", ") });
    case "skill.refused":
      return sys("feed.skillRefused", { name: str(p.name), reason: str(p.reason) });
    case "skill.unavailable":
      // With a session: its skills were announced, then could not be written.
      return p.session_id ? sys("feed.skillsTakenBack", { reason: str(p.reason) }, true) : sys("feed.noSkills", { reason: str(p.reason) });
    case "skill.used":
      return sys("feed.skillUsed", { name: str(p.name) });
    case "skill.proposed":
      return sys("feed.skillProposed", { name: str(p.name) });
    case "skill.decided":
      return p.accepted === true ? sys("feed.skillAccepted", { name: str(p.name), version: str(p.version) }) : sys("feed.skillRejected");
    default: {
      if (!isMemoryEvent(e.kind)) return null;
      const m = memoryMsg(e.kind, p);
      return m ? sys(m.key, m.params) : { ...base, kind: "system", text: e.kind };
    }
  }
}

function addFeed(feed: Record<string, FeedItem[]>, taskId: string, items: FeedItem[]): Record<string, FeedItem[]> {
  if (items.length === 0) return feed;
  const cur = feed[taskId] ?? [];
  const seen = new Set(cur.map((i) => i.seq));
  const fresh = items.filter((i) => !seen.has(i.seq));
  if (fresh.length === 0) return feed;
  const merged = [...cur, ...fresh].sort((a, b) => a.seq - b.seq).slice(-FEED_CAP);
  return { ...feed, [taskId]: merged };
}

function permId(e: DaemonEvent): string {
  return e.kind === "permission_answer_unrecorded" ? "" : str((e.payload ?? {}).permission_id);
}

/** Whether `e` is the event a card's single feed line belongs to (its first closing event
 * seen, or any non-closing event). */
function ownsLine(perms: Record<string, Perm>, e: DaemonEvent): boolean {
  if (!CLOSING.has(e.kind)) return true;
  const closedBy = perms[permId(e)]?.closedBy;
  return closedBy === undefined || closedBy === e.seq;
}

const statusOf = (e: DaemonEvent): PermState => {
  const a = str((e.payload ?? {}).answer);
  if (e.kind === "permission_expired") return "expired";
  return a === "allowed_once" || a === "allowed_session" ? a : "denied";
};

function permEffect(perms: Record<string, Perm>, e: DaemonEvent, rev: number): Record<string, Perm> {
  const p = e.payload ?? {};
  const id = permId(e);
  // An answer typed after the card was settled is audit only: the card keeps its outcome.
  // (`permission_superseded` is recorded denied by the daemon, so it settles as one.)
  if (!id || !e.task_id || !e.kind.startsWith("permission_")) return perms;
  const cur = perms[id];
  const closedBy = CLOSING.has(e.kind) ? (cur?.closedBy ?? e.seq) : cur?.closedBy;
  if (cur && cur.seq >= e.seq) return closedBy === cur.closedBy ? perms : { ...perms, [id]: { ...cur, closedBy } };
  if (e.kind === "permission_requested") {
    const status: PermState = cur?.status ?? "pending";
    return {
      ...perms,
      [id]: { ...cur, id, task_id: e.task_id, tool: str(p.tool), detail: str(p.detail), status, created_at: e.ts_ms, seq: e.seq, rev },
    };
  }
  if (!CLOSING.has(e.kind)) return perms;
  const status = statusOf(e);
  const known = cur ?? { id, task_id: e.task_id, tool: "", detail: "", created_at: e.ts_ms, status, seq: e.seq, rev };
  return { ...perms, [id]: { ...known, status, seq: e.seq, rev, closedBy } };
}

/** What an event says about a loaded queue (the `message.list_queued` refetch it triggers
 * is the authority). */
function queueEffect(queues: State["queues"], e: DaemonEvent): State["queues"] {
  const id = e.task_id;
  const cur = id ? queues[id] : undefined;
  if (!id || !cur) return queues;
  const p = e.payload ?? {};
  const put = (list: QueuedMessage[]) => ({ ...queues, [id]: list });
  switch (e.kind) {
    case "message_queued":
      return typeof p.id === "string" && !cur.some((m) => m.id === p.id) ? put([...cur, { id: p.id, text: str(p.text) }]) : queues;
    case "message_sent":
      return typeof p.id === "string" ? put(cur.filter((m) => m.id !== p.id)) : queues;
    case "queue_dropped":
    case "session_ended":
    case "process_exited":
      return put([]);
    case "queue_changed":
      switch (p.op) {
        case "edited":
          return put(cur.map((m) => (m.id === p.id ? { ...m, text: str(p.text) } : m)));
        case "removed":
          return put(cur.filter((m) => m.id !== p.id));
        case "reordered": {
          const ids = Array.isArray(p.ids) ? p.ids.map(str) : [];
          const rank = (m: QueuedMessage) => (ids.includes(m.id) ? ids.indexOf(m.id) : ids.length);
          return put([...cur].sort((a, b) => rank(a) - rank(b)));
        }
        default:
          return queues;
      }
    default:
      return queues;
  }
}

/** What an event says about the task row itself (refined later by `task.get`). */
function taskEffect(tasks: Record<string, TaskView>, e: DaemonEvent): Record<string, TaskView> {
  const id = e.task_id;
  const p = e.payload ?? {};
  if (!id) return tasks;
  if (e.kind === "task_created") {
    const defaults = { live: false, boot: null, needs_you: 0, queued: 0, detail: null };
    return { ...tasks, [id]: { ...defaults, ...p } as TaskView };
  }
  const t = tasks[id];
  if (!t) return tasks;
  const put = (patch: Partial<TaskView>) => ({ ...tasks, [id]: { ...t, ...patch } });
  switch (e.kind) {
    case "status":
      return put({ status: p.to as TaskView["status"] });
    case "lifecycle":
      return put({ lifecycle: p.to as TaskView["lifecycle"] });
    case "boot":
      return put({ boot: p.state as TaskView["boot"], live: p.state !== "exited" });
    case "session_started":
      return put({ live: true });
    case "session_ended":
    case "process_exited":
      return put({ live: false, boot: null });
    case "permission_requested":
      return put({ needs_you: t.needs_you + 1 });
    case "message_queued":
      return put({ queued: t.queued + 1 });
    case "task_updated":
      return put({ goal: str(p.goal), notes: str(p.notes) });
    default:
      return tasks;
  }
}

/** Structural equality of JSON-shaped values (daemon replies). */
export function same(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a);
  const kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  const ob = b as Record<string, unknown>;
  return ka.every((k) => Object.hasOwn(ob, k) && same((a as Record<string, unknown>)[k], ob[k]));
}

/** `m` without the entries whose key is no task of `tasks` (`m` itself when none goes). */
function keepIds<T>(m: Record<string, T>, tasks: Record<string, TaskView>): Record<string, T> {
  return keepTasks(m, (_v, k) => k, tasks);
}

function keepTasks<T>(m: Record<string, T>, taskOf: (v: T, k: string) => string, tasks: Record<string, TaskView>): Record<string, T> {
  const gone = Object.keys(m).filter((k) => !(taskOf(m[k], k) in tasks));
  if (gone.length === 0) return m;
  const out = { ...m };
  for (const k of gone) delete out[k];
  return out;
}

/** `next` with each entry that equals `prev`'s kept as `prev`'s object; `prev` itself when
 * nothing differs (so views of unchanged rows, and of nothing, do not render again). */
function keepSame<T>(prev: Record<string, T>, next: Record<string, T>): Record<string, T> {
  let changed = Object.keys(prev).length !== Object.keys(next).length;
  for (const k of Object.keys(next)) {
    if (k in prev && same(prev[k], next[k])) next[k] = prev[k];
    else changed = true;
  }
  return changed ? next : prev;
}

function memoryNotice(e: DaemonEvent): MemoryNotice {
  const m = memoryMsg(e.kind, e.payload ?? {});
  return { seq: e.seq, ts: e.ts_ms, task_id: e.task_id, kind: e.kind, text: describeMemoryEvent(e.kind, e.payload ?? {}), ...(m ? { msg: m } : {}) };
}

export function reduce(s: State, a: Action): State {
  switch (a.type) {
    case "connected": {
      const snap = a.snapshot;
      if (snap.resumed) return { ...s, connection: { state: "live" }, bootId: snap.boot_id, seq: snap.seq };
      const tasks: Record<string, TaskView> = {};
      for (const t of snap.tasks) tasks[t.id] = t;
      const base = { ...s, connection: { state: "live" } as Connection, bootId: snap.boot_id, seq: snap.seq, tasks };
      // What belonged to tasks no longer there goes.
      const perms = keepTasks(s.perms, (p) => p.task_id, tasks);
      if (s.bootId !== null && snap.boot_id !== s.bootId) {
        // A new daemon: its sessions, queues, notices and event numbers start over. Histories
        // are loaded again (they would not merge: seqs restart with the boot).
        return { ...base, perms, feed: {}, queues: {}, sessionSkills: {}, historyErrors: {}, memory: [] };
      }
      return {
        ...base,
        perms,
        feed: keepIds(s.feed, tasks),
        queues: keepIds(s.queues, tasks),
        historyErrors: keepIds(s.historyErrors, tasks),
        sessionSkills: keepIds(s.sessionSkills, tasks),
      };
    }
    case "disconnected":
      return { ...s, connection: { state: "down", error: a.error, retryMs: a.retryMs } };
    case "event": {
      const e = a.event;
      if (e.boot_id === s.bootId && e.seq <= s.seq) return s;
      const memory = isMemoryEvent(e.kind)
        ? [...s.memory, memoryNotice(e)].slice(-20)
        : s.memory;
      let tasks = taskEffect(s.tasks, e);
      let perms = permEffect(s.perms, e, s.permRev + 1);
      const permRev = perms === s.perms ? s.permRev : s.permRev + 1;
      const id = permId(e);
      // Each card lowers its task's count once, whichever of its closing events comes first.
      if (CLOSING.has(e.kind) && id && perms[id] && !perms[id].counted) {
        perms = { ...perms, [id]: { ...perms[id], counted: true } };
        const t = e.task_id ? tasks[e.task_id] : undefined;
        if (t) tasks = { ...tasks, [t.id]: { ...t, needs_you: Math.max(0, t.needs_you - 1) } };
      }
      const item = ownsLine(perms, e) ? feedItem(e) : null;
      return {
        ...s,
        bootId: e.boot_id,
        seq: e.seq,
        tasks,
        perms,
        permRev,
        feed: item && e.task_id ? addFeed(s.feed, e.task_id, [item]) : s.feed,
        memory,
        queues: queueEffect(s.queues, e),
        dnaVersion: dnaEffect(s.dnaVersion, tasks, e),
        ...skillEffect(s, e),
      };
    }
    case "task":
      return same(s.tasks[a.view.id], a.view) ? s : { ...s, tasks: { ...s.tasks, [a.view.id]: a.view } };
    case "tasks": {
      const tasks: Record<string, TaskView> = {};
      for (const t of a.views) tasks[t.id] = t;
      for (const id of a.keep ?? []) if (s.tasks[id]) tasks[id] = s.tasks[id];
      const kept = keepSame(s.tasks, tasks);
      return kept === s.tasks ? s : { ...s, tasks: kept };
    }
    case "permissions": {
      const asked = a.rev ?? Infinity;
      const perms = { ...s.perms };
      // Cards an event changed after the list was asked for: the event is newer.
      const newer = (id: string) => (perms[id]?.rev ?? 0) > asked;
      for (const p of a.list) {
        if (newer(p.id)) continue;
        const cur = perms[p.id];
        perms[p.id] = {
          ...cur,
          id: p.id,
          task_id: p.task_id,
          tool: p.tool,
          detail: p.detail,
          status: p.status,
          created_at: p.created_at,
          seq: cur?.seq ?? 0,
          rev: cur?.rev ?? 0,
        };
      }
      // A pending card the daemon no longer lists was answered elsewhere (CLI) or expired.
      for (const id of Object.keys(perms)) {
        if (perms[id].status === "pending" && !newer(id) && !a.list.some((p) => p.id === id)) {
          perms[id] = { ...perms[id], status: "expired" };
        }
      }
      const kept = keepSame(s.perms, perms);
      return kept === s.perms ? s : { ...s, perms: kept };
    }
    case "history": {
      let perms = s.perms;
      let permRev = s.permRev;
      const items: FeedItem[] = [];
      let skills: Pick<SkillSlice, "sessionSkills"> = s;
      for (const e of [...a.events].sort((x, y) => x.seq - y.seq)) {
        // Only what the task's session got (library events are not history to replay).
        if (SKILL_TASK.has(e.kind) && e.task_id === a.taskId) {
          skills = { sessionSkills: skillEffect({ ...s, ...skills }, e).sessionSkills };
        }
        const next = permEffect(perms, e, permRev + 1);
        if (next !== perms) permRev++;
        perms = next;
        const item = ownsLine(perms, e) ? feedItem(e) : null;
        if (item) items.push(item);
      }
      const feed = addFeed(s.feed, a.taskId, items);
      if (feed === s.feed && perms === s.perms && skills.sessionSkills === s.sessionSkills && !(a.taskId in s.historyErrors)) return s;
      const { [a.taskId]: _cleared, ...historyErrors } = s.historyErrors;
      return { ...s, feed, perms, permRev, historyErrors, sessionSkills: skills.sessionSkills };
    }
    case "queue":
      return same(s.queues[a.taskId], a.list) ? s : { ...s, queues: { ...s.queues, [a.taskId]: a.list } };
    case "historyFailed":
      return { ...s, historyErrors: { ...s.historyErrors, [a.taskId]: a.error } };
    case "dnaKey":
      return s.dnaKeys[a.path] === a.key ? s : { ...s, dnaKeys: { ...s.dnaKeys, [a.path]: a.key } };
    case "skillsSettled":
      return s.skillsRev === s.skillsVersion ? s : { ...s, skillsRev: s.skillsVersion };
  }
}
