# Changelog

All notable changes to Mirante. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [Semantic Versioning](https://semver.org/) (0.x: anything may change). Every
crate and the app share one version. How a release is cut: [RELEASING.md](RELEASING.md).

## [Unreleased]

### Fixed

- **The × next to a project in the rail did nothing** for a project that still had tasks
  (it was listed because of its tasks, so dropping it from the saved list changed nothing,
  silently). Every project now has a × that works: with no tasks it goes at once; with
  tasks it asks inline: **Archive tasks and remove** (live sessions stopped first,
  worktrees removed, branches kept; a dirty worktree the daemon refuses is listed with why
  and an **Open task**, and its project stays) or **Hide from Projects** (tasks kept; the
  project and its tasks leave the rail and Home; **Show hidden projects (n)** brings them
  back).

### Added

- **Add project takes a folder of repositories.** A folder that is no git checkout is looked
  into (two levels deep, symlinks never followed, hidden / `node_modules` / `target` /
  `dist` / `build` skipped, capped and time-limited; the app's new `scan_repos` command,
  read-only) and the repositories found are listed to pick: main ones picked, linked
  worktrees not ("worktree of …"). The picked ones sit under the folder's name in the
  rail, foldable, with one × for the group. A folder added before (not a repository) shows
  "… is a folder, not a repository" with **Scan it for repositories**. Past 8 projects the
  list gets a filter.

### Changed

- **Rules is a top-level view** in the app's top bar, next to Brain and Skills (⌘R; Ctrl+R
  off macOS, which never reloads the window). A project picker (the selected project by
  default, works with repo memory off), the Claude / Codex switch, and **For** to show
  what one of the project's tasks received. ⌘K has "Go to Rules" and, with a task open,
  "Rules this task received"; a task's **Rules this agent received** opens the view on
  that task (Esc goes back to it). The app reopens on Rules if it closed there. Brain no
  longer has a Notes | Rules switch: it is the notes graph only.

## [0.1.0] — 2026-11-09

The first release: prebuilt binaries for macOS (Apple silicon and Intel) on GitHub Releases
and a Homebrew formula for the CLI, daemon, hook and MCP server
(`brew install sirus-alpha-canis/tap/mirante`). The desktop app is built from source until
signed releases.

### What exists today

- **Daemon** (`mirante-daemon`): one per `MIRANTE_HOME`, SQLite as the single owner of
  state, JSON-RPC over a local Unix socket (`0600`, peer uid checked) with a per-identity
  method allowlist that tells a person from a Mirante-launched agent.
- **Agents**: Claude Code (its interactive CLI in a PTY; status from hooks and transcript)
  and Codex (`codex app-server` over JSON-RPC), each task in its own git worktree, under
  your own logins. Steer now or queue for the turn end; interrupt, stop, resume on the
  native conversation, hibernate and wake.
- **Permissions**: persisted cards answered once by a person; nothing auto-approved, no
  skip-permission flags. Claude dialogs are answered in the terminal by default; the
  daemon types an answer only with `MIRANTE_CLAUDE_INLINE_ANSWERS=1` and transcript
  evidence. A red badge when a Claude session leaves manual mode.
- **Memory guard**: per-session budgets, pause under pressure, hibernate the largest idle
  session under sustained critical pressure; orphan processes and checkouts reported by
  `mirante doctor`. Auto-resume after a daemon restart.
- **Repo memory (DNA)**, opt-in per repo: cited Markdown notes in `.mirante/memory/`,
  flagged stale when the cited code changes; agents read and propose through
  `mirante-mcp`; a person reviews; `mirante memory check --offline` for CI.
- **Skills**: a reviewed, linted, signed (ssh-keygen ed25519) library scoped per client,
  injected into each new session in the agent's native form; opt-in learning from
  repeated instructions. Mirante never edits `~/.claude` or `~/.codex`.
- **CLI** (`mirante`): every core action of the app.
- **Desktop app** (Tauri 2 + React): Home inbox across projects, dispatch form, task detail
  (Steps, Terminal, Diff, Memory), grid, Brain, Skills, command bar, English and
  Portuguese.

### Added

