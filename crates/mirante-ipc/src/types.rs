//! Method names and their parameter / result types. A task may be referenced by its full
//! id, its branch name, or a unique suffix of its id (at least 4 characters).
use mirante_core::groups::TaskGroup;
use mirante_core::model::{AgentKind, GroupKind, Lifecycle, PermissionRequest, Status, Task, TaskRepo};
use mirante_core::orchestration::{ProposalStatus, SpawnProposal};
use mirante_core::threads::{Moved, SummaryBy, ThreadLink};
use serde::{Deserialize, Serialize};

pub mod method {
    pub const TASK_CREATE: &str = "task.create";
    pub const TASK_LIST: &str = "task.list";
    pub const TASK_GET: &str = "task.get";
    pub const TASK_ARCHIVE: &str = "task.archive";
    /// A task's audit trail, oldest first ([`super::TaskEventsParams`]).
    pub const TASK_EVENTS: &str = "task.events";
    /// The task branch's changes against its base ([`super::TaskDiff`]).
    pub const TASK_DIFF: &str = "task.diff";
    pub const SESSION_START: &str = "session.start";
    pub const SESSION_RESUME: &str = "session.resume";
    pub const SESSION_TRUST: &str = "session.trust";
    pub const SESSION_STOP: &str = "session.stop";
    pub const SESSION_INTERRUPT: &str = "session.interrupt";
    /// The attached terminal changed size ([`super::ResizeParams`]).
    pub const SESSION_RESIZE: &str = "session.resize";
    pub const MESSAGE_SEND: &str = "message.send";
    /// A live session's queued messages, next first ([`super::QueueRef`]; result:
    /// `Vec<`[`super::QueuedMessage`]`>`).
    pub const MESSAGE_LIST_QUEUED: &str = "message.list_queued";
    /// Rewrites a message still in the queue ([`super::EditQueued`]; result: the queue).
    pub const MESSAGE_EDIT: &str = "message.edit";
    /// Takes a message out of the queue ([`super::QueuedRef`]; result: the queue).
    pub const MESSAGE_REMOVE: &str = "message.remove";
    /// Sets the queue's whole order ([`super::ReorderQueue`]; result: the queue).
    pub const MESSAGE_REORDER: &str = "message.reorder";
    pub const PERMISSION_LIST: &str = "permission.list";
    pub const PERMISSION_ANSWER: &str = "permission.answer";
    pub const EVENTS_SUBSCRIBE: &str = "events.subscribe";
    pub const ATTACH: &str = "attach";
    pub const HOOK_EVENT: &str = "hook.event";
    pub const DAEMON_STATUS: &str = "daemon.status";
    pub const DAEMON_SHUTDOWN: &str = "daemon.shutdown";
    /// Ends an idle session to free memory, keeping it resumable ([`super::TaskRef`]).
    pub const SESSION_HIBERNATE: &str = "session.hibernate";
    /// Resumes a hibernated session ([`super::TaskRef`]; result: [`super::StartResult`]).
    pub const SESSION_WAKE: &str = "session.wake";
    /// Orphan processes / checkouts and memory ([`super::DoctorReport`]).
    pub const DAEMON_DOCTOR: &str = "daemon.doctor";
    /// Kills one orphan the user picked from `daemon.doctor` ([`super::KillOrphan`]).
    pub const DAEMON_KILL_ORPHAN: &str = "daemon.kill_orphan";
    /// A repository's files whose path matches a query, for attaching them to a goal
    /// ([`super::RepoFiles`] → `Vec<String>`, relative paths). A person only.
    pub const REPO_FILES: &str = "repo.files";
    /// A task's worktree files (tracked and untracked, not ignored) whose path matches a
    /// query, for the cockpit's quick-open ([`super::TaskFiles`] → `Vec<String>`, relative
    /// to where the agent runs). A person only.
    pub const TASK_FILES: &str = "task.files";
    /// One file of a task's worktree, read by descriptor walk ([`super::TaskFile`] →
    /// [`super::FileView`]). A person only.
    pub const TASK_FILE: &str = "task.file";
    /// Claude's own agents (subagents) in a task's live session, read-only
    /// ([`super::TaskSubagents`] → `Vec<`[`super::SubagentView`]`>`). A person only.
    pub const TASK_SUBAGENTS: &str = "task.subagents";
    /// A person's fan-out group: a shared goal its children are created into
    /// ([`super::CreateGroup`] → `TaskGroup`). No worktree, no session. A person only.
    pub const GROUP_CREATE: &str = "group.create";
    /// A group and its tasks with each one's last answer and diff stat
    /// ([`super::GroupRef`] → [`super::GroupView`]). A person only.
    pub const GROUP_GET: &str = "group.get";
    /// Archives every task of a group, then the group: refused before anything is archived
    /// when one of them is live or holds uncommitted work ([`super::GroupRef`] →
    /// [`super::GroupView`]). A person only.
    pub const GROUP_ARCHIVE: &str = "group.archive";
    /// The children's results, fenced as data, for a new task in the group or for one of
    /// its children; a preview first ([`super::Synthesize`] → [`super::Synthesis`]). A
    /// person only.
    pub const GROUP_SYNTHESIZE: &str = "group.synthesize";
    /// A thread's notes and/or link, changed by the person against the hash they edited
    /// ([`super::ThreadEdit`] → [`super::TaskView`]). A person only.
    pub const TASK_THREAD: &str = "task.thread";
    /// A thread's summary: deterministic (status, last answer, diff stat), or, on the
    /// person's click, from their own Claude CLI run headless and tool-less
    /// ([`super::Summarize`] → [`super::SummaryView`]). A person only.
    pub const TASK_SUMMARIZE: &str = "task.summarize";
    /// A person turns orchestration on or off for a task ([`super::Orchestrate`] →
    /// [`super::TaskView`]): while on, its next sessions get the `mirante-orchestrator` MCP
    /// server. Never for a child task. A person only.
    pub const TASK_ORCHESTRATE: &str = "task.orchestrate";
    /// A task's agent proposes child tasks, each waiting for a person's decision
    /// ([`super::SpawnPropose`] → `Vec<`[`super::SpawnProposalView`]`>`). Refused while
    /// orchestration is off. An agent proposes for its own task only.
    pub const TASK_SPAWN_PROPOSE: &str = "task.spawn_propose";
    /// Proposed child tasks, of one task or every one's ([`super::SpawnProposals`] →
    /// `Vec<`[`super::SpawnProposalView`]`>`). A person only.
    pub const TASK_SPAWN_PROPOSALS: &str = "task.spawn_proposals";
    /// A person approves (creates and starts the child, with their edits) or rejects a
    /// proposal ([`super::SpawnReview`] → [`super::SpawnReviewed`]). A person only.
    pub const TASK_SPAWN_REVIEW: &str = "task.spawn_review";
    /// The children a task's agent proposed and a person approved, and its proposals
    /// ([`super::TaskRef`] → [`super::ChildrenView`]; agent text redacted and capped). An
    /// agent reads its own task's only.
    pub const TASK_CHILDREN: &str = "task.children";
    /// One approved child's result: status, last finished answer, diff stat
    /// ([`super::ChildResultParams`] → [`super::ChildResult`]). An agent reads its own
    /// task's children only.
    pub const TASK_CHILD_RESULT: &str = "task.child_result";
    /// The daemon's settings, changing the ones given ([`super::SettingsParams`] →
    /// [`super::DaemonSettings`]). A person only.
    pub const DAEMON_SETTINGS: &str = "daemon.settings";
    /// Tenants (normalized origin owners) with learning state and counts (`{}` →
    /// `Vec<`[`super::TenantView`]`>`).
    pub const SKILL_TENANTS: &str = "skill.tenants";
    /// A person turns mining on or off for a tenant ([`super::SkillLearn`] →
    /// [`super::TenantView`]).
    pub const SKILL_LEARN: &str = "skill.learn";
    /// The library ([`super::SkillListParams`] → `Vec<`[`super::SkillSummary`]`>`).
    pub const SKILL_LIST: &str = "skill.list";
    /// One skill, a version of it, its files and verification ([`super::SkillGet`] →
    /// [`super::SkillView`]).
    pub const SKILL_GET: &str = "skill.get";
    /// A person confirms a checkout's origin after it changed ([`super::SkillConfirmOrigin`]
    /// → [`super::OriginConfirmed`]).
    pub const SKILL_CONFIRM_ORIGIN: &str = "skill.confirm_origin";
    /// Pure lint of a draft ([`super::SkillLintParams`] → `Vec<`[`super::Finding`]`>`).
    pub const SKILL_LINT: &str = "skill.lint";
    /// A proposed skill or edit, for a person's review ([`super::SkillPropose`] →
    /// [`super::SkillProposalView`]). An agent proposes only for its own task.
    pub const SKILL_PROPOSE: &str = "skill.propose";
    /// A person imports a skill folder or inline text as a proposal ([`super::SkillAdd`] →
    /// [`super::SkillProposalView`]).
    pub const SKILL_ADD: &str = "skill.add";
    /// A person queues a skill version's folder, as it is on disk now, for review again
    /// ([`super::SkillRereview`] → [`super::SkillProposalView`]). The daemon builds the
    /// path from the name and version (the caller never names a folder); a folder whose
    /// `SKILL.md` names another skill is refused. A person only.
    pub const SKILL_REREVIEW: &str = "skill.rereview";
    /// The review queue ([`super::SkillProposals`] → `Vec<`[`super::SkillProposalView`]`>`).
    pub const SKILL_PROPOSALS: &str = "skill.proposals";
    /// How many proposals wait for review, every client's (`{}` → [`super::SkillPending`]).
    pub const SKILL_PENDING: &str = "skill.pending";
    /// A person accepts (writes, signs and ledgers a version) or rejects a proposal
    /// ([`super::SkillReview`] → [`super::SkillProposalView`]).
    pub const SKILL_REVIEW: &str = "skill.review";
    /// A person retires, restores, rescopes or rolls back a skill ([`super::SkillAction`] →
    /// [`super::SkillView`]).
    pub const SKILL_ACTION: &str = "skill.action";
    /// A person finds repeated instructions in a learning tenant's tasks
    /// ([`super::SkillMine`] → [`super::SkillMined`]).
    pub const SKILL_MINE: &str = "skill.mine";
    /// What a task's next session gets: its tenant, the matching verified skills and the
    /// refused ones ([`super::SkillTaskParams`] → [`super::TaskSkills`]).
    pub const SKILL_TASK_SKILLS: &str = "skill.task_skills";
    /// `{}` when the caller may review skills, else `FORBIDDEN`.
    pub const SKILL_MAY_REVIEW: &str = "skill.may_review";
    /// A checkout's pinned origin and its live one, as the daemon reads them, for the CLI's
    /// `skills tenant|learn|mine` ([`super::SkillRepoParams`] → [`super::RepoOrigin`]).
    /// A person only.
    pub const SKILL_REPO_ORIGIN: &str = "skill.repo_origin";
    /// Every instruction layer an agent session reads, in load order, for a checkout or a
    /// task ([`super::RulesParams`] → [`super::RulesView`]). Read-only; a person only.
    pub const RULES_LAYERS: &str = "rules.layers";
    /// Notification carrying one `mirante_core::model::Event`.
    pub const EVENT: &str = "event";
    /// A repo's memory: enabled, counts, HEAD ([`super::DnaRef`] → [`super::DnaStatus`]).
    pub const DNA_STATUS: &str = "dna.status";
    /// Creates `.mirante/memory/README.md` and turns injection on (a person only;
    /// Mirante-launched agents are refused).
    pub const DNA_ENABLE: &str = "dna.enable";
    /// Turns injection off for new sessions; the files stay (a person only).
    pub const DNA_DISABLE: &str = "dna.disable";
    /// Notes and `[[links]]` ([`super::DnaRef`] → [`super::DnaGraph`]).
    pub const DNA_GRAPH: &str = "dna.graph";
    /// Full-text search ([`super::DnaSearch`] → `Vec<`[`super::NoteSummary`]`>`).
    pub const DNA_SEARCH: &str = "dna.search";
    /// One note ([`super::DnaGet`] → [`super::NoteView`]).
    pub const DNA_GET: &str = "dna.get";
    /// Reindex and recheck every anchor at the checkout's HEAD ([`super::DnaRef`] →
    /// [`super::CheckReport`]).
    pub const DNA_CHECK: &str = "dna.check";
    /// An agent (or the CLI) proposes a note; it waits for a person's review
    /// ([`super::DnaPropose`] → [`super::ProposalView`]).
    pub const DNA_PROPOSE: &str = "dna.propose";
    /// The review queue ([`super::DnaProposals`] → `Vec<`[`super::ProposalView`]`>`).
    pub const DNA_PROPOSALS: &str = "dna.proposals";
    /// A person accepts (writes the note) or rejects a proposal ([`super::DnaReview`] →
    /// [`super::ProposalView`]). Mirante-launched agents are refused.
    pub const DNA_REVIEW: &str = "dna.review";
    /// A task's memory: notes relevant to it, notes its branch puts at risk, its proposals
    /// ([`super::DnaContextParams`] → [`super::DnaTaskContext`]).
    pub const DNA_TASK_CONTEXT: &str = "dna.task_context";
    /// A person confirms (re-anchors at HEAD) or retires a note ([`super::DnaNoteAction`] →
    /// [`super::NoteView`]). Mirante-launched agents are refused.
    pub const DNA_NOTE_ACTION: &str = "dna.note_action";
    /// `{}` when this caller may make a person's memory decision, else `FORBIDDEN` with
    /// why (the CLI asks before it prompts). The decision itself checks again.
    pub const DNA_MAY_REVIEW: &str = "dna.may_review";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    /// Deliver now: typed into Claude's TUI (absorbed at the next tool boundary) or
    /// `turn/steer` for Codex. Starts a turn when none is running.
    Steer,
    /// Hold in the daemon and deliver when the running turn ends.
    Queue,
}

