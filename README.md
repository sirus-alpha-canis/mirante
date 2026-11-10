# Mirante

[![CI](https://github.com/sirus-alpha-canis/mirante/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/sirus-alpha-canis/mirante/actions/workflows/ci.yml)

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
- **The CLI, daemon, hook and MCP server** ship as prebuilt macOS binaries (Apple silicon
  and Intel) through Homebrew and [GitHub Releases](https://github.com/sirus-alpha-canis/mirante/releases).
- **The desktop app is source-only** until signed releases: it is unsigned and not
  notarized, so you build it yourself.
- **Some UI labels are in Portuguese** for now (for example "Precisa de você", meaning
  "needs you").
- **Supported agents:** Claude Code and Codex only.

> Screenshots: none yet. They will be added to this section.

## Quickstart (5 minutes)

You need macOS with `git` (the Xcode command line tools: `xcode-select --install`), and
Claude Code and/or the Codex CLI installed and logged in. Then:

```bash
brew install sirus-alpha-canis/tap/mirante
mirante daemon start
mirante doctor          # which agent binaries the daemon found, memory, orphans

cd ~/code/your-repo     # any git repository with at least one commit
mirante new . --agent claude "add a health-check endpoint"   # or --agent codex
mirante ls              # status, and what needs you
mirante attach mirante/add-a-health-check-endpoint          # the live terminal; Ctrl-] detaches
```

The first start of freshly installed binaries can take about 20 seconds while macOS scans
them. From source instead (needs [rustup](https://rustup.rs)):

```bash
git clone https://github.com/sirus-alpha-canis/mirante.git && cd mirante
scripts/install.sh      # builds, links into ~/.local/bin, starts the daemon, runs `mirante doctor`
scripts/try.sh          # optional: a one-minute tour with a fake agent (no login, no quota)
```

The first build takes a few minutes. `install.sh` is safe to run again after a `git pull`:
it rebuilds, relinks and restarts an idle daemon. If `~/.local/bin` is not on your `PATH`,
it says what to add to your shell profile.

The desktop app is built from source: `scripts/install.sh --app` builds it (also needs
Node 22.12+ and pnpm 10; add `--install-app` to copy it to `/Applications`), then
`mirante app` opens it, starting the daemon first if needed. The daemon itself has no window: `mirante daemon start` alone opens
nothing. `mirante help` lists every command, `mirante <command> --help` explains one.

## Requirements

- macOS (Linux: partial, see above) with the Xcode command line tools and `git` 2.31 or newer.
- Homebrew, for the prebuilt CLI (macOS 12 or newer, Apple silicon or Intel). The rest of
  this list is for building from source.
- Rust through [rustup](https://rustup.rs), at the version pinned in `rust-toolchain.toml`
  (1.99.1, with clippy and rustfmt; `rustup toolchain install` in the repo fetches it). Some
  dependencies use edition 2024, so an older Homebrew `cargo` (1.81, for example) fails to build them.
  If both are installed, put rustup first:
  `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`.
- Node 22.12 or newer and pnpm 10, for the desktop app.
- Claude Code and/or the Codex CLI, installed and logged in. Mirante runs them unmodified,
  under your own logins.
- For skills: `/usr/bin/ssh-keygen` with `-Y sign|verify` support (OpenSSH 8.2 or newer).

## Install

### 1. CLI, daemon, hook and MCP server

#### With Homebrew (prebuilt, macOS)

```bash
brew install sirus-alpha-canis/tap/mirante
mirante daemon start
mirante doctor
```

This installs `mirante`, `mirante-daemon`, `mirante-hook` and `mirante-mcp` together into
Homebrew's prefix (`/opt/homebrew/bin` on Apple silicon, `/usr/local/bin` on Intel). Upgrade
with `brew upgrade sirus-alpha-canis/tap/mirante` (a plain `brew upgrade` includes it once
you `brew trust --formula sirus-alpha-canis/tap/mirante`), then `mirante daemon restart`
once no task is running. Use
either Homebrew or `install.sh`, not both: if you used `install.sh` before, run
`scripts/uninstall.sh` (it keeps `~/.mirante`) so only one copy is on your `PATH`. The
tarballs and their `SHA256SUMS` are also on the
[releases page](https://github.com/sirus-alpha-canis/mirante/releases) (keep the four
binaries in one directory). How releases are made: [RELEASING.md](RELEASING.md).

#### From source

`scripts/install.sh` does all of this (`--prefix <dir>` links somewhere other than
`~/.local/bin`; `--no-start` leaves the daemon alone). By hand:

```bash
git clone https://github.com/sirus-alpha-canis/mirante.git mirante && cd mirante
cargo build --release --locked -p mirante-cli -p mirante-daemon -p mirante-hook -p mirante-mcp
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
mirante daemon restart
mirante doctor          # which agent binaries the daemon resolved, and from where
```

### 2. The desktop app

The app is source-only until there are signed releases (it is not in Homebrew).
`scripts/install.sh --app` (`--install-app` also copies it to `/Applications`, replacing an
older copy after you confirm, or with `--yes`), or by hand:

```bash
pnpm --dir apps/cockpit install
pnpm --dir apps/cockpit tauri build --bundles app
mirante app             # starts the daemon if needed, then opens the app
```

`mirante app` (alias `mirante open`; `--no-daemon` leaves the daemon alone) opens the first
of: `$MIRANTE_APP`, `/Applications/Mirante.app`, `~/Applications/Mirante.app`, then the
build in the checkout the `mirante` on your `PATH` was built from
(`apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app`). A copy in
`/Applications` wins over a fresh build, so after rebuilding either reinstall it
(`scripts/install.sh --install-app`) or remove it. The desktop app is macOS-only for now.

The app is a thin client of the daemon. It looks for `mirante-daemon` in `~/.cargo/bin`,
`~/.local/bin`, `/opt/homebrew/bin` and `/usr/local/bin`, then on your login shell's `PATH`
(or at `$MIRANTE_DAEMON_BIN`). If no daemon is running, its banner offers **Start daemon**.

An app opened from Finder or the Dock gets macOS's minimal `PATH`. So before it starts a
daemon, the app asks your login shell for its environment, once per run with a 3-second
limit. It passes on only `PATH` and the `MIRANTE_*` variables, and never tokens or keys. If
your shell is slow to start, run `mirante daemon restart` from a terminal instead.

A copied bundle is quarantined on another Mac. For a build you made yourself, clear the
quarantine with `xattr -dr com.apple.quarantine Mirante.app`.

On macOS, `scripts/install.sh` signs what it builds with a local, self-signed identity
(`scripts/dev-sign.sh`; see Troubleshooting, "macOS keeps asking … Documents folder"), so
privacy answers survive rebuilds. To have Tauri sign with an identity of your own, set
`APPLE_SIGNING_IDENTITY` when building (`APPLE_SIGNING_IDENTITY="Mirante Local Signing"
pnpm --dir apps/cockpit tauri build --bundles app`); nothing about signing is committed.
Release builds stay unsigned until there is an Apple Developer ID.

More detail, including the fake daemon for UI work without agents:
[`apps/cockpit/README.md`](apps/cockpit/README.md).

## Use

### The app

`mirante app` opens it (and starts the daemon first if it is not running).

- **Projects.** Add a repository on the left with **Choose…** (the folder picker), or paste
  its path.
- **Dispatch.** Home has one field. Write the goal (the arrow inside the field, or ⌘↵,
  sends), then pick the agent, the repository and the branch from the tokens under it. The
  branch defaults to `mirante/<words-of-the-goal>`, and you can edit it. **+** attaches repo
  files, memory notes or skills. You can
  also start the text with `@claude` or `@codex`: `@claude fix the login` uses the selected
  repository and names the branch for you; `@claude shop fix the login` picks a project by
  name; `@claude shop/feat/login fix the login` also names the branch. You never have to
  type a branch. ⌘↵ dispatches.
- **Home** is an inbox across all projects: what needs you first, then what is ready to
  review, then what is working. The Mirante logo at the top left (or **⌘1**) brings you
  back to it. **⌘J** jumps to the next task that needs you.
- **Detail.** Each task has four tabs. **Steps** shows tool steps summed up. **Terminal** is
  the live terminal. **Diff** shows the task's changes. **Memory** shows the repo notes
  relevant to the task, the notes its branch puts at risk, and what it proposed. The
  composer's arrow steers now (⌘↵); ⌥-click or ⌥↵ queues for the turn end.
- **Grid (⌘G)** shows every agent at once.
- **Brain (⌘B)** is the repo memory graph and its review queue.
- **Skills (⌘L)** lists the skill library and its proposals.
- **Rules (⌘R)** lists what each agent session reads, for a project or one of its tasks.
- **⌘K** opens the command bar: tasks, projects, and `@agent [project[/branch]] goal`.

Closing the app never ends a session, because the daemon owns them. Off macOS, ⌘ is Ctrl.

After an update the old daemon keeps running until it restarts. The app notices (the
daemon's build differs from the `mirante-daemon` binary on disk) and offers **Restart
daemon**; views the old daemon cannot serve offer the same button. Live sessions come back
on their conversations when auto-resume is on.

### The CLI

Every core app action has a CLI equivalent. Run `mirante help` for the full usage and
`mirante <command> --help` for one command. A `<task>` is an id, a unique id suffix (4+
characters) or the branch name. Without `--branch`, `new` names the branch
`mirante/<words-of-the-goal>` (`-2`, `-3`… when taken), like the app.

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
mirante archive feat/login      # remove the worktree, keep the branch (--stop: end it first)
mirante doctor                  # orphan processes and checkouts, memory, agent binaries
mirante daemon start|stop|restart|status
mirante app                     # open the desktop app (starts the daemon first; --no-daemon: not)
```

## Troubleshooting

- **The first start is slow.** The first launch of a freshly built binary can take 10-20
  seconds on macOS while it scans it (XProtect). The CLI waits up to 30 seconds and says
  so. `scripts/install.sh` does that first launch for you.
- **An agent is not found, or the wrong one runs.** `mirante doctor` prints the binary the
  daemon resolved for `claude` and `codex` and where it came from. The daemon reads
  `MIRANTE_CLAUDE_BIN`, `MIRANTE_CODEX_BIN` and `PATH` **when it starts**: after changing
  them in your shell profile, open a new terminal and run `mirante daemon restart`.
- **cmux (or another terminal's) shims.** A `claude` in a cmux shim directory is skipped
  (it would inject its own hooks); `mirante doctor` says "skipped cmux shim …". Set
  `MIRANTE_CLAUDE_BIN` to the real binary if the one it found is not the one you want.
- **A Claude permission card says "Answer in terminal".** That is the default: screen text
  cannot prove which request a dialog belongs to, so you answer in Claude's own terminal.
  Click **Answer in terminal** in the app, or `mirante attach <task>` (also offered by
  `mirante allow`), answer the dialog, then detach with **Ctrl-]**.
- **A red "auto mode on" badge.** That Claude session left manual permission mode and
  approves its own commands. Press Shift+Tab in its terminal until it is back to manual.
- **"macOS reports memory pressure" under the dispatch box.** Advice only: by default the
  memory guard never pauses work (a busy Mac reports pressure most of the time). It shows
  the level, swap and each session's memory, offers to hibernate the largest idle session,
  and only under sustained *critical* pressure hibernates an idle one itself (never one
  that is working or waits on you; `mirante wake <task>` brings it back). `mirante watch`
  and `mirante doctor` say `pausing: off`.
- **New dispatches say memory is paused.** You (or `MIRANTE_MEMGUARD_PAUSE`) set the guard
  to pause: **Pause new sessions under memory pressure** in the app's Sessions section
  (open it at the rail's foot) is *Only when critical* or *When elevated*. New sessions and
  queued deliveries then wait until it eases; set it back to *Off* to keep working. Tune the guard with
  `MIRANTE_MEM_BUDGET_MB` (per-session budget), `MIRANTE_IDLE_HIBERNATE_MIN` (hibernate
  idle sessions) or turn it off with `MIRANTE_MEMGUARD=off`, then restart the daemon.
- **"This daemon cannot … Update Mirante to use it."** The daemon running is older than
  the one installed (`mirante doctor` says so: "restart with: mirante daemon restart"). Run
  `mirante daemon restart`, or click **Restart daemon** in the app.
- **"socket path is N bytes, limit is 103".** Unix socket paths are short:
  `MIRANTE_HOME` must be a short directory (for experiments, `/tmp/mh`).
- **"… is not a git repository" / "has no commit yet".** A task's worktree is a branch of a
  repository with at least one commit: `git init && git commit --allow-empty -m init`.
- **`mirante daemon start` opened no window.** The daemon has none; `mirante app` opens the
  desktop app. If it says the app is not built or installed, run `scripts/install.sh --app`.
- **The app does not open on another Mac.** A copied, unsigned bundle is quarantined. For
  a build you made yourself: `xattr -dr com.apple.quarantine Mirante.app`.
- **After a reboot nothing is running.** The first `mirante` command, or **Start daemon**
  in the app, starts the daemon, which then resumes the sessions it had (auto-resume). To
  have it start at login, install the launch agent: `mirante daemon install-agent` (macOS;
  below).
- **macOS keeps asking "“Mirante” would like to access files in your Documents folder".**
  Two things make it ask again and again, mostly for checkouts under `~/Documents`:
  1. *An ad-hoc signed build.* `cargo build` and `tauri build` sign ad hoc, and macOS
     remembers your answer for that exact build (its cdhash): every rebuild is a new app
     to it. `scripts/install.sh` now signs the four binaries and the app with a local
     identity, "Mirante Local Signing" (made once in your login keychain by
     `scripts/dev-sign.sh`; `--no-sign` skips it), so the answer survives rebuilds. The
     first signing asks whether `codesign` may use that key: type your login password and
     click **Always Allow**. After a
     plain `cargo build --release` or `tauri build`, run `scripts/dev-sign.sh` again.
     `codesign -d -r- <path>` should say `certificate leaf = H"…"`, not `cdhash H"…"`.
  2. *The app is answerable for the daemon.* macOS attributes a process's file access to
     the app that started it: a daemon started by the app's **Start daemon** or **Restart
     daemon** asks in Mirante's name for every repo its agents work in. Let launchd start
     it instead: `mirante daemon install-agent` (in your terminal; it also starts the
     daemon at login; `mirante daemon uninstall-agent` undoes it). The daemon then asks once
     for itself, as `mirante-daemon`.

  One-time steps: `scripts/install.sh --app` (or `scripts/dev-sign.sh` after a build),
  `mirante daemon install-agent`, then open the app and click **Allow** once for Mirante,
  and once for `mirante-daemon` when it asks. Both are listed in **System Settings → Privacy
  & Security → Files and Folders**, where you can check that **Documents Folder** is on.
  If an old, denied entry is stuck there, turn it off and on, or remove Mirante with **–**
  and allow it again. Copying the app to `/Applications` (`--install-app`) also keeps the
  bundle itself out of `~/Documents`. The app asks the daemon binary its version only when
  a daemon starts, never on a timer.
- **Where are the logs?** The daemon's log is `~/.mirante/daemon.log` (rotated to
  `daemon.log.1` past 10 MiB); each task's agent stderr is under
  `~/.mirante/sessions/<task-id>/`; hook events that could not reach the daemon are
  recorded in `~/.mirante/hook-lost`. `mirante --verbose <command>` adds the daemon's error
  codes to error messages. (`MIRANTE_HOOK_LOG` is read only by the spike kit's hook, not by
  `mirante-hook`.)

## Uninstall

With Homebrew: `mirante daemon stop`, then `brew uninstall mirante` (and
`brew untap sirus-alpha-canis/tap`). That leaves `~/.mirante`; delete it yourself if you
want the data gone. From source:

```bash
scripts/uninstall.sh            # stops the daemon, removes the links install.sh made
scripts/uninstall.sh --purge    # also deletes ~/.mirante, after you type "delete"
```

`uninstall.sh` keeps `~/.mirante` (the task database and history, the skill library and
its signing key, the task worktrees) unless you pass `--purge` and type `delete` at its
prompt. The worktrees' branches stay in your repositories (`git worktree prune` in a repo
forgets the removed checkouts). Repo memory notes (`<repo>/.mirante/memory/`) are your
files and are never touched. Mirante never edits `~/.claude` or `~/.codex`, so there is
nothing to undo there. Delete the checkout, and any copy of `Mirante.app` you made, yourself.

## Environment

The daemon reads these when it starts (`mirante daemon restart` after a change); the CLI
and the app pass their own environment to a daemon they start. The app passes only `PATH`
and `MIRANTE_*` from your login shell.

| Variable | Read by | What it does |
|---|---|---|
| `MIRANTE_HOME` | all | Where Mirante keeps its state (default `~/.mirante`). Keep it short: the socket path must fit in about 104 bytes. |
| `MIRANTE_CLAUDE_BIN` | daemon | The real `claude` binary (default: `PATH` without shims, then the usual install directories). |
| `MIRANTE_CODEX_BIN` | daemon | The real `codex` binary (same search). |
| `MIRANTE_DAEMON_BIN` | CLI, app | The `mirante-daemon` to start (default: the one next to `mirante`; the app searches `~/.cargo/bin`, `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, then your login shell's `PATH`). |
| `MIRANTE_APP` | CLI | The `Mirante.app` that `mirante app` opens (default: `/Applications`, `~/Applications`, then the checkout's build). |
| `MIRANTE_HOOK_BIN` | daemon | The `mirante-hook` Claude sessions report through (default: next to the daemon). |
| `MIRANTE_MCP_BIN` | daemon | The `mirante-mcp` server for memory and skills (default: next to the daemon). |
| `MIRANTE_AUTO_RESUME` | daemon | `off` (or `0`, `false`, `no`): do not resume sessions when the daemon starts again. The app's setting does the same. |
| `MIRANTE_CLAUDE_INLINE_ANSWERS` | daemon | `1`: the daemon may type a Claude permission answer itself when it has transcript evidence for that request (default: answered in the terminal). |
| `MIRANTE_MEM_BUDGET_MB` | daemon | Per-session memory budget in MiB (default: 4 GiB or 25 % of RAM, whichever is smaller). |
| `MIRANTE_IDLE_HIBERNATE_MIN` | daemon | Hibernate a session idle this many minutes (default: off). |
| `MIRANTE_MEMGUARD` | daemon | `off`: no memory guard at all. |
| `MIRANTE_MEMGUARD_PAUSE` | daemon | `off`, `critical` or `elevated`: from which pressure level new sessions and queued deliveries wait, for this daemon (default: the app's setting, *Off*). |
| `MIRANTE_SSH_KEYGEN` | daemon | The `ssh-keygen` used to sign and verify skills (default `/usr/bin/ssh-keygen`; never looked up on `PATH`). |
| `MIRANTE_HOOK_LOG` | spike kit only | Where `crates/spike-kit`'s hook logs; `mirante-hook` does not read it. |
| `MIRANTE_TASK`, `MIRANTE_TASK_ID`, `MIRANTE_SESSION_ID` | hook, MCP | Set by the daemon for the agents it launches. Do not set them yourself. |

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
pressure. It sets no hard limits. A session near its budget gets a warning. Under pressure
it advises (it offers to hibernate the largest idle session) and work goes on; new
dispatches and queued deliveries pause only if you ask for it (the app's **Pause new
sessions under memory pressure**, or `MIRANTE_MEMGUARD_PAUSE`). Under sustained critical pressure, the largest
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

## See what your agents are told

Every agent session reads more than your goal: an organization's managed policy, your
global `~/.claude/CLAUDE.md` or `~/.codex/AGENTS.md`, the project's `CLAUDE.md` /
`AGENTS.md` and their `@imports`, rules folders, settings (permissions, hooks), skills,
Claude's auto memory, and what Mirante itself adds per task (launch mode, injected skills,
the repo-memory tools, the operator-message label). In the app, **Rules** (⌘R, in the top
bar) lists all of them for a project, for Claude or Codex, in the order the agent loads
them: scope, path, size, contents as plain text, imports you can expand, and the places
where rules could live but do not (greyed). Its project picker starts on the selected
project; **For** picks one of the project's tasks to show that task's worktree and
session instead. From a task, **Rules this agent received** (or "Rules this task received"
in ⌘K) opens it on that task. It works with repo memory off.

It is read-only: Mirante never writes to `~/.claude`, `~/.codex` or your files. It reads
only inside your home folder, the project and the managed folders, never a FIFO or device,
at most 64 KiB per file; files that usually hold credentials are never shown, and secrets
in settings (`env` values, tokens) are masked. The loading rules, with the agent versions
they were checked against and their sources:
[`docs/agents/instruction-layers.md`](docs/agents/instruction-layers.md).

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

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs on every pull request and
push to `main`: the workspace build, tests (e2e included) and both clippy passes on macOS,
the core crates and clippy on Linux, the app's tests, typecheck, build and Tauri crate
checks, `cargo deny` (licenses and advisories, [`deny.toml`](deny.toml)) for both Rust
workspaces, and shellcheck on the scripts. On `main` it also runs a new user's path:
`scripts/install.sh` into a temp prefix, `mirante doctor`, `scripts/try.sh --smoke` and
`scripts/uninstall.sh`. Dependabot proposes weekly updates of the actions, crates and npm
packages. See [`CONTRIBUTING.md`](CONTRIBUTING.md#checks).

Without agents: `scripts/try.sh` runs the real daemon with the e2e tests' fake `claude`
(`crates/mirante-cli/tests/fake-claude.sh`) in a throwaway repo and its own `MIRANTE_HOME`
under `/tmp`, and prints what to try next; `--smoke` checks it and cleans up. For UI work
without any daemon, the scripted fake daemon is in
[`apps/cockpit/README.md`](apps/cockpit/README.md#without-agents-the-fake-daemon).

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
