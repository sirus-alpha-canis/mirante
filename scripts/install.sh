#!/usr/bin/env bash
# Builds Mirante from this checkout and links its four binaries (mirante, mirante-daemon,
# mirante-hook, mirante-mcp) into a bin directory, then starts the daemon and runs
# `mirante doctor`. Safe to run again: it rebuilds, relinks and restarts an idle daemon.
#
#   scripts/install.sh [--prefix <bin dir>] [--app [--install-app [--yes]]] [--no-start] [--no-sign]
#
#   --prefix <dir>  where the links go (default: $PREFIX, else ~/.local/bin)
#   --app           also build the desktop app (needs Node 22.12+ and pnpm 10)
#   --install-app   build it (implies --app) and copy it to /Applications (macOS); an older
#                   copy there is replaced only after you confirm, or with --yes
#   --yes           replace an older /Applications/Mirante.app without asking
#   --no-start      do not start or restart the daemon (and skip doctor)
#   --sign          (macOS, the default) sign the binaries and the app with a local identity
#                   (scripts/dev-sign.sh, which creates it once in your login keychain), so
#                   macOS privacy answers (Documents access) survive rebuilds
#   --no-sign       leave them ad-hoc signed (macOS then asks again after every build)
#
# It never edits your shell profile, ~/.claude or ~/.codex; on macOS it adds one code-signing
# identity to your login keychain (not with --no-sign). Undo with scripts/uninstall.sh.
set -euo pipefail

BINS=(mirante mirante-daemon mirante-hook mirante-mcp)

say() { printf '==> %s\n' "$*"; }
warn() { printf 'install.sh: warning: %s\n' "$*" >&2; }
die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; }

prefix="${PREFIX:-$HOME/.local/bin}"
app=0
install_app=0
yes=0
start=1
sign=1
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
    --install-app)
      app=1
      install_app=1
      shift
      ;;
    --yes | -y)
      yes=1
      shift
      ;;
    --no-start)
      start=0
      shift
      ;;
    --sign)
      sign=1
      shift
      ;;
    --no-sign)
      sign=0
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
if [ "$install_app" = 1 ] && [ "$(uname -s)" != Darwin ]; then
  die "--install-app copies the app to /Applications, which is macOS-only"
fi
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

# --- Sign (macOS): a stable identity, so TCC remembers its answers across rebuilds.
[ "$(uname -s)" = Darwin ] || sign=0
dev_sign() {
  [ "$sign" = 1 ] || return 0
  "$root/scripts/dev-sign.sh" "$@" ||
    warn "could not sign $* (scripts/dev-sign.sh above): macOS will ask for Documents access again after each build"
}
dev_sign "${BINS[@]/#/$root/target/release/}"

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
    # shellcheck disable=SC2016 # a literal $PATH for the profile
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
# Copies the built bundle to /Applications/Mirante.app. An older copy is replaced only after
# a "y" on the terminal (or with --yes); without a terminal it is left alone.
install_app_bundle() {
  local src="$1" dest=/Applications/Mirante.app answer=""
  if [ -e "$dest" ] && [ "$yes" != 1 ]; then
    if [ -r /dev/tty ] && { : </dev/tty; } 2>/dev/null; then
      printf 'Replace the existing %s with this build? [y/N] ' "$dest" >/dev/tty
      read -r answer </dev/tty || answer=""
    fi
    case "$answer" in
      y | Y | yes | YES) ;;
      *)
        warn "left $dest as it was (scripts/install.sh --install-app --yes replaces it); 'mirante app' opens it"
        return 0
        ;;
    esac
  fi
  say "Copying the app to $dest"
  local tmp="$dest.installing.$$"
  rm -rf "$tmp"
  ditto "$src" "$tmp" || {
    rm -rf "$tmp"
    die "could not copy the app to /Applications (permissions?)"
  }
  rm -rf "$dest"
  mv "$tmp" "$dest"
}

if [ "$app" = 1 ]; then
  command -v pnpm >/dev/null 2>&1 || die "--app needs pnpm 10 (corepack enable, or npm i -g pnpm@10) and Node 22.12+"
  say "Building the desktop app (apps/cockpit)"
  pnpm --dir apps/cockpit install --frozen-lockfile
  pnpm --dir apps/cockpit tauri build --bundles app
  bundle="$root/apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app"
  if [ -d "$bundle" ]; then
    # APPLE_SIGNING_IDENTITY set: Tauri signed it with that identity already.
    [ -n "${APPLE_SIGNING_IDENTITY:-}" ] || dev_sign "$bundle"
    say "App: $bundle"
    if [ "$install_app" = 1 ]; then
      install_app_bundle "$bundle"
    else
      if [ -e /Applications/Mirante.app ]; then
        warn "'mirante app' opens /Applications/Mirante.app first, an older copy: run with --install-app to replace it"
      else
        say "Open it with 'mirante app'; to copy it to /Applications: scripts/install.sh --install-app"
      fi
    fi
  elif [ "$install_app" = 1 ]; then
    die "the app build left no $bundle to copy"
  fi
fi

cat <<EOF

Mirante is installed: ${BINS[*]} -> $prefix
$([ "$on_path" = 1 ] || printf '(add %s to your PATH first; see the warning above)\n' "$prefix")
Next:
  mirante app                   # open the desktop app (build it with --app; the daemon has no window)
  cd <a git repo>
  mirante new . --agent claude "describe what you want done"   # or --agent codex
  mirante ls                    # status; what needs you
  mirante attach <branch>       # the live terminal; Ctrl-] detaches

  scripts/try.sh                # a 1-minute tour with a fake agent (no login, no quota)
  scripts/install.sh --app      # also build the desktop app (--install-app: copy it to /Applications)
  mirante help                  # every command; mirante <command> --help for one

If an agent is not found: set MIRANTE_CLAUDE_BIN / MIRANTE_CODEX_BIN in your shell profile,
open a new terminal and run 'mirante daemon restart' (the daemon reads them when it starts).
EOF