/// Where a live session's agent process is in its startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootState {
    Booting,
    /// Claude's workspace-trust dialog is on screen and waits for the user's decision.
    TrustPrompt,
    Ready,
    Exited,
}

/// A task as clients see it: the stored task plus live-session facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskView {
    #[serde(flatten)]
    pub task: Task,
    pub live: bool,
    pub boot: Option<BootState>,
    /// Pending permission requests, plus one while a trust prompt waits.
    pub needs_you: usize,
    /// Messages held for delivery at the end of the running turn.
    pub queued: usize,
    /// Extra status detail, e.g. "turn done, 1 command still finishing".
    pub detail: Option<String>,
    /// Codex quota / token usage while live, e.g. "5h 42% · week 9% · 31k tokens".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
    /// Codex: the effective sandbox while live, e.g. "net:off" or "net:on, +1 root".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// Memory of the live session's process tree, once sampled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemView>,
    /// The session was hibernated to free memory (`mirante wake` resumes it).
    #[serde(default)]
    pub hibernated: bool,
    /// Claude, while live: its permission mode as its hooks and transcript last said
    /// (`default`, `plan`, `acceptEdits`, `auto`, `bypassPermissions`, ...). Mirante starts
    /// it in manual mode; anything that approves on its own is worth a warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// `task.create` only: the origin fixed for the task's skills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<CreatedOrigin>,
    /// A workspace task: each repository it may change, primary first (absent for a
    /// single-repo task, and from older daemons).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<TaskRepo>,
    /// A task of a fan-out group: the group (absent otherwise, and from older daemons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<TaskGroup>,
    /// A thread: its link, notes hash, delivery and summary (absent otherwise, and from
    /// older daemons). Its notes are the task's `notes`; its parent the task's `parent_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadView>,
    /// The person turned orchestration on: the task's next sessions get the
    /// `mirante-orchestrator` server (`task_spawn` proposals, reads of its own children).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub orchestration: bool,
    /// While live: whether this session was given that server (it applies from the next
    /// session when they differ).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_live: Option<bool>,
    /// Child tasks its agent proposed that wait for the person's decision.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub spawn_pending: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// A thread's state, as clients see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadView {
    pub link: ThreadLink,
    /// What `task.thread` must be given back as `expect_hash` (the link and notes now).
    pub notes_hash: String,
    /// The agent was handed these notes and link (at its start, a resume, or the message
    /// after they changed).
    pub delivered: bool,
    /// The newest summary (agent-derived text when `summary_by` is `claude`: shown as text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_by: Option<SummaryBy>,
}

