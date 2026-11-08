#!/usr/bin/env bash
# Builds Mirante from this checkout and links its four binaries (mirante, mirante-daemon,
# mirante-hook, mirante-mcp) into a bin directory, then starts the daemon and runs
# `mirante doctor`. Safe to run again: it rebuilds, relinks and restarts an idle daemon.
#
#   scripts/install.sh [--prefix <bin dir>] [--app] [--no-start]
#
#   --prefix <dir>  where the links go (default: $PREFIX, else ~/.local/bin)
#   --app           also build the desktop app (needs Node 22.12+ and pnpm 10)
#   --no-start      do not start or restart the daemon (and skip doctor)
#
# It never edits your shell profile, ~/.claude or ~/.codex. Undo with scripts/uninstall.sh.
set -euo pipefail

BINS=(mirante mirante-daemon mirante-hook mirante-mcp)

say() { printf '==> %s\n' "$*"; }
warn() { printf 'install.sh: warning: %s\n' "$*" >&2; }
die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

usage() { sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; }

prefix="${PREFIX:-$HOME/.local/bin}"
app=0
start=1
while [ $# -gt 0 ]; do
  case "$1" in
    --prefix)
      [ $# -ge 2 ] || die "--prefix needs a directory"
      prefix="$2"
      shift 2
      ;;
    --prefix=*)
      prefix="${1#--prefix=}"
      shift
      ;;
    --app)
      app=1
      shift
      ;;
    --no-start)
      start=0
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
done
[ -n "$prefix" ] || die "--prefix is empty"
case "$prefix" in /*) ;; *) prefix="$PWD/$prefix" ;; esac

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
[ -f "$root/Cargo.toml" ] && [ -d "$root/crates/mirante-cli" ] || die "run this from a Mirante checkout ($root is not one)"
cd "$root"

# --- Toolchain: rustup first. A Homebrew cargo is too old for the edition-2024 dependencies.
for d in /opt/homebrew/opt/rustup/bin "$HOME/.cargo/bin"; do
  if [ -x "$d/rustup" ]; then
    PATH="$d:$PATH"
  fi
done
export PATH
command -v rustup >/dev/null 2>&1 || die "rustup is required (https://rustup.rs). A Homebrew 'rust' is not enough: install rustup, then run this again."
command -v git >/dev/null 2>&1 || die "git is required (xcode-select --install)"
pinned="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)"
say "Rust toolchain $pinned (rust-toolchain.toml)"
# Installs the pinned toolchain if missing (older rustup: `rustup show` does it).
rustup toolchain install >/dev/null 2>&1 || rustup show >/dev/null
have="$(cargo -V 2>/dev/null || true)"
case "$have" in
  "cargo $pinned "*) ;;
  *) die "cargo here is '$have', not $pinned: put rustup's cargo first on PATH (export PATH=\"/opt/homebrew/opt/rustup/bin:\$PATH\")" ;;
esac

# --- Build.
say "Building ${BINS[*]} (release; the first build takes a few minutes)"
cargo build --release --locked -p mirante-cli -p mirante-daemon -p mirante-hook -p mirante-mcp

# --- Link. Only links are replaced: a real file of the same name is left alone.
say "Linking into $prefix"
mkdir -p "$prefix"
for b in "${BINS[@]}"; do
  target="$root/target/release/$b"
  [ -x "$target" ] || die "$target was not built"
  link="$prefix/$b"
  if [ -e "$link" ] && [ ! -L "$link" ]; then
    die "$link exists and is not a link (another install?): remove it, then run this again"
  fi
  ln -sfn "$target" "$link"
done
case ":$PATH:" in
  *":$prefix:"*) on_path=1 ;;
  *)
    on_path=0
    warn "$prefix is not on your PATH. Add this to your shell profile (~/.zshrc), then open a new terminal:"
    printf '    export PATH="%s:$PATH"\n' "$prefix" >&2
    ;;
esac

# --- Warm up: macOS scans a freshly built binary on its first launch (10-20 s each).
say "First launch of each binary (macOS may scan each one for a few seconds)"
for b in "${BINS[@]}"; do
  "$prefix/$b" --version
done

# --- Daemon.
mirante="$prefix/mirante"
if [ "$start" = 1 ]; then
  status="$("$mirante" daemon status 2>&1 || true)"
  case "$status" in
    running*"live sessions 0 "*)
      say "Restarting the idle daemon on the new build"
      "$mirante" daemon restart
      ;;
    running*)
      warn "the daemon is running with live sessions; it keeps the old build until you run"
      warn "'mirante daemon restart' (that ends them; auto-resume brings them back)"
      ;;
    *)
      say "Starting the daemon"
      "$mirante" daemon start
      ;;
  esac
  say "mirante doctor"
  "$mirante" doctor || warn "doctor failed (see above)"
fi

# --- The desktop app.
if [ "$app" = 1 ]; then
  command -v pnpm >/dev/null 2>&1 || die "--app needs pnpm 10 (corepack enable, or npm i -g pnpm@10) and Node 22.12+"
  say "Building the desktop app (apps/cockpit)"
  pnpm --dir apps/cockpit install --frozen-lockfile
  pnpm --dir apps/cockpit tauri build --bundles app
  bundle="$root/apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app"
  [ -d "$bundle" ] && say "App: $bundle (open it, or copy it to /Applications)"
fi

cat <<EOF

Mirante is installed: ${BINS[*]} -> $prefix
$([ "$on_path" = 1 ] || printf '(add %s to your PATH first; see the warning above)\n' "$prefix")
Next:
  cd <a git repo>
  mirante new . --agent claude "describe what you want done"   # or --agent codex
  mirante ls                    # status; what needs you
  mirante attach <branch>       # the live terminal; Ctrl-] detaches

  scripts/try.sh                # a 1-minute tour with a fake agent (no login, no quota)
  scripts/install.sh --app      # also build the desktop app
  mirante help                  # every command; mirante <command> --help for one

If an agent is not found: set MIRANTE_CLAUDE_BIN / MIRANTE_CODEX_BIN in your shell profile,
open a new terminal and run 'mirante daemon restart' (the daemon reads them when it starts).
EOF
