#!/usr/bin/env bash
# Undoes scripts/install.sh: stops the daemon (ending live sessions) and removes the links
# install.sh made. Your data stays unless you ask for --purge.
#
#   scripts/uninstall.sh [--prefix <bin dir>] [--purge]
#
#   --prefix <dir>  where install.sh put the links (default: $PREFIX, else ~/.local/bin)
#   --purge         also delete Mirante's state directory ($MIRANTE_HOME, else ~/.mirante):
#                   the task database, logs, the skill library and its signing key, and
#                   the task worktrees under it (uncommitted work in them is lost; the
#                   branches stay in your repos). It asks you to type "delete" first.
#
# Mirante never edits ~/.claude or ~/.codex, so there is nothing to undo there. Repo memory
# notes (<repo>/.mirante/memory) are your files in your repos; they are never touched.
set -euo pipefail

BINS=(mirante mirante-daemon mirante-hook mirante-mcp)

say() { printf '==> %s\n' "$*"; }
die() {
  printf 'uninstall.sh: %s\n' "$*" >&2
  exit 1
}

usage() { sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; }

prefix="${PREFIX:-$HOME/.local/bin}"
purge=0
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
    --purge)
      purge=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
done

home="${MIRANTE_HOME:-}"
if [ -z "$home" ]; then
  [ -n "${HOME:-}" ] || die "neither MIRANTE_HOME nor HOME is set"
  home="$HOME/.mirante"
fi

# --- Stop the daemon (through the installed CLI, so it is the one for this home).
if [ -x "$prefix/mirante" ]; then
  say "Stopping the daemon (live sessions end; their conversations stay with the agents)"
  "$prefix/mirante" daemon stop || true
fi

# --- The LaunchAgent (mirante daemon install-agent), when it runs this home's daemon.
agent="$HOME/Library/LaunchAgents/com.mirante.daemon.plist"
if [ "$(uname -s)" = Darwin ] && [ -f "$agent" ] && grep -qF "<string>$home</string>" "$agent"; then
  say "Removing the launch agent $agent"
  launchctl bootout "gui/$(id -u)/com.mirante.daemon" 2>/dev/null || true
  rm -f "$agent"
fi

# --- Remove only links that point at a Mirante build (target/release/<name>).
for b in "${BINS[@]}"; do
  link="$prefix/$b"
  if [ -L "$link" ]; then
    case "$(readlink "$link")" in
      */target/release/"$b" | */target/debug/"$b")
        rm -f "$link"
        say "removed $link"
        ;;
      *) printf 'uninstall.sh: left %s (it points elsewhere: %s)\n' "$link" "$(readlink "$link")" >&2 ;;
    esac
  elif [ -e "$link" ]; then
    printf 'uninstall.sh: left %s (not a link install.sh made)\n' "$link" >&2
  fi
done

# --- Purge: only with the flag, only a directory that looks like Mirante's, only after
# the person types the word.
if [ "$purge" = 1 ]; then
  if [ ! -d "$home" ]; then
    say "nothing to purge: $home does not exist"
  else
    resolved="$(cd "$home" && pwd -P)"
    case "$resolved" in
      / | "$(cd "$HOME" && pwd -P)") die "refusing to delete $resolved" ;;
    esac
    if [ ! -e "$resolved/mirante.sqlite" ] && [ ! -e "$resolved/daemon.log" ] && [ ! -e "$resolved/daemon.lock" ]; then
      die "refusing to delete $resolved: it does not look like a Mirante home (no mirante.sqlite, daemon.log or daemon.lock)"
    fi
    if [ -S "$resolved/daemon.sock" ]; then
      die "a daemon still answers in $resolved: stop it (mirante daemon stop), then run this again"
    fi
    (exec </dev/tty) 2>/dev/null || die "--purge asks for a typed confirmation and needs a terminal"
    {
      printf '\nThis deletes %s: the task database and history, logs, the skill library\n' "$resolved"
      printf 'and its signing key, and the task worktrees in it (uncommitted work there is lost;\n'
      # shellcheck disable=SC2016 # literal backticks
      printf 'the branches stay in your repos; run `git worktree prune` in a repo to forget them).\n'
      printf 'Type "delete" to confirm: '
    } >/dev/tty
    read -r answer </dev/tty || answer=""
    if [ "$answer" != "delete" ]; then
      die "not confirmed: nothing was deleted"
    fi
    rm -rf -- "$resolved"
    say "deleted $resolved"
  fi
else
  say "kept $home (tasks, history, skills); scripts/uninstall.sh --purge deletes it"
fi

cat <<EOF

Done. Not touched: ~/.claude and ~/.codex (Mirante never edits them), your repos and their
.mirante/memory notes, and this checkout (delete it yourself; a built Mirante.app lives in
apps/cockpit/src-tauri/target, and any copy you made in /Applications stays until you remove it).
On macOS, the "Mirante Local Signing" identity scripts/dev-sign.sh made stays in your login
keychain (delete it in Keychain Access if you no longer build Mirante).
EOF