/// `task.orchestrate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Orchestrate {
    pub task: String,
    pub enabled: bool,
}

/// One child task an agent proposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedChild {
    pub goal: String,
    /// Default: the proposing task's agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Why the agent wants it, for the person deciding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// `task.spawn_propose`: 1 to 5 children of `task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnPropose {
    pub task: String,
    pub children: Vec<ProposedChild>,
}

/// `task.spawn_proposals`: of one task (absent: every task's), of one status (absent: any).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnProposals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ProposalStatus>,
}

/// A proposal as clients see it: what the agent wrote, and the task it is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnProposalView {
    #[serde(flatten)]
    pub proposal: SpawnProposal,
    pub parent_title: String,
    pub parent_branch: String,
    /// A workspace parent: its repos' names (primary first). The child runs on these, or
    /// on fewer the person picks (the primary always). Empty for a single-repo parent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
}

/// What the person changes before approving: each one given replaces the proposal's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnEdits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A workspace parent: the repos (names) the child runs on; the primary is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
}

/// `task.spawn_review`. `start` (default true): an approved child starts at once with its
/// goal as the first prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnReview {
    pub id: String,
    pub accept: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<SpawnEdits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnReviewed {
    pub proposal: SpawnProposalView,
    /// The child an approval made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<TaskView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<StartResult>,
    /// The child was made but did not start (e.g. memory is tight): start it later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_error: Option<String>,
}

/// One approved child, as its parent's agent reads it (agent text redacted and capped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildView {
    pub id: String,
    pub title: String,
    pub branch: String,
    pub agent: AgentKind,
    pub status: Status,
    pub lifecycle: Lifecycle,
    pub live: bool,
    /// A permission (or a trust prompt) waits for the person in it.
    pub needs_you: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
    /// The start of its newest finished answer. Agent text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_answer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildrenView {
    pub children: Vec<ChildView>,
    /// What the agent proposed, with where each stands (the newest last).
    pub proposals: Vec<SpawnProposal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildResultParams {
    pub task: String,
    pub child: String,
}

/// One approved child's result (agent text redacted and capped).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildResult {
    pub child: String,
    pub title: String,
    pub branch: String,
    pub status: Status,
    pub lifecycle: Lifecycle,
    /// No session of it is running (it ended, or never started).
    pub ended: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_answer: Option<String>,
    /// `None` once archived or when git could not tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
}

/// `task.thread`: each one given replaces what the thread has; `expect_hash` is the
/// `notes_hash` the person edited (a `CONFLICT` when it changed meanwhile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadEdit {
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<ThreadLink>,
    pub expect_hash: String,
}

/// The origin `task.create` fixed for a task (its skills follow it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedOrigin {
    /// The normalized origin key (`host/path`, or `local/<hash>`).
    pub origin: String,
    pub tenant: String,
    /// The live origin when it is not confirmed: no skills until a person confirms it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    /// This task pinned the checkout's origin (its first, a person's).
    #[serde(default)]
    pub pinned: bool,
}

/// One live session's memory: the summed footprint of its process tree (macOS:
/// `phys_footprint`, what Activity Monitor shows), its peak, its budget and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemView {
    pub bytes: u64,
    pub peak: u64,
    pub budget: u64,
    /// "ok", "soft" (past the soft share of the budget) or "hard" (past the budget).
    pub state: String,
}

/// The machine's memory as the daemon's guard sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemView {
    /// "normal", "elevated" or "critical".
    pub level: String,
    pub total: u64,
    /// Strictly free pages (macOS: free + speculative + purgeable; Linux: `MemAvailable`).
    pub free: u64,
    /// What the OS says is available: macOS `kern.memorystatus_level` (its free percentage)
    /// times the RAM, Linux `MemAvailable`. Absent from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available: Option<u64>,
    pub swap_used: u64,
    pub swap_total: u64,
    /// New sessions and queued deliveries wait (only when `pausing` is set and the level
    /// reached it).
    pub paused: bool,
    /// The operating system's own pressure measure, when it gave one and it decided `level`:
    /// "normal", "warn" or "critical" (macOS `kern.memorystatus_vm_pressure_level`, Linux
    /// PSI). Absent when the level was judged from swap and free memory, and from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_level: Option<String>,
    /// Where `kernel_level` came from: "macos" (memorystatus) or "linux-psi".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_source: Option<String>,
    /// From which level the guard pauses new sessions and queued deliveries (the setting
    /// `pause_new_sessions_under_pressure`). Absent from older daemons (they paused at
    /// `elevated`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pausing: Option<PausePolicy>,
}

/// When the memory guard pauses new sessions and queued deliveries. `Off` (the default):
/// never: pressure is shown and idle sessions are offered for hibernation, but work goes
/// on (macOS reports "warn" pressure most of the time on a busy Mac). Whatever this says,
/// sustained critical pressure still hibernates an IDLE session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PausePolicy {
    #[default]
    Off,
    /// Only at `critical`.
    Critical,
    /// At `elevated` and `critical` (the guard's behaviour before this setting existed).
    Elevated,
}

impl PausePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            PausePolicy::Off => "off",
            PausePolicy::Critical => "critical",
            PausePolicy::Elevated => "elevated",
        }
    }

    /// "off", "critical" or "elevated" (any case, trimmed); `None` for anything else.
    pub fn parse(s: &str) -> Option<PausePolicy> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(PausePolicy::Off),
            "critical" => Some(PausePolicy::Critical),
            "elevated" => Some(PausePolicy::Elevated),
            _ => None,
        }
    }
}

