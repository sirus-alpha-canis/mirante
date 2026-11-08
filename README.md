# Mirante

Mirante is a local-first cockpit for dispatching and supervising coding agents. It runs
Claude Code and Codex side by side, each task in its own git worktree, and more agents (via
ACP) are planned. Task status comes from the agents' hooks and protocol events, never from
scraping the screen. You can steer an agent mid-run or queue a message for the end of its
turn. Permission requests become cards that a person answers, and nothing is auto-approved.
Sessions resume on their native conversation after a stop or a restart, and a memory guard
keeps a handful of agents from taking the machine down. Two optional layers sit on top:
cited repo memory ("DNA"), and reviewed, signed skills scoped per client. Everything runs on
your machine. There is no cloud component, no account and no telemetry.

## Status

Early prototype. Expect rough edges and breaking changes.

- **macOS first.** Linux is partially supported: the daemon has Linux code paths (process
  trees, memory sampling), but the app and the full flow are only exercised on macOS.
  Windows is not supported.
- **The app is unsigned** and not notarized. You build it yourself.
- **Some UI labels are in Portuguese** for now (for example "Precisa de você", meaning
  "needs you").
- **Supported agents:** Claude Code and Codex only.

> Screenshots: none yet. They will be added to this section.

## Requirements

- macOS (Linux: partial, see above) with the Xcode command line tools and `git` 2.31 or newer.
- Rust stable, installed through [rustup](https://rustup.rs). Some dependencies use
  edition 2024, so an older Homebrew `cargo` (1.81, for example) fails to build them.
  If both are installed, put rustup first:
  `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`.
- Node 22.12 or newer and pnpm 10, for the desktop app.
- Claude Code and/or the Codex CLI, installed and logged in. Mirante runs them unmodified,
  under your own logins.
- For skills: `/usr/bin/ssh-keygen` with `-Y sign|verify` support (OpenSSH 8.2 or newer).

## Install

### 1. CLI, daemon, hook and MCP server

```bash
git clone <this repo> mirante && cd mirante
cargo build --release --workspace
mkdir -p ~/.local/bin
ln -sf "$PWD"/target/release/{mirante,mirante-daemon,mirante-hook,mirante-mcp} ~/.local/bin/
```

The four binaries must come from the same build. They find each other when they sit in
the same directory: the CLI starts the `mirante-daemon` next to it, and the daemon uses the
`mirante-hook` and `mirante-mcp` next to it. `~/.local/bin` must be on your `PATH`.

The daemon finds the agents in this order: `MIRANTE_CLAUDE_BIN` / `MIRANTE_CODEX_BIN`, then
`PATH`, then `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin` and nvm's
newest Node that has the agent. If `claude` or `codex` on your `PATH` is a wrapper or shim,
point at the real binaries in your shell profile:

```bash
export MIRANTE_CLAUDE_BIN=/opt/homebrew/bin/claude
export MIRANTE_CODEX_BIN=~/.nvm/versions/node/<version>/bin/codex
```

Then open a new shell, so the daemon inherits them:

```bash
mirante daemon start
mirante doctor          # which agent binaries the daemon resolved, and from where
```

### 2. The desktop app

```bash
pnpm --dir apps/cockpit install
pnpm --dir apps/cockpit tauri build --bundles app
open apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app
```

The app is a thin client of the daemon. It looks for `mirante-daemon` in `~/.cargo/bin`,
`~/.local/bin`, `/opt/homebrew/bin` and `/usr/local/bin`, then on your login shell's `PATH`
(or at `$MIRANTE_DAEMON_BIN`). If no daemon is running, its banner offers **Start daemon**.

An app opened from Finder or the Dock gets macOS's minimal `PATH`. So before it starts a
daemon, the app asks your login shell for its environment, once per run with a 3-second
limit. It passes on only `PATH` and the `MIRANTE_*` variables, and never tokens or keys. If
your shell is slow to start, run `mirante daemon restart` from a terminal instead.

A copied bundle is quarantined on another Mac. For a build you made yourself, clear the
quarantine with `xattr -dr com.apple.quarantine Mirante.app`.

More detail, including the fake daemon for UI work without agents:
[`apps/cockpit/README.md`](apps/cockpit/README.md).

## Use

### The app

- **Projects.** Add a repository on the left with **Choose…** (the folder picker), or paste
  its path.
- **Dispatch.** Home has one form. Write the goal, then pick the agent, the repository and
  the branch with the chips below it. The branch defaults to `mirante/<words-of-the-goal>`,
  and you can edit it. **+ contexto** attaches repo files, memory notes or skills. The
  one-line form `@claude shop/feat/login add a login page` works too. ⌘↵ dispatches.
- **Home** is an inbox across all projects: what needs you first, then what is ready to
  review, then what is working. **⌘J** jumps to the next task that needs you.
- **Detail.** Each task has four tabs. **Steps** shows tool steps summed up. **Terminal** is
  the live terminal. **Diff** shows the task's changes. **Memory** shows the repo notes
  relevant to the task, the notes its branch puts at risk, and what it proposed. The
  composer steers now (⌘↵) or queues for the turn end (⌥↵).
- **Grid (⌘G)** shows every agent at once.
- **Brain (⌘B)** is the repo memory graph and its review queue.
- **Skills** lists the skill library and its proposals.
- **⌘K** opens the command bar: tasks, projects, and `@agent repo/branch goal`.

Closing the app never ends a session, because the daemon owns them. Off macOS, ⌘ is Ctrl.

### The CLI

Every core app action has a CLI equivalent. Run `mirante` with no arguments for the full
usage. A `<task>` is an id, a unique id suffix (4+ characters) or the branch name.

```bash
mirante new . --agent claude --branch feat/login "add a login page"
mirante ls                      # tasks, status, what needs you
mirante watch                   # live board; rings the bell when a task needs you
mirante attach feat/login       # live terminal (Claude) or event stream (Codex); Ctrl-] detaches
mirante say feat/login "use the existing session helper"          # steer now
mirante say feat/login --queue "then add tests"                   # after the current turn
mirante queue ls feat/login     # also: queue edit | rm | mv
mirante perms                   # pending permission requests
mirante allow <perm>            # or: mirante deny <perm>  (always once, never "always")
mirante interrupt feat/login    # stop the current step
mirante stop feat/login         # end the agent process (resumable)
mirante resume feat/login       # back on its native conversation (--fresh: a new one)
mirante hibernate feat/login    # free memory from an idle agent; `mirante wake` resumes it
mirante archive feat/login      # remove the worktree, keep the branch
mirante doctor                  # orphan processes and checkouts, memory, agent binaries
mirante daemon start|stop|restart|status
```

## How it behaves

**Permissions.** Nothing is ever auto-approved, and Mirante never uses skip-permissions
flags. A request is persisted, survives an app restart, and is answered once.
- **Codex** requests arrive over its app-server protocol and show as **Allow / Deny** cards
  (`mirante allow|deny`).
- **Claude Code** dialogs are answered **in the task's terminal** by default. The card offers
  **Answer in terminal**, and `mirante allow` offers to attach. Screen text cannot prove
  which request a dialog belongs to. The daemon answers a Claude dialog itself only when it
  runs with `MIRANTE_CLAUDE_INLINE_ANSWERS=1` and has transcript evidence for that request.
- A Claude session that leaves manual permission mode gets a red **auto mode on** badge.

**Memory guard.** The daemon samples each session's process tree and the machine's memory
pressure. It sets no hard limits. A session near its budget gets a warning. Under pressure,
new dispatches and queued deliveries pause. Under sustained critical pressure, the largest
*idle* session is hibernated (stopped, resumable), at most one per minute. A working session,
or one waiting on a permission, is never hibernated. Leftover processes are reported, and
killing one is your call (`mirante doctor --kill <pid>` asks first). Stress test:
[`docs/dogfood/memory-stress.md`](docs/dogfood/memory-stress.md).

**Auto-resume.** When the daemon restarts (an upgrade, a reboot, a crash), it resumes the
sessions it ended on their native conversations, one at a time while memory allows. Stopped
and hibernated tasks are never resumed this way. To turn it off, use the cockpit's setting
or `MIRANTE_AUTO_RESUME=off`.

## Repo memory (DNA)

This feature is opt-in per repo: `mirante memory enable`. Notes are Markdown files with
`[[links]]` in `<repo>/.mirante/memory/`. They open as an Obsidian vault, and you commit
them like code. Each note cites concrete code (path, symbol, line range, commit) and is
flagged **stale** when that code changes. Agents read the notes, and can propose new ones,
through the `mirante-mcp` server. A person reviews every proposal (`mirante memory
proposals | accept | reject`, or the Brain view). `mirante memory check --offline` rechecks
every anchor without a daemon, for use in CI. Details:
[`docs/plans/2026-11-07-plan-4-dna-repo-memory.md`](docs/plans/2026-11-07-plan-4-dna-repo-memory.md),
[`docs/dogfood/memory.md`](docs/dogfood/memory.md).

## Skills

A library of reusable instructions. Each new Claude Code or Codex session gets the skills
whose scope (`personal`, `org:…` or `repo:…`) matches the task repo's normalized git origin,
in the agent's native form. Mirante never edits `~/.claude` or `~/.codex`. With
`mirante skills learn on` (opt-in, on this machine only), Mirante finds instructions you
repeated across tasks and proposes skills. Agents can propose skills through MCP. A person
reviews every proposal. Each accepted version is linted, stored with a content-hash
manifest, and signed with a local ed25519 key (`ssh-keygen -Y`). The signature is checked
again before each injection. Details:
[`docs/plans/2026-11-08-plan-5-evolving-skills.md`](docs/plans/2026-11-08-plan-5-evolving-skills.md),
[`docs/dogfood/skills.md`](docs/dogfood/skills.md).

## Architecture

```
 Mirante.app (Tauri + React) ──► mirante-bridge ──┐
                                                  │  JSON-RPC over a Unix socket
 mirante (CLI) ───────────────────────────────────┤  ~/.mirante/daemon.sock (0600, peer uid)
                                                  │
 mirante-hook (Claude hooks) ─────────────────────┤
 mirante-mcp (memory / skills MCP) ───────────────┘
                                                  ▼
                                          mirante-daemon ──► SQLite (one owner of state)
                                           │
                                           ├─ driver: Claude Code  (interactive CLI in a PTY;
                                           │                        status from hooks + transcript)
                                           ├─ driver: Codex        (codex app-server, JSON-RPC stdio)
                                           ├─ git: worktrees, task diffs (hardened invocations)
                                           └─ memory guard, DNA index, skills library
```

| Crate / app | What it is |
|---|---|
| `crates/mirante-core` | Pure library: domain model, SQLite store, status engine, steer/queue rules, transcript tail, worktrees, git hardening, DNA and skills logic. |
| `crates/mirante-ipc` | Wire types, JSON-RPC client and socket paths shared by every binary. |
| `crates/mirante-daemon` | The daemon: sessions, agent drivers, permission handling, memory guard, access control. |
| `crates/mirante-cli` | The `mirante` command. |
| `crates/mirante-hook` | The Claude Code hook command, which reports hook events to the daemon. |
| `crates/mirante-mcp` | The MCP server that agents use for repo memory and skill proposals. |
| `crates/mirante-bridge` | The app's daemon client (allowlisted RPC, resumable event stream, attach), plus a scripted fake daemon. |
| `crates/spike-kit` | Throwaway risk spikes. Not part of the product. |
| `apps/cockpit` | The desktop app (Tauri 2 + React). |

Design: [`docs/spec/2026-11-07-cockpit-prototype-design.md`](docs/spec/2026-11-07-cockpit-prototype-design.md).
Plans: [`docs/plans/`](docs/plans/). Spike findings: [`docs/spikes/`](docs/spikes/).

## Security model

- **Local socket only.** No TCP port and no remote access. The socket is mode `0600`, and
  the daemon checks the peer's uid.
- **Per-identity method allowlist.** The daemon tells a person (the app, the CLI) from a
  Mirante-launched agent's process tree. Agents may only report their own hook events,
  read and propose memory for their own task, propose skills, and read daemon status. They
  cannot create or steer tasks, answer permissions, or accept proposals: Mirante-launched
  agents are refused. A caller that cannot be identified gets status only. Methods are
  person-only unless they are listed (fail closed).
- **Agents can't approve themselves.** Nothing is auto-approved. Memory and skill decisions
  also require typing the verb on the terminal. That is an intent check, not a boundary.
  The boundary is the daemon refusing agents.
- **Hardened git.** Agents write the worktrees, and the daemon runs git in them as you.
  Every git call overrides config that could execute code (fsmonitor, hooks, filters, diff
  drivers, pager, ssh and credential helpers), clears the redirecting `GIT_*` variables, and
  ignores replace refs and grafts.
- **Untrusted text is labeled.** Memory notes and attached context handed to an agent are
  wrapped and labeled as untrusted data, and they cannot close their own wrapper.
- **Best-effort redaction.** Credential patterns are masked in step summaries and in
  agent-written proposals. This lowers what leaks; it is no guarantee.

**Honest limits (same uid).** Agents run as your user. A prompt-injected agent with shell
access can read anything you can, including the skills signing key, and it can reach the
socket. The identity check stops it from acting *as a person through the daemon*, and the
signature catches edits made outside Mirante. Neither is a sandbox against a process
running as you. Use the agents' own sandboxes and permission modes for that.

## Development

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"   # if Homebrew cargo shadows rustup
cargo build --workspace            # the e2e tests use the built binaries
cargo test --workspace             # unit + e2e (fake claude, temp dirs)
cargo clippy --workspace --all-targets
pnpm --dir apps/cockpit test       # Vitest
pnpm --dir apps/cockpit typecheck
```

The tests against the real agents are ignored by default. They need your logins and spend
plan quota; see the header of `crates/mirante-daemon/tests/real_agents.rs`.

App smoke test (opens the window for a few seconds, against the fake daemon):

```bash
cargo build -p mirante-bridge --features fake
pnpm --dir apps/cockpit tauri build --debug --bundles app
apps/cockpit/scripts/smoke.sh
```

Manual checklists for real use live in [`docs/dogfood/`](docs/dogfood/): `cockpit.md`,
`memory.md`, `skills.md`, `memory-stress.md`. For experiments, set a short
`MIRANTE_HOME=/tmp/mh` so you don't touch your real `~/.mirante`. Socket paths are limited
to about 104 bytes.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). In short: clean-room only (never copy code from
other projects), test first, one change per PR, and all the checks above must pass.

## License

Apache License 2.0. See [`LICENSE`](LICENSE) and
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
