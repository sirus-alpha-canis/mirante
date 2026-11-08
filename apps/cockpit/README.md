# Mirante cockpit (desktop app)

Tauri 2 + React. A thin client of `mirante-daemon`: every call goes over the daemon's Unix
socket through `crates/mirante-bridge`; the app has no network access, no login and no
telemetry.

## Run it

Prerequisites: Rust (stable), Node 22.12+ and pnpm 10, Xcode command line tools (macOS).

```bash
# The daemon, the hook and the CLI, once (the app finds the daemon in ~/.cargo/bin,
# ~/.local/bin, /opt/homebrew/bin, /usr/local/bin or on your login shell's PATH, or at
# $MIRANTE_DAEMON_BIN):
cargo install --path crates/mirante-daemon
cargo install --path crates/mirante-hook
cargo install --path crates/mirante-cli

pnpm --dir apps/cockpit install
pnpm --dir apps/cockpit tauri dev
```

If no daemon is running, the banner offers **Start daemon**.

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
open apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app
```

The bundle is not signed or notarized. Copied to another Mac it is quarantined; clear that
with `xattr -dr com.apple.quarantine Mirante.app` (only for a build you made yourself).

## Keys

| Keys | What |
|---|---|
| ⌘K | Command bar (tasks, projects, `@claude repo/branch goal` to dispatch) |
| ⌘G | Toggle the agent grid |
| ⌘1 | Home |
| Esc | Leave a task's detail |
| ⇧⌘J | Theme: system → light → dark |
| ⌘↵ / ⌥↵ | In a task's composer: steer now / queue |

Off macOS, ⌘ is Ctrl (the hints in the app name your platform's keys). Keys match their
physical position, so they work on any keyboard layout; holding a key never repeats a
shortcut, and while the command bar is open only ⌘K and Esc act. In a task's terminal,
off macOS the Ctrl chords (Ctrl+K, Ctrl+G…) belong to the terminal; on macOS ⌘K still
opens the command bar there (⌘ is not a terminal key).