/// A process left behind by an agent: parent gone (ppid 1), working directory or command
/// line inside Mirante's worktrees. Shown by `mirante doctor`; killed only on request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanView {
    pub pid: i32,
    /// Start time; with the pid it names exactly this process.
    pub start: u64,
    pub cwd: Option<String>,
    pub argv: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub orphan_processes: Vec<OrphanView>,
    /// Checkouts under the worktree root that no task owns (never deleted by Mirante).
    pub orphan_worktrees: Vec<String>,
    pub memory: Option<SystemView>,
    /// The agent binaries the daemon would launch, and where it found them (absent from an
    /// older daemon).
    #[serde(default)]
    pub agents: Vec<AgentBinView>,
}

/// One agent binary as the daemon resolves it: `path` and `source` (`MIRANTE_CLAUDE_BIN`,
/// `PATH`, `well-known directory`), or `error` when none was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBinView {
    pub agent: String,
    pub path: Option<String>,
    pub source: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillOrphan {
    pub pid: i32,
    pub start: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    pub task: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTask {
    /// The repository's checkout; for a task across several repositories (`repos` set), the
    /// folder that holds them (a group).
    pub repo: String,
    /// A workspace task: the checkouts inside `repo` the task may change (absolute paths,
    /// the first is the primary). Each gets a worktree on `branch`; the folder's other
    /// repositories are named to the agent as read-only. Empty: a single-repo task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    pub agent: AgentKind,
    pub branch: String,
    #[serde(default)]
    pub base: Option<String>,
    pub goal: String,
    #[serde(default)]
    pub title: Option<String>,
    /// A child of this fan-out group (its id). An empty `branch` is then named by the
    /// daemon (`<group branch>--<n>-<slug>`) and the task starts from the repo's `HEAD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// Who summarizes a thread on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarizeWith {
    Claude,
}

/// `task.summarize`. Without `with`: the deterministic summary (no model). `with: claude`:
/// the person's own Claude CLI, only when the `summaries` setting is `claude` and the
/// tool-less probe passed for the installed Claude version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summarize {
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<SummarizeWith>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryView {
    pub summary: String,
    pub by: SummaryBy,
    /// What the CLI run reported it cost (`claude` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// Whether threads may be summarized by the person's CLI (never automatically; each run is
/// a click). Codex is not offered: no invocation of it is reliably tool-less (Spike S2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummariesSetting {
    #[default]
    None,
    Claude,
}

impl SummariesSetting {
    pub fn as_str(self) -> &'static str {
        match self {
            SummariesSetting::None => "none",
            SummariesSetting::Claude => "claude",
        }
    }
}

/// `task.create` as the cockpit sends it: a [`CreateTask`], plus, for a thread, what it is
/// bound to. Without `link` it is a plain [`CreateTask`] (older clients send that).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateThread {
    #[serde(flatten)]
    pub task: CreateTask,
    /// A thread: what it is bound to. With `parent`, an empty `branch` is named by the
    /// daemon (`<parent branch>--<n>-<slug>`) from the parent's branch (a workspace parent:
    /// the same repos at `HEAD`), and an empty `repo` is the parent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<ThreadLink>,
    /// A thread's parent: the conversation it came from (a task; a thread's own parent when
    /// it is a thread, one level deep).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// A thread's notes (the person's; redacted, at most 8 KiB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// A thread made from a message of the parent's conversation (moved out of its queue,
    /// or copied).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved: Option<Moved>,
}

/// `group.create`: the shared goal of a fan-out and its children's branch prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateGroup {
    pub kind: GroupKind,
    pub goal: String,
    #[serde(default)]
    pub title: Option<String>,
    /// The children's branches start with it (`<branch>--<n>-<slug>`); no ref of this
    /// name is made.
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRef {
    pub group: String,
}

/// A task's changes at a glance (committed and uncommitted against its base, plus
/// untracked files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DiffStat {
    pub files: u64,
    pub added: u64,
    pub removed: u64,
    pub untracked: u64,
}

/// One task of a group, with what a person reads first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMember {
    pub view: TaskView,
    /// Its newest finished answer (redacted and capped when it was recorded). Agent text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_answer: Option<String>,
    /// `None` once archived (no worktree) or when git could not tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffStat>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupView {
    pub group: TaskGroup,
    /// Not archived first, then by creation.
    pub members: Vec<GroupMember>,
}

/// `group.synthesize`. `into`: one of the group's children (a task reference) receives the
/// request as a queued operator message; absent, a new task in the group (role `synth`,
/// `agent`, the first child's repo or folder) gets it as its goal. `preview`: only the text
/// and its `expect` hash; the call that acts must pass that `expect` back (refused when the
/// results changed meanwhile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Synthesize {
    pub group: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    #[serde(default)]
    pub preview: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Synthesis {
    pub text: String,
    pub expect: String,
    /// The synthesizer made (not started: the caller starts it with `text`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskView>,
    /// The child it was sent to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent: Option<SendResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartSession {
    pub task: String,
    /// First message, delivered as soon as the agent is ready.
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartResult {
    pub task: String,
    pub boot: BootState,
    /// True when the agent's native session / thread was resumed (context restored).
    pub resumed: bool,
    /// Last lines of the agent's screen (Claude) for the trust prompt / exit reason.
    pub screen: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustAnswer {
    pub task: String,
    pub accept: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessage {
    pub task: String,
    pub text: String,
    pub mode: SendMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendResult {
    /// What actually happened: `Queue` when held, `Steer` when handed to the agent now.
    pub delivered: SendMode,
    /// Queue length after this call.
    pub queued: usize,
}

/// A message held in a live session's queue. `id` is stable while it waits (also when a
/// delivery was aborted and handed it back); once it is being delivered it leaves the queue
/// and can no longer be edited, removed or moved (`CONFLICT`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub id: String,
    pub text: String,
}

/// `message.list_queued` / the task half of `message.reorder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRef {
    /// Task id or branch (like every `task` parameter).
    #[serde(alias = "task")]
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditQueued {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedRef {
    pub id: String,
}

/// The queue's new order: exactly the ids queued now, each once. A queue that changed since
/// the client read it (a message delivered or queued meanwhile) is a `CONFLICT`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReorderQueue {
    #[serde(alias = "task")]
    pub task_id: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionView {
    #[serde(flatten)]
    pub request: PermissionRequest,
    pub task_title: String,
    pub branch: String,
    /// The type of Claude's own agent that asked (`general-purpose`, `Explore`…), when a
    /// subagent asked rather than the task's main agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
}

/// `task.subagents` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSubagents {
    pub task: String,
}

/// Whether one of Claude's own agents still runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Running,
    Done,
}

/// One of Claude's own agents in a task's live session, as hooks and its side transcript
/// show it. Read-only: Mirante never steers or stops it. Texts are the agent's (redacted,
/// capped): data, never instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentView {
    /// Claude's `agent_id`.
    pub id: String,
    /// `general-purpose`, `Explore`, a custom agent's name…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// What the main agent asked it to do (the `Agent` call's description).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: SubagentStatus,
    /// When the daemon saw it start / stop (ms since the epoch).
    pub started_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<i64>,
    /// Tool calls it finished.
    pub tools: u32,
    /// Its newest text: the final answer once done, else the latest text in its side file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_text: Option<String>,
    /// Permission requests it raised (ids), oldest first.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// How many of those still wait for an answer.
    #[serde(default)]
    pub pending: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerPermission {
    /// Full id or a unique suffix (at least 4 characters).
    pub id: String,
    pub allow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachParams {
    pub task: String,
    /// The client's terminal size; 0 x 0 keeps the agent's current size (a re-attach).
    /// After the answer the stream starts with a replay for a fresh terminal (scrollback,
    /// then the screen): a client re-attaching resets its terminal first.
    pub rows: u16,
    pub cols: u16,
}

/// After this response the connection carries raw bytes in both directions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachResult {
    /// False for a read-only stream (Codex): typed bytes are ignored.
    pub input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeParams {
    pub task: String,
    pub rows: u16,
    pub cols: u16,
}

/// `events.subscribe` parameters. With `since_seq` and the `boot_id` of the snapshot or
/// event it came from, a reconnecting client resumes: the daemon replays the events after
/// `since_seq` (as `event` notifications) instead of sending a full snapshot, when it
/// still can (same boot, gap not too large). Otherwise a full snapshot is sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookParams {
    pub task: String,
    /// The hook's stdin, verbatim (parsed by the daemon so errors are reported, not lost).
    pub payload: String,
    pub ts_ms: i64,
}

/// What `mirante-hook` actually puts on the wire and in the spool: [`HookParams`] plus
/// optional fields (flattened, so a plain `HookParams` reader ignores them and old spool
/// lines without them still parse).
///
/// The daemon's `hook.event` handling and spool ingest MUST dedupe by `event_id`: a hook
/// that timed out waiting for the ack also spools the event, so the daemon may see it
/// twice. Spool replays can also be older than live events already applied (the spool is
/// ingested at daemon start); order by `received_ts_ms`/`ts_ms`, not by arrival.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEnvelope {
    #[serde(flatten)]
    pub params: HookParams,
    /// `<ts_ms>-<pid>-<counter>`, unique per hook invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// When the hook process received the payload (same clock as `ts_ms`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_ts_ms: Option<i64>,
    /// The hook's stdin was cut at the size cap; `payload` is a stub, not Claude's JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    /// The Mirante session id the hook ran under (`MIRANTE_SESSION_ID` in Claude's env),
    /// so the daemon can fence events from an older session of the same task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// First response of `events.subscribe`; `event` notifications with `seq > seq` follow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub boot_id: String,
    pub seq: u64,
    /// Empty when `resumed`.
    pub tasks: Vec<TaskView>,
    /// The subscription resumed from the requested `since_seq`: `seq` is that value, `tasks`
    /// is empty, and the missed events follow as notifications. The client keeps its state.
    #[serde(default)]
    pub resumed: bool,
}

/// One changed file in a [`TaskDiff`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    pub path: String,
    /// Lines added / removed; `None` for a binary file.
    pub added: Option<u64>,
    pub removed: Option<u64>,
}

