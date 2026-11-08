# Cockpit prototype (Layer 0) — design

- **Date:** 2026-11-07
- **Status:** Approved by the founder in brainstorming. Not yet planned.
- **Living context doc:** "ProjectDNA Local — pivô e design" (Claude Docs). It covers the pivot, the market and code studies, and the adversarial review.

## 1. Why this exists

ProjectDNA is pivoting from a cloud SaaS to a **local-first developer platform**:

- **Core:** a cockpit to dispatch coding agents, control many contexts and see everything visually, at or above the level of T3 Code, cmux and GrokBot.
- **Differentiator (later layers):** cited repo memory and evolving skills.
- **Paid tier (later):** an internal skill registry for companies.

The founder decided the platform is the core.

The Layer 0 prototype answers one question:

> **Do the founder and at least 3 other developers replace their current setup (cmux, T3 Code, plain terminals) with this cockpit in their daily work?**

## 2. Scope

### In

1. **Dispatch.** "New task" → pick an agent (Claude Code or Codex) → create a git worktree. The branch name is chosen by the user; there is no forced prefix and no assumed `main`.
2. **See every task.** A task list with a status for each task: `working`, `needs_you`, `idle`, `ended`. Status comes **only** from hooks and protocol events, never from scraping the terminal. The app notifies the user only when a task needs them, and suppresses the notification when that task is already on screen.
3. **Open a task.** A live terminal, a step feed (tool calls, commands, files touched) and a diff per turn.
4. **Talk mid-run.** Two message modes:
   - **Steer now:** delivered at the next step boundary.
   - **Queue:** delivered when the turn ends. The queue is editable and reorderable.

   When an agent can't be steered, the UI shows it and falls back to cancel + re-prompt.
5. **Permissions.** Agent permission requests appear as persisted cards that the user answers once. Nothing is ever auto-approved, and a request survives an app restart.
6. **Context that survives.** Each task stores its goal, notes, history and native session id on disk. Closing and reopening the app restores everything and resumes the agent's native session.

### Out (explicitly)

- Memory, skills, the board between agents, triggers and bots.
- Agents other than Claude Code and Codex.
- Windows (the prototype targets macOS and Linux).
- Any cloud component, account or login.

## 3. Risks first — week 1 spikes

If a spike fails, the design changes before the rest is built.

