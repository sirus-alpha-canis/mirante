# Mirante cockpit (desktop app)

Tauri 2 + React. A thin client of `mirante-daemon`: every call goes over the daemon's Unix
socket through `crates/mirante-bridge`; the app has no network access, no login and no
telemetry.

## Run it

Install Mirante as the root [README](../../README.md#quickstart-5-minutes) says: from the
repository root, `scripts/install.sh --app` builds and links the CLI, the daemon, the hook
and the MCP server (into `~/.local/bin`), starts the daemon and builds this app. The
prerequisites are there too: Rust through rustup at the version pinned in
`rust-toolchain.toml`, Node 22.12+, pnpm 10 (pinned in `package.json`'s `packageManager`)
and the Xcode command line tools.

For work on the app itself, after `scripts/install.sh`:

```bash
pnpm --dir apps/cockpit install
pnpm --dir apps/cockpit tauri dev
```

If no daemon is running, the banner offers **Start daemon**. If the daemon running is older
than the `mirante-daemon` binary the app would start (its build, from `daemon.status`,
differs from what `mirante-daemon --version` prints), a banner offers **Restart daemon**
(the shell's `daemon_restart`: `daemon.shutdown`, then the same start).

An app opened from Finder or the Dock gets macOS's minimal `PATH` and none of your
exports, so before **Start daemon** starts a daemon the app asks your login shell once per
run: `$SHELL` (only if listed in `/etc/shells`, else `/bin/zsh`) runs as `-l -i -c` with
stdin closed and a 3 s limit, and prints its environment. The daemon gets only `PATH` and
`MIRANTE_*` from it (`MIRANTE_CLAUDE_BIN`, `MIRANTE_CODEX_BIN`,
`MIRANTE_CLAUDE_INLINE_ANSWERS`…; never `MIRANTE_HOME`, which the app's socket decides),
plus `HOME`/`USER`/`LANG` if the app lacks them; nothing else (no tokens or keys) is passed
on. If the shell fails or is too slow, the daemon gets the app's `PATH` with `~/.local/bin`,
`/opt/homebrew/bin`, `/usr/local/bin` and `~/.cargo/bin` appended.

The daemon also finds the agents on its own: `MIRANTE_CLAUDE_BIN` / `MIRANTE_CODEX_BIN`,
then `PATH`, then those directories and nvm's (`~/.nvm/versions/node/*/bin`, newest Node
that has the agent), never a cmux shim; an agent found off `PATH` runs with its directory
first on `PATH`, so an npm-installed `codex` finds its `node`. `mirante doctor` prints which
binaries the daemon resolved and from where.

### Without agents: the fake daemon

The scripted fake daemon serves three demo tasks (working, needs-you with a permission card,
ready for review) and adds a step every two seconds. Its repo memory (`dna.*`) is a 6-note demo
graph for `/repos/mirante` (one stale note, one unreviewed, one dangling link) with two pending
proposals; `/repos/shop` starts with memory off, to show the enable path:

```bash
mkdir -p /tmp/mr-ui
cargo run -p mirante-bridge --features fake --bin mirante-fake-daemon -- /tmp/mr-ui/daemon.sock &
MIRANTE_HOME=/tmp/mr-ui pnpm --dir apps/cockpit tauri dev
```

(`MIRANTE_HOME` must be a short path: Unix socket paths are limited to about 104 bytes.)

## Test

```bash
pnpm --dir apps/cockpit test            # Vitest: store, parser, components
pnpm --dir apps/cockpit typecheck
cargo test -p mirante-bridge            # the bridge against a fake daemon
```

Smoke test of the built app against the fake daemon (opens the window for a few seconds):

```bash
cargo build -p mirante-bridge --features fake
pnpm --dir apps/cockpit tauri build --debug --bundles app
apps/cockpit/scripts/smoke.sh
```

## Build for local use (macOS, unsigned)

```bash
pnpm --dir apps/cockpit tauri build --bundles app
mirante app             # starts the daemon if needed, then opens this build
```

`mirante app` opens `$MIRANTE_APP` if set, else `/Applications/Mirante.app`, else
`~/Applications/Mirante.app`, else this checkout's build, so a stale copy in `/Applications`
wins over a fresh build: `scripts/install.sh --install-app` replaces it (after you confirm,
or with `--yes`), or point `MIRANTE_APP` at the build.

The bundle is not signed or notarized. Copied to another Mac it is quarantined; clear that
with `xattr -dr com.apple.quarantine Mirante.app` (only for a build you made yourself).

## Projects

Add a repository on the left with **Choose…** (the macOS folder picker; the app may use the
picker's `open` and nothing else of the dialog plugin) or by pasting its path and **Add**
(`~/…` is expanded with your home folder). A path that is missing or a file is refused there;
whether it is a git checkout is checked when you dispatch a task to it.

## Home: an inbox of every project

Home lists what needs you first, across all projects: **Needs you** (each card with its one
action: Allow / Deny for Codex, Answer in terminal for Claude, Trust, or Open terminal),
**Ready for review** (the agent's last answer in two lines, expandable, and Review diff),
**Working** (one line each: title, agent, time on the turn, what it is doing in plain words;
the raw steps stay in the task's detail), and the **Idle** ones folded away, all grouped by
project with a color per project. Past 10 minutes it says how many agents have waited on
you. **Next** (⌘J) opens the next task that needs you (Claude's on its Terminal tab) and
says where you are ("2 of 5"). Escape leaves a task's detail for the view it was opened
from, and Home puts the keyboard back on that task's card. The detail's task list groups by
project then state, with filters (Needs you / Working / Ready / All) and a search; its Steps
tab sums runs of tool steps up ("read 14 files, ran 3 commands", click to see them), folds
repeated lines (×N), follows new lines while scrolled to the bottom, and says when a task's
history could not be loaded (with Retry). Every view that loads from the daemon offers
Retry when that fails, and loads again when the daemon restarts. A Claude session that leaves manual
permission mode gets a red **auto mode on** badge (from the `permission_mode` its hooks and
transcript report); switch back with Shift+Tab in its terminal.

The app remembers where you were (view, project, open task and tab, Home's scroll) on this
machine. Closing it never ends a session: the daemon owns them. When the daemon itself
restarts (an upgrade, a reboot, a crash), the sessions it ended are resumed on their
conversations, one at a time while memory allows; turn that off under Sessions on the left
(or `MIRANTE_AUTO_RESUME=off`). Stopped and hibernated tasks are never resumed by it.

While the daemon is not reachable the banner counts down to the next try, and actions that
need it (dispatch, steer and queue, answers, session actions) are off. A task's terminal
attaches again on its own when its stream ends while the task is live (after 0.5, 1, 2 and
5 s, then a **Reconnect** button) and at once when the daemon restarts; the daemon repaints
the screen, so the terminal is cleared first. ⌘/Ctrl-click opens a terminal link in the
browser only for github.com and local dev servers (`http://localhost:<port>` or
`http://127.0.0.1:<port>`); any other link says to copy it. A click on a task's
notification opens that task.

## Language

The interface speaks English or Portuguese (Brazil): the **EN / PT** menu at the right of
the top bar (or "Language" in the command bar) switches it, and the choice is remembered on
this machine. Until you choose, a system language starting with `pt` gives Portuguese,
anything else English. Every word the app shows lives in `src/lib/i18n.ts` (`en` is
complete and the fallback; `pt` translates every key, checked by `i18n.test.ts`). What
Mirante sends to agents (the dispatch's context references, memory and skill requests)
stays in English whatever the interface's language.

## Dispatch

Home's form is one box: write the goal, then use the chips below it. **Agent** (Claude or
Codex, remembered on this machine; more agents will join the menu), **Repository** (one of
your Projects; it starts on the one selected on the left; type to filter), the branch (it
appears once there is a goal: `mirante/<words-of-the-goal>`, accents dropped, about 40
characters; a name another task of the repo uses, or one git already has, moves on to `-2`,
`-3`…; **edit** to type your own, and an existing branch is then checked out as it is), and
**+ context**: attach repo files (searched by path), repo memory notes or skills; they are
added after the goal as a labeled list of references for the agent ("Context attached in
Mirante"), nothing else is read or sent. Files and notes belong to the repository they were
picked in: a dispatch to another repository is refused until they are removed. The form
(goal, attached context, edited branch, picked repository) is kept while you visit other
views. While the memory guard pauses new sessions the form says so. Text starting with `@claude` or `@codex` is the one-line form,
`@claude shop/feat/login add a login page`: typing `@` suggests the agents, then the
Projects (filtered as you type; ↑/↓, Enter or Tab picks, Esc closes); `shop/` with no
branch gets one named from the goal.

## Keys

| Keys | What |
|---|---|
| ⌘K | Command bar (tasks, projects, `@claude repo/branch goal` into the dispatch form) |
| ⌘G | Toggle the agent grid |
| ⌘1 | Home (also: the Mirante logo at the top left) |
| ⌘J | Next: the next task that needs you |
| ⌘B | Brain: the repo memory graph and its review queue |
| ⌘L | Skills |
| ⌘R | Rules: what each agent session reads, for a project or one task (again: back) |
| Esc | Leave a task's detail (back to the view it was opened from); in Rules opened from a task, back to that task |
| ⇧⌘J | Theme: system → light → dark |
| ⌘↵ | In Home's dispatch form: dispatch (Enter is a new line of the goal) |
| ⌘↵ / ⌥↵ | In a task's composer: steer now / queue |

Off macOS, ⌘ is Ctrl (the hints in the app name your platform's keys). Keys match their
physical position, so they work on any keyboard layout; holding a key never repeats a
shortcut, and while the command bar is open only ⌘K and Esc act. In a task's terminal,
off macOS the Ctrl chords (Ctrl+K, Ctrl+G…) belong to the terminal; on macOS ⌘K still
opens the command bar there (⌘ is not a terminal key).