/// `task.diff` result: the task branch (committed and uncommitted) against the fork point
/// from its base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDiff {
    /// The base branch (the repo checkout's current branch), or a commit when detached.
    pub base: String,
    pub merge_base: String,
    /// At most 2000 (`files_truncated` when cut).
    pub files: Vec<DiffFile>,
    /// Untracked files (not in `patch`); at most 2000 (`files_truncated` when cut).
    pub untracked: Vec<String>,
    /// `files` or `untracked` was cut.
    #[serde(default)]
    pub files_truncated: bool,
    /// Unified diff, cut at 512 KiB.
    pub patch: String,
    pub truncated: bool,
    /// GitHub's "open a pull request" page, when `origin` is on GitHub.
    pub compare_url: Option<String>,
    /// `origin/<branch>` exists locally and has every commit of the task branch
    /// (`ahead == Some(0)`).
    pub pushed: bool,
    /// Commits on the task branch that `origin/<branch>` lacks; `None` when that ref does
    /// not exist locally (never pushed, or not fetched).
    #[serde(default)]
    pub ahead: Option<u64>,
    /// A workspace task: one section per repository, primary first (the fields above are
    /// then the primary's). Absent for a single-repo task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<RepoDiff>,
}

/// One repository's section of a workspace task's [`TaskDiff`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoDiff {
    /// Its folder name in the workspace.
    pub name: String,
    /// Its main checkout.
    pub repo: String,
    #[serde(flatten)]
    pub diff: TaskDiff,
}

/// Most events one `task.events` call returns.
pub const TASK_EVENTS_MAX: usize = 500;

/// `task.events`: the last `limit` events of a task (default 200, at most
/// [`TASK_EVENTS_MAX`]) with `seq < before_seq` when given, oldest first. Page backwards by
/// passing the first returned `seq` as the next `before_seq`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEventsParams {
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub boot_id: String,
    pub version: String,
    pub live_sessions: usize,
    /// The memory guard's view (absent from older daemons and before the first sample).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<SystemView>,
    /// Mirante types Claude permission answers itself (`MIRANTE_CLAUDE_INLINE_ANSWERS=1`).
    /// Off (the default): Claude cards are answered in the terminal (`mirante attach`).
    #[serde(default)]
    pub claude_inline_answers: bool,
    /// Sessions the daemon ended (restart, crash) are resumed when it starts again
    /// (`daemon.settings`). Absent from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume: Option<bool>,
    /// `MIRANTE_AUTO_RESUME=off` turned auto-resume off for this daemon (the person's
    /// setting then does not apply until it restarts without it). Absent when not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume_forced_off: Option<bool>,
    /// When the memory guard pauses new sessions (`daemon.settings`'s
    /// `pause_new_sessions_under_pressure`). Absent from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_new_sessions_under_pressure: Option<PausePolicy>,
    /// `MIRANTE_MEMGUARD_PAUSE` set it for this daemon. Absent when not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_forced_by_env: Option<bool>,
    /// The daemon's build (`<commit>[-dirty].<unix seconds>`, as `mirante-daemon --version`
    /// prints it): a newer binary on disk has another one. Absent from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Thread summaries by the person's own CLI (`none` | `claude`). Absent from older
    /// daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summaries: Option<SummariesSetting>,
}

/// What `mirante-daemon --version` says: its version, and its build (absent from older
/// binaries).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonBinary {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

/// `repo.files` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoFiles {
    pub repo: String,
    #[serde(default)]
    pub query: String,
    /// Default 30, at most 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// `task.files` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFiles {
    pub task: String,
    #[serde(default)]
    pub query: String,
    /// Default 30, at most 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// `task.file` parameters: `path` is relative to where the agent runs (the worktree, or a
/// workspace task's folder: `<repo>/…`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFile {
    pub task: String,
    pub path: String,
    /// Read a file that may hold secrets (`.env`, keys) anyway: the person clicked Reveal.
    #[serde(default)]
    pub reveal: bool,
    /// Check that it is a file of the worktree and return its absolute path, without its
    /// contents (the shell's "open in editor", a clicked path).
    #[serde(default)]
    pub stat: bool,
}

/// What `task.file` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Text,
    Binary,
    Image,
    TooLarge,
    /// A file that may hold secrets, not read until the person reveals it.
    SecretHidden,
}

/// An image file, for a `data:` URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileImage {
    pub mime: String,
    pub b64: String,
}

/// `task.file`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileView {
    /// As asked (relative).
    pub path: String,
    /// Where it is on disk (checked by the daemon; joined only after a successful read).
    pub abs: String,
    pub size: u64,
    pub kind: FileKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The text was cut at the display cap.
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<FileImage>,
    /// The highlighter's language, from the file name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