| Spike | Question | Paths to test | Pass criterion |
|---|---|---|---|
| S1 Mid-run messaging | Can each agent be steered without waiting for the turn to end? | **Claude:** interactive CLI in a PTY with hooks, **vs** `claude -p --input-format stream-json --output-format stream-json`. **Codex:** `codex app-server` JSON-RPC with `turn/steer` / `turn/interrupt` | Steer works on at least one path per agent, or the fallback is defined |
| S2 Quota | Does the chosen mode burn more of the user's plan than normal interactive use? This is T3 Code's top pain: Agent SDK traffic billed differently (t3code #7338) | Run the same 3 tasks in each mode and compare the usage shown by each CLI | The chosen mode costs no more than interactive use, or the difference is documented and accepted |
| S3 Status | Do hooks/events give a deterministic `working / needs_you / idle / ended`? | Claude Code hooks (SessionStart, UserPromptSubmit, Pre/PostToolUse, Notification, Stop, SessionEnd); Codex app-server events | Zero wrong states in 50 hand-checked transitions |
| S4 Resume | Does reopening resume the native session? | `claude --resume <id>` and the Codex resume path, using the session id captured by the hook | A killed app comes back with the session intact |

Check the Claude Code / Codex terms of use before relying on headless or automated modes.

## 4. Architecture

Patterns come from the clean-room study. They are reimplemented from prose specs, with no third-party code.

```
Tauri app (web UI) ──┐
                     ├── Unix socket (0600) ──► cockpitd (Rust daemon) ──► SQLite (WAL)
CLI (cockpit …) ─────┘                              │
                                                    ├── driver: claude-code (PTY+hooks | stream-json)
                                                    ├── driver: codex (app-server JSON-RPC)
                                                    └── git: worktrees + hidden checkpoint refs
```

- **One owner of state.** `cockpitd` owns all state. The Tauri app and the CLI are thin clients over a typed RPC on a Unix domain socket with mode `0600`. There is no TCP port, no CORS and no remote access.
- **Driver interface.** Every agent sits behind one trait:
  - operations `start`, `send(mode: steer|queue)`, `interrupt`, `respond_permission`, `resume`, `capabilities()`;
  - a normalized event stream: `status_changed`, `step`, `output_delta`, `permission_requested`, `turn_ended`, `session_ended`.

  An unsupported capability is a typed result, never a silent failure.
- **Durable model.** The minimal set of concepts is **Task → Conversation (durable) → Session (process activation) → Turn → PermissionRequest → Checkpoint**. Events carry a monotonic `seq` plus a `boot_id`. Clients reconcile from snapshots; the stream is only a hint. State is persisted on every transition, not only at shutdown.
- **Checkpoints.** Each turn ends with a commit to a hidden git ref (`refs/cockpit/<task>/<turn>`). This gives a diff per turn and revert without polluting the user's branch.
- **Hooks install.** Hooks are installed per agent, scoped by an env var (`COCKPIT_TASK_ID`), so they do nothing outside the cockpit. There is no global behavior change, and install and uninstall are explicit.
- **Worktrees.** They live under `~/.cockpit/worktrees/<repo>/<branch>`. Optional repo scripts `setup` / `teardown` are declared in a repo-level config file. The worktree is cleaned up on task archive, with the branch preserved.

## 5. Security baseline

- **Daemon socket.** Mode `0600` in `~/.cockpit/`. The daemon checks the peer uid.
- **Agent permissions.** Never auto-approve and never use skip-permissions flags. Permission answers are scoped `once` by default.
- **No secrets in the database.** Logs keep a bounded scrollback and redact token patterns.
- **Untrusted text.** Agent output and repo text are treated as untrusted. Mid-run messages from the UI are labelled as coming from the human operator: a person's message (steered or queued) reaches the agent as `[Mirante · message from the human operator] <text>` on one line. Slash commands, a task's first prompt and messages sent by another agent are not labelled, and an agent's message that carries the label is refused. Events, the queue and the feed keep the person's own words.

## 6. Success metrics (2 weeks of real use after the build)

| Metric | Target |
|---|---|
| Founder uses it instead of cmux | 2 consecutive weeks |
| External devs (founder recruits 3+) | ≥ 3 keep using it after 1 week |
| Context recreated by hand after closing and reopening | 0 |
| Wrong status | 0 in 50 hand-checked transitions |
| Mid-run steering | Works on both agents, or the fallback is explicit in the UI |

## 7. Estimate

About 5–6 weeks part-time: spikes 1, daemon and two drivers 2, UI 1–2, persistence and polish 1. All new Rust/TypeScript code. Nothing from the old NestJS API is used in this layer.

## 8. Carried over from agent-plataform (founder's Mar–Apr 2026 project)

That project was a cloud "autonomous engineer": a Jira webhook started a hand-written LLM pipeline that produced a GitHub PR. It ran end to end against a real repo, and it stalled for two reasons: about 40% of its commits fought cloud infra, and hand-orchestrating raw LLM calls (JSON parsing, patches, token limits) was fragile. Mirante avoids both: it delegates execution to Claude Code or Codex, and it runs as a local daemon.

**Bring as concepts (no code ported):**

- **`NEEDS_HUMAN` as a first-class exit.** Several guards can raise it: cost, protected paths, failed checks. It maps to `needs_you` plus a notification.
- **Audit trail of transitions.** Use the `runs / run_steps / run_events (from→to) / run_artifacts` shape, adapted to SQLite as Task / Turn / Event / Artifact. Add the transition validation the old code lacked; the trail also lets us verify "0 wrong status in 50 transitions".
- **Per-task lock.** Never run two sessions on the same task or worktree. In a local daemon this needs no Redis.
- **In-run cost/token ceiling.** Check it while the task runs, from the usage the hooks and CLIs report. Exceeding it means `needs_you`.
- **Task artifacts as persistent context.** Keep the plan, notes and review summary per task in a local directory.
- **Protected paths.** Turn them into suggested permission rules, never auto-decisions. Match on repo-relative paths: the old guard compared against `'/'`-prefixed paths, so it likely never blocked anything.

**Do not repeat:**

- States that do nothing (`HUMAN_QA`).
- Failures that never reach `FAILED`, and locks left orphaned until TTL.
- Parse errors that silently return empty.
- Webhook guards that fail open when the secret is missing.
- Untrusted ticket/PR text sent raw to the LLM.
- Compiled `.d.ts` files committed and drifting from the source.
- Policies with no end-to-end test.

**Discard:**

- the LLM pipeline and its prompts;
- the "MCP servers" (they were REST, unauthenticated and unused);
- `panel-web` (an empty scaffold);
- all AWS infra (SQS, S3, Redis, Terraform).

**Later layers:** the Jira client (ADF flattening, status transitions by alias) is worth porting when Layer 3 triggers arrive. Fix it first: URL-encode parameters, wrap ticket text as untrusted, check `res.ok`.

## 9. Open items (non-blocking for spikes)

- **Repo location.** Recommended: a new repository for the cockpit, for clean licensing and history. This spec lives here for continuity.
- **Product license.** Apache-2.0 is suggested. Add `LICENSE` and `THIRD_PARTY_NOTICES` before any public code.
- **Clean-room protocol.** Implementers work only from the prose pattern reports, never from third-party source. A log of what was read exists (2026-11-07).
