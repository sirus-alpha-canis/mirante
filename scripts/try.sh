#!/usr/bin/env bash
# A one-minute tour of Mirante without a real agent: no login, no quota, nothing of yours
# touched. It runs a fake `claude` (crates/mirante-cli/tests/fake-claude.sh, the one the
# end-to-end tests use: it echoes each message and asks for a permission when its task's
# goal starts with "ask") against its own daemon, in its own MIRANTE_HOME and a throwaway git
# repo under /tmp. Your ~/.mirante and your running daemon are left alone.
#
#   scripts/try.sh            run the tour, leave its daemon up, print what to try next
#   scripts/try.sh --smoke    run the tour, check it, then stop and delete everything (CI)
#
# Binaries: target/release (after scripts/install.sh), else a debug build made here.
set -euo pipefail

smoke=0
case "${1:-}" in
  "") ;;
  --smoke) smoke=1 ;;
  -h | --help)
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)
    echo "try.sh: unknown option $1 (see --help)" >&2
    exit 1
    ;;
esac

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
fake="$root/crates/mirante-cli/tests/fake-claude.sh"
[ -x "$fake" ] || {
  echo "try.sh: $fake is missing" >&2
  exit 1
}

bin="$root/target/release"
if [ ! -x "$bin/mirante" ] || [ ! -x "$bin/mirante-daemon" ] || [ ! -x "$bin/mirante-hook" ]; then
  for d in /opt/homebrew/opt/rustup/bin "$HOME/.cargo/bin"; do
    if [ -x "$d/rustup" ]; then PATH="$d:$PATH"; fi
  done
  export PATH
  echo "==> building mirante, mirante-daemon, mirante-hook (debug)"
  (cd "$root" && cargo build --locked -q -p mirante-cli -p mirante-daemon -p mirante-hook)
  bin="$root/target/debug"
fi

# Short on purpose: a Unix socket path must fit in about 104 bytes.
dir="$(mktemp -d /tmp/mrt.XXXXXX)"
home="$dir/h"
repo="$dir/demo-repo"

export MIRANTE_HOME="$home"
export MIRANTE_DAEMON_BIN="$bin/mirante-daemon"
export MIRANTE_HOOK_BIN="$bin/mirante-hook"
export MIRANTE_CLAUDE_BIN="$fake"
export FAKE_CLAUDE_LOG="$dir/fake-claude.log"
# The tour must not depend on this machine's memory pressure, nor resume anything later.
export MIRANTE_MEMGUARD=off
export MIRANTE_AUTO_RESUME=off
unset MIRANTE_CLAUDE_INLINE_ANSWERS
m() { "$bin/mirante" "$@"; }

cleanup() {
  m daemon stop >/dev/null 2>&1 || true
  case "$dir" in /tmp/mrt.?*) rm -rf -- "$dir" ;; esac
}
if [ "$smoke" = 1 ]; then trap cleanup EXIT; fi

step() { printf '\n\033[1m$ mirante %s\033[0m\n' "$*"; }

git init -q -b main "$repo"
git -C "$repo" -c user.name=Try -c user.email=try@example.invalid commit -q --allow-empty -m "first commit"

step "new $repo --agent claude \"say hello\""
m new "$repo" --agent claude "say hello"
branch="mirante/say-hello"

sleep 1
step "ls"
m ls

step "say $branch \"what changed?\"   (steer: delivered now, labeled as yours)"
m say "$branch" "what changed?"
sleep 1

# The fake asks for a permission when its task's goal starts with "ask".
asking="mirante/ask-to-touch-a-file"
step "new $repo --agent claude \"ask to touch a file\"   (this one asks for a permission)"
m new "$repo" --agent claude "ask to touch a file"
for _ in $(seq 1 50); do
  if m perms | grep -q "Bash"; then break; fi
  sleep 0.2
done

step "ls"
m ls

step "perms"
m perms

step "show $branch"
m show "$branch"

if [ "$smoke" = 1 ]; then
  m ls | grep -q "$branch" || {
    echo "try.sh: the task is missing from mirante ls" >&2
    exit 1
  }
  m perms | grep -q "Bash" || {
    echo "try.sh: no permission card" >&2
    exit 1
  }
  [ -s "$FAKE_CLAUDE_LOG" ] || {
    echo "try.sh: the fake agent never ran" >&2
    exit 1
  }
  printf '\ntry.sh --smoke: ok\n'
  exit 0
fi

cat >"$dir/env.sh" <<EOF
export MIRANTE_HOME="$home"
export MIRANTE_DAEMON_BIN="$bin/mirante-daemon"
export MIRANTE_HOOK_BIN="$bin/mirante-hook"
export MIRANTE_CLAUDE_BIN="$fake"
export FAKE_CLAUDE_LOG="$dir/fake-claude.log"
export MIRANTE_MEMGUARD=off MIRANTE_AUTO_RESUME=off
export PATH="$bin:\$PATH"
EOF

cat <<EOF

That was a real daemon running a fake agent. What to look at next, in this terminal:

  source $dir/env.sh            # point mirante at the tour's daemon (not your ~/.mirante)
  mirante attach $asking
                                # the agent's live terminal, with its permission dialog:
                                # type 1 to allow it, then Ctrl-] to detach
  mirante allow <perm>          # Claude dialogs are answered in the terminal: it offers to attach
  mirante watch                 # the live board (Ctrl-C quits)
  mirante stop $branch; mirante resume $branch    # back on the same conversation
  mirante archive --stop $branch                  # worktree removed, branch kept

The desktop app against this daemon (from this checkout):
  MIRANTE_HOME=$home pnpm --dir apps/cockpit tauri dev
UI work without any daemon: the scripted fake daemon in apps/cockpit/README.md.

When you are done:
  MIRANTE_HOME=$home $bin/mirante daemon stop && rm -rf $dir
EOF