/// `daemon.settings` parameters: each one given is changed (and persisted).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_new_sessions_under_pressure: Option<PausePolicy>,
    /// Thread summaries by the person's own CLI (`none` | `claude`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summaries: Option<SummariesSetting>,
}

/// The daemon's settings (`<home>/settings.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonSettings {
    /// Resume the sessions the daemon itself ended (shutdown, crash) when it starts again
    /// (what this daemon does: off while `forced_by_env`).
    pub auto_resume: bool,
    /// `MIRANTE_AUTO_RESUME=off` keeps it off for this daemon; the person's choice is saved
    /// for the next start.
    #[serde(default)]
    pub forced_by_env: bool,
    /// When the memory guard pauses new sessions and queued deliveries (what this daemon
    /// does: the environment's `MIRANTE_MEMGUARD_PAUSE` when set). Off by default.
    #[serde(default)]
    pub pause_new_sessions_under_pressure: PausePolicy,
    /// `MIRANTE_MEMGUARD_PAUSE` decides it for this daemon; the person's choice is saved for
    /// the next start.
    #[serde(default)]
    pub pause_forced_by_env: bool,
    /// Thread summaries by the person's own CLI (`none` by default; each run is still a
    /// click, its cost shown first).
    #[serde(default)]
    pub summaries: SummariesSetting,
}
#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::model::{Lifecycle, Status};
    use serde_json::json;

    /// The pause setting is a word on the wire; an older daemon's settings (no field) read
    /// as off, and an older status says nothing.
    #[test]
    fn the_pause_policy_is_a_word_and_defaults_to_off() {
        assert_eq!(serde_json::to_value(PausePolicy::Critical).unwrap(), json!("critical"));
        let p: SettingsParams =
            serde_json::from_value(json!({ "pause_new_sessions_under_pressure": "elevated" })).unwrap();
        assert_eq!(p.pause_new_sessions_under_pressure, Some(PausePolicy::Elevated));
        assert!(
            serde_json::from_value::<SettingsParams>(json!({ "pause_new_sessions_under_pressure": "always" })).is_err()
        );
        let old: DaemonSettings = serde_json::from_value(json!({ "auto_resume": true })).unwrap();
        assert_eq!((old.pause_new_sessions_under_pressure, old.pause_forced_by_env), (PausePolicy::Off, false));
        assert_eq!(PausePolicy::parse(" Elevated "), Some(PausePolicy::Elevated));
        assert_eq!(PausePolicy::parse("on"), None);
        assert_eq!(PausePolicy::default().as_str(), "off");
    }

    #[test]
    fn task_view_flattens_the_task() {
        let task = Task {
            id: "t1".into(),
            title: "T".into(),
            repo_path: "/r".into(),
            worktree_path: "/w".into(),
            branch: "feat".into(),
            agent: AgentKind::Codex,
            goal: "g".into(),
            notes: String::new(),
            lifecycle: Lifecycle::Active,
            status: Status::NeedsYou,
            created_at: 1,
            updated_at: 2,
            workspace_path: None,
            group_path: None,
            group_id: None,
            group_role: None,
            parent_id: None,
            child_kind: None,
        };
        let v = TaskView {
            task,
            live: true,
            boot: Some(BootState::Ready),
            needs_you: 1,
            queued: 0,
            detail: None,
            usage: None,
            sandbox: Some("net:off".into()),
            memory: Some(MemView { bytes: 1, peak: 2, budget: 3, state: "ok".into() }),
            hibernated: false,
            permission_mode: Some("auto".into()),
            origin: None,
            repos: Vec::new(),
            group: None,
            thread: None,
            orchestration: false,
            orchestrator_live: None,
            spawn_pending: 0,
        };
        let j = serde_json::to_value(&v).unwrap();
        assert!(j.get("repos").is_none() && j.get("workspace_path").is_none(), "single-repo: as before");
        assert_eq!(j["id"], "t1");
        assert_eq!(j["status"], "needs_you");
        assert_eq!(j["boot"], "ready");
        assert_eq!(j["permission_mode"], "auto");
        assert_eq!(serde_json::from_value::<TaskView>(j).unwrap(), v);
    }

    #[test]
    fn a_workspace_diff_has_one_flat_section_per_repo() {
        let one = |base: &str| TaskDiff {
            base: base.into(),
            merge_base: "m".into(),
            files: vec![DiffFile { path: "a".into(), added: Some(1), removed: Some(0) }],
            untracked: vec![],
            files_truncated: false,
            patch: String::new(),
            truncated: false,
            compare_url: None,
            pushed: false,
            ahead: None,
            repos: vec![],
        };
        let mut d = one("main");
        d.repos = vec![RepoDiff { name: "site".into(), repo: "/g/site".into(), diff: one("main") }];
        let j = serde_json::to_value(&d).unwrap();
        assert_eq!(j["repos"][0]["name"], "site");
        assert_eq!(j["repos"][0]["files"][0]["path"], "a", "flattened");
        assert_eq!(serde_json::from_value::<TaskDiff>(j).unwrap(), d);
        let old: TaskDiff =
            serde_json::from_value(json!({"base": "main", "merge_base": "m", "files": [], "untracked": [],
            "patch": "", "truncated": false, "compare_url": null, "pushed": false}))
            .unwrap();
        assert!(old.repos.is_empty());
    }

    #[test]
    fn additive_fields_are_optional_on_the_wire() {
        let s: SubscribeParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(s, SubscribeParams::default());
        assert_eq!(serde_json::to_value(&s).unwrap(), json!({}));
        let snap: Snapshot = serde_json::from_value(json!({"boot_id": "b", "seq": 3, "tasks": []})).unwrap();
        assert!(!snap.resumed);
        let e: HookEnvelope = serde_json::from_value(json!({"task": "t", "payload": "{}", "ts_ms": 1})).unwrap();
        assert_eq!(e.session, None);
        assert!(serde_json::to_value(&e).unwrap().get("session").is_none());
        let r: ResizeParams = serde_json::from_value(json!({"task": "t", "rows": 30, "cols": 100})).unwrap();
        assert_eq!((r.rows, r.cols), (30, 100));
        let d: DaemonStatus =
            serde_json::from_value(json!({"pid": 1, "boot_id": "b", "version": "0.1.0", "live_sessions": 0})).unwrap();
        assert_eq!(d.memory, None);
        // A system view from a daemon that does not say which signal decided its level.
        let v: SystemView = serde_json::from_value(json!({
            "level": "elevated", "total": 1, "free": 1, "swap_used": 0, "swap_total": 0, "paused": true
        }))
        .unwrap();
        assert_eq!((&v.kernel_level, &v.kernel_source), (&None, &None));
        assert!(serde_json::to_value(&v).unwrap().get("kernel_level").is_none());
    }

    #[test]
    fn optional_fields_default() {
        let c: CreateTask =
            serde_json::from_value(json!({"repo": "/r", "agent": "claude", "branch": "b", "goal": "g"})).unwrap();
        assert_eq!((c.base.clone(), c.title.clone(), c.repos.len()), (None, None, 0));
        assert!(serde_json::to_value(&c).unwrap().get("repos").is_none(), "an older daemon sees the same call");
        let w: CreateTask = serde_json::from_value(
            json!({"repo": "/g", "repos": ["/g/a", "/g/b"], "agent": "codex", "branch": "b", "goal": "g"}),
        )
        .unwrap();
        assert_eq!(w.repos, ["/g/a", "/g/b"]);
        let s: StartSession = serde_json::from_value(json!({"task": "t"})).unwrap();
        assert_eq!(s.prompt, None);
        let l: ListParams = serde_json::from_value(json!({})).unwrap();
        assert!(!l.archived);
        let m: SendMessage = serde_json::from_value(json!({"task": "t", "text": "x", "mode": "queue"})).unwrap();
        assert_eq!(m.mode, SendMode::Queue);
        let q: QueueRef = serde_json::from_value(json!({"task_id": "t"})).unwrap();
        assert_eq!(q, serde_json::from_value(json!({"task": "t"})).unwrap());
        let r: ReorderQueue = serde_json::from_value(json!({"task_id": "t", "ids": ["b", "a"]})).unwrap();
        assert_eq!(r.ids, vec!["b".to_string(), "a".to_string()]);
    }
}