- Distribution: `.github/workflows/release.yml` builds `mirante`, `mirante-daemon`,
  `mirante-hook` and `mirante-mcp` for `aarch64-apple-darwin` and `x86_64-apple-darwin` on a
  `v*` tag and publishes `mirante-<version>-<target>.tar.gz` (binaries, LICENSE,
  THIRD_PARTY_NOTICES.md, README.md) with `SHA256SUMS` as a GitHub Release; with the
  `HOMEBREW_TAP_TOKEN` secret it also updates the formula in
  [sirus-alpha-canis/homebrew-tap](https://github.com/sirus-alpha-canis/homebrew-tap).
  `scripts/homebrew-formula.sh` writes that formula. Steps: `RELEASING.md`.
- `scripts/install.sh` (build, link into `~/.local/bin`, warm up, start the daemon,
  doctor; `--app`, `--prefix`, `--no-start`), `scripts/uninstall.sh` (`--purge` only with a
  typed confirmation) and `scripts/try.sh` (a tour with a fake agent; `--smoke` for CI).
- `mirante --version` / `-V`, also answered by `mirante-daemon`, `mirante-hook` and
  `mirante-mcp`.
- Per-command help (`mirante <command> --help`, `mirante help <command>`); a usage error
  prints that command's usage only.
- `mirante new` without `--branch` names the branch `mirante/<words-of-the-goal>` with the
  app's rule (`-2`, `-3`… when taken); both check one shared set of test vectors.
- `mirante archive --stop`.
- `mirante app` (alias `mirante open`) opens the desktop app on macOS, starting the daemon
  first unless `--no-daemon`. It opens `$MIRANTE_APP`, `/Applications/Mirante.app`,
  `~/Applications/Mirante.app` or the build in the CLI's own checkout, and says how to
  build the app when there is none. `mirante daemon start|restart` on a terminal end with
  a hint to it (the daemon has no window).
- `scripts/install.sh --install-app` copies the built app to `/Applications` (an older copy
  is replaced after you confirm, or with `--yes`); its next steps suggest `mirante app`.
- `mirante --verbose` keeps the daemon's error codes in error messages.
- **Brain → Rules** and a task's **Rules this agent received**: every instruction layer a
  Claude or Codex session reads (managed, global, project, local, nested, and what Mirante
  adds), with contents as plain text, `@imports`, settings with secrets masked, and missing
  places greyed; read-only, works with repo memory off. Daemon: `rules.layers` (a person
  only). Loading rules: `docs/agents/instruction-layers.md`.
- The daemon reports its build (`<commit>[-dirty].<unix seconds>`, embedded at compile
  time; `MIRANTE_BUILD_ID` / `SOURCE_DATE_EPOCH` for reproducible builds) in
  `daemon.status` and `mirante-daemon --version`. When the running daemon is older than the
  binary on disk the app shows "Mirante was updated — restart the daemon…" with a
  **Restart daemon** button (also in place of "Update Mirante" in Rules, repo memory and
  Skills), and `mirante doctor` prints "restart with: mirante daemon restart".
- README: Quickstart, Troubleshooting, Uninstall and an Environment reference;
  `SECURITY.md`; issue templates; Dependabot and `cargo deny` in CI.

### Changed

- The CLI waits up to 30 s for a starting daemon (the first launch of a fresh build is
  scanned by macOS) and says so; commands that auto-start at once start one daemon.
- `task.create` refuses a folder that is not a git repository (or has no commit) and an
  agent binary the daemon cannot find, before any branch or worktree exists.
- Error messages no longer end in the daemon's internal "(code N)".
- `mirante doctor` names the wrapper shims it skipped.
- The app's auto-named branches fold ß, æ and œ to ss, ae and oe, like the CLI's.
- The app's top bar: the Orb-Dot mark and "Mirante" are the Home button (⌘1); the
  separate Home tab is gone.
- The left rail's System memory panel shows one muted line, the latest notice of the last
  10 minutes (a hibernate suggestion only while pressure is still elevated), and the rest
  under **History**.

### Fixed

- `mirante daemon status` with a `MIRANTE_HOME` too long for a socket path says so instead
  of "not running".
- A daemon that exits during startup is reported with the end of its log, not
  "No such file or directory (os error 2)".

[Unreleased]: https://github.com/sirus-alpha-canis/mirante/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/sirus-alpha-canis/mirante/releases/tag/v0.1.0