/// A repo's memory, named by the repo checkout or by a task (its repo). Agent worktrees are
/// never indexed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaStatus {
    /// The canonical checkout path.
    pub repo: String,
    /// New sessions of this repo's tasks get the memory tools.
    pub enabled: bool,
    /// `.mirante/memory/` exists.
    pub dir: bool,
    pub notes: usize,
    pub stale: usize,
    pub unreviewed: usize,
    pub pending: usize,
    pub invalid: usize,
    pub head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorView {
    pub path: String,
    pub symbol: Option<String>,
    pub start: u32,
    pub end: u32,
    pub commit: String,
    /// `unchecked`, `fresh`, `relocated`, `stale` or `unverifiable`.
    pub state: String,
    pub reason: Option<String>,
    /// Where a relocated anchor's lines are now.
    pub now_start: Option<u32>,
    pub now_end: Option<u32>,
}

/// A note in a list. Its text fields (title, anchor paths, symbols, reasons) are in the
/// caller's view: control and invisible characters always stripped; `<` escaped as `&lt;`
/// only in the agent view (Mirante-launched agents, `dna.task_context { for_agent }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSummary {
    pub slug: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    /// "fresh" | "stale" | "unverifiable" | "unchecked": the worst anchor's state
    /// ("relocated" counts as fresh).
    pub state: String,
    /// A person accepted or confirmed this exact content.
    pub reviewed: bool,
    pub anchors: Vec<AnchorView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteView {
    #[serde(flatten)]
    pub summary: NoteSummary,
    pub body: String,
    pub links: Vec<String>,
    pub backlinks: Vec<String>,
    /// SHA-256 of the file (`dna.note_action` needs it back as `expect_hash`).
    pub content_hash: String,
    pub source: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaSearch {
    #[serde(flatten)]
    pub at: DnaRef,
    pub query: String,
    /// Default 10, at most 25.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaGet {
    #[serde(flatten)]
    pub at: DnaRef,
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphLink {
    pub src: String,
    pub dst: String,
    /// No note named `dst`.
    pub dangling: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaGraph {
    pub nodes: Vec<NoteSummary>,
    pub links: Vec<GraphLink>,
}

/// `dna.check`: what the reindex found.
pub use mirante_core::dna::index::RefreshReport as CheckReport;

/// A proposal as the review queue shows it.
pub use mirante_core::dna::index::Proposal as ProposalView;

/// What `dna.review` answers: the decided proposal (its fields, flattened) and how many
/// secrets were redacted from the text that became the note, the person's edits included
/// (0 on reject).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewedView {
    #[serde(flatten)]
    pub proposal: ProposalView,
    #[serde(default)]
    pub redacted: usize,
}

/// Code a proposed note cites: committed lines of the task's worktree at its HEAD.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposeAnchor {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaPropose {
    pub task: String,
    pub title: String,
    /// `fact`, `decision`, `pattern` or `gotcha`.
    pub kind: String,
    pub body: String,
    pub anchors: Vec<ProposeAnchor>,
}

/// The review queue, oldest first, a page at a time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaProposals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// `pending` (the default), `accepted`, `rejected` or `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Only this task's proposals (a task reference: id, branch or id suffix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Only the proposal with this id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The next page: proposals after this id (the last one of the previous page).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Default 50, at most 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaReview {
    pub id: String,
    pub accept: bool,
    /// The person's edits, used instead of the proposed title / body on accept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Why (kept with the decision).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A note whose cited lines the task branch changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtRisk {
    #[serde(flatten)]
    pub note: NoteSummary,
    /// What changed, per anchor (`path: reason`), joined with `; `.
    pub reason: String,
}

/// `dna.task_context` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaContextParams {
    pub task: String,
    /// The agent view: every `<` in titles, paths, symbols, reasons and proposal text
    /// escaped as `&lt;` (for text handed to an agent). Default: the person view (control
    /// characters stripped, `<` as written), which the cockpit renders as text. A
    /// Mirante-launched agent gets the agent view whatever it asks for.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub for_agent: bool,
}

/// Every text field is in the caller's view (see [`DnaContextParams::for_agent`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaTaskContext {
    /// The task's repo has memory enabled; everything else is empty when not.
    pub enabled: bool,
    /// Notes citing the files the task changed first, then full-text hits on its title
    /// and goal; at most 10.
    pub relevant: Vec<NoteSummary>,
    /// Notes whose cited lines the task branch changed (stale at the branch tip).
    pub at_risk: Vec<AtRisk>,
    /// This task's proposals, oldest first.
    pub proposals: Vec<ProposalView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteAction {
    /// Re-anchor every anchor at the checkout's HEAD and mark this content reviewed.
    Confirm,
    /// Set `status: retired`; the file stays.
    Retire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnaNoteAction {
    pub repo: String,
    pub slug: String,
    pub action: NoteAction,
    /// The `content_hash` the person saw: the action is refused if the file changed since.
    pub expect_hash: String,
}

#[cfg(test)]
mod dna_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dna_params_flatten_the_ref() {
        let s: DnaSearch = serde_json::from_value(json!({"task": "t", "query": "spool"})).unwrap();
        assert_eq!((s.at.task.as_deref(), s.at.repo, s.limit), (Some("t"), None, None));
        let g: DnaGet = serde_json::from_value(json!({"repo": "/r", "slug": "a"})).unwrap();
        assert_eq!(g.at.repo.as_deref(), Some("/r"));
        assert_eq!(serde_json::to_value(DnaRef::default()).unwrap(), json!({}));
    }
}

/// A tenant: the owner part of normalized origins (`github.com/acme`, `dev.azure.com/org`,
/// `local/<hash>`). Mining reads a tenant's tasks only while `learning` is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantView {
    pub tenant: String,
    pub learning: bool,
    /// Tasks whose origin is in this tenant.
    pub tasks: usize,
    /// Skills scoped to this tenant (org or repo); personal skills are not counted.
    pub skills: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillLearn {
    pub tenant: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillListParams {
    /// Only skills that apply in this tenant (personal ones always do).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// `active` or `retired`; both when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// A skill in the library list. `verified`: the current version on disk matches the
/// manifest a person accepted and its signature (`reason` says what failed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    pub name: String,
    pub version: u32,
    /// `personal`, `org:<prefix>` or `repo:<key>`.
    pub scope: String,
    /// `always` or `on_demand`.
    pub activation: String,
    /// `active` or `retired`.
    pub status: String,
    pub description: String,
    /// The current manifest hash (`skill.action` needs it back as `expect_hash`).
    pub manifest_hash: String,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub injected_30d: usize,
    pub used_30d: usize,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillGet {
    pub name: String,
    /// Default: the current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

/// A bundled file of a skill version (text only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFileView {
    pub path: String,
    pub text: String,
}

/// One version of a skill. `manifest`, `skill_md` and `files` are filled only when that
/// version verifies (a changed folder's text is never shown as the skill's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillView {
    #[serde(flatten)]
    pub summary: SkillSummary,
    /// The version shown (the current one unless asked).
    pub shown_version: u32,
    pub manifest: Option<mirante_core::skills::format::Manifest>,
    pub skill_md: String,
    pub files: Vec<SkillFileView>,
    /// Versions on disk, ascending.
    pub versions: Vec<u32>,
    /// E.g. a personal skill learned in a client's tasks.
    pub warnings: Vec<String>,
    /// The version folder on disk: a person re-adopts a changed one with `skill.add {dir}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillConfirmOrigin {
    /// A task of the checkout (id, branch or id suffix).
    pub task: String,
    /// The origin key the person confirms: the checkout's pinned one or its live one.
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRepoParams {
    /// A path in the checkout (canonicalized by the caller).
    pub repo: String,
}

/// A checkout's origins: the one Mirante pinned (at its first person-created task, or a
/// person's confirmation) and the live `remote.origin.url`, both normalized (no token, no
/// ssh alias).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoOrigin {
    pub live: String,
    pub live_tenant: String,
    /// `None` until a person creates a task in this checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_tenant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginConfirmed {
    pub origin: String,
    pub tenant: String,
    /// Tasks of the checkout fixed to this origin that get skills again.
    pub cleared: usize,
    /// When the confirmed origin is not the checkout's live `remote.origin.url`: new tasks
    /// will wait for a confirmation again until the remote is fixed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// A lint finding (`rule`, `severity`, `file`, `line`, `message`).
pub use mirante_core::skills::lint::Finding;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillLintParams {
    pub name: String,
    pub description: String,
    pub body: String,
    /// `personal`, `org:<prefix>` or `repo:<key>`.
    pub scope: String,
    /// The proposal being reviewed: its bundled files are linted and their secrets counted
    /// with the draft (default: none, a draft without files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPropose {
    pub task: String,
    /// A new skill's name (default: from the description).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The existing skill this edits (its current version is the base).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub description: String,
    pub body: String,
    /// `always` (default) or `on_demand`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInline {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// `dir` (a folder with `SKILL.md` and text files; symlinks and executables refused) or
/// `inline` text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillAdd {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<SkillInline>,
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<String>,
}

/// A skill version to review again (see [`method::SKILL_REREVIEW`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillRereview {
    pub name: String,
    pub version: u32,
}

/// See [`method::SKILL_PENDING`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPending {
    pub pending: usize,
}

/// The review queue, oldest first, a page at a time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillProposals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// A task reference: only its proposals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `pending` (the default), `accepted`, `rejected` or `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Default 50, at most 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// A proposal (its fields, flattened) and what the review card must say about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillProposalView {
    #[serde(flatten)]
    pub proposal: mirante_core::skills::index::SkillProposal,
    /// E.g. a personal scope for text learned in a client's tasks.
    pub warnings: Vec<String>,
    /// For an edit in the review queue: the version it was made against, as a person may
    /// trust it (its text only when it verifies). What the card diffs against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<SkillBase>,
}

/// The version an edit proposal changes. `description`, `activation`, `body` and `files`
/// are filled only when `verified`: a folder changed outside Mirante is never a diff base.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillBase {
    /// `None` when no reviewed version has the proposal's base hash.
    pub version: Option<u32>,
    pub verified: bool,
    /// Why it does not verify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The skill's current version is another one (an accept would be refused).
    pub moved: bool,
    pub description: String,
    pub activation: String,
    pub body: String,
    pub files: Vec<SkillFileView>,
}

/// A person's edits, used instead of the proposed values on accept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillEdits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillReview {
    pub id: String,
    pub accept: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<SkillEdits>,
    /// Why (kept with the decision).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillActionKind {
    /// Stop injecting it (the versions stay).
    Retire,
    Restore,
    /// A new version with `scope`.
    Rescope,
    /// A new version with the content of `version`.
    Rollback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillAction {
    pub name: String,
    pub action: SkillActionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// The `manifest_hash` the person saw: refused if the skill moved on since.
    pub expect_hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMine {
    pub tenant: String,
}

/// What a mining run did: new proposals, pending ones whose evidence it updated, and
/// clusters it skipped (covered by an active skill, rejected before, or over a cap).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillMined {
    pub tenant: String,
    pub proposed: usize,
    pub updated: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillTaskParams {
    pub task: String,
}

/// A skill a task's session gets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSkill {
    pub name: String,
    pub version: u32,
    pub activation: String,
    pub scope: String,
}

/// A skill a task's session does not get, and why (modified on disk, over budget, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusedSkill {
    pub name: String,
    pub reason: String,
}

/// What the task's next session would get, resolved now (nothing is recorded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSkills {
    pub tenant: String,
    pub origin: String,
    pub learning: bool,
    /// False while the task's origin waits for a person's confirmation (no skills).
    pub confirmed: bool,
    /// While unconfirmed: the checkout's live origin as the task saw it (`origin` is the
    /// pinned one). `skill.confirm_origin` takes either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    pub active: Vec<TaskSkill>,
    pub refused: Vec<RefusedSkill>,
    /// Why no skill applies at all (no origin, unconfirmed origin, no ssh-keygen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// `rules.layers` parameters: a checkout (`repo`) or a task (its worktree, plus what
/// Mirante adds to its session), and the agent whose layers to list (default: the task's,
/// else `claude`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
}

/// A file pulled in by an `@path` import (Claude), with its own imports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleImport {
    /// As written after the `@`.
    pub spec: String,
    /// Where it resolved (`~` for the home directory).
    pub path: String,
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Text (redacted, capped); absent when missing or refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    /// Why it is not shown (outside the allowed places, a cycle, too deep, may hold secrets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<RuleImport>,
}

/// One place an agent session takes instructions, settings, skills, memory or a mode from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleLayer {
    /// `managed`, `user`, `project`, `local`, `nested` or `mirante`.
    pub scope: String,
    /// `claude` or `codex`.
    pub agent: String,
    /// `instructions`, `settings`, `skills`, `memory` or `mode`.
    pub kind: String,
    /// Display path (`~` for the home directory); a description for a Mirante layer.
    pub path: String,
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Last modified, Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    /// Text as the agent reads it (redacted, capped); a settings file's relevant keys; a
    /// skills folder's `name: description` lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<RuleImport>,
    /// When and how it applies (load order, on demand, what wins).
    pub note: String,
    /// Read at launch (false: on demand, shadowed, or only when a setting picks it).
    #[serde(default)]
    pub loaded: bool,
    /// Skills listed in a skills layer.
    #[serde(default)]
    pub skills: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What a session reads at launch, counted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesSummary {
    pub instruction_files: usize,
    pub instruction_bytes: u64,
    pub settings_files: usize,
    pub skills: usize,
}

/// `rules.layers` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesView {
    pub agent: String,
    /// The project root the agent starts in (the checkout, or the task's worktree).
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The agent's version as researched for these rules (e.g. `2.1.285`).
    pub researched: String,
    pub layers: Vec<RuleLayer>,
    pub summary: RulesSummary,
}
