#!/usr/bin/env bash
# Creates a throwaway git repo for spikes. Prints its path.
# Guarded: only ever deletes a target under $HOME/.mirante-spike/ or a temp dir,
# and only if it carries the .mirante-sandbox marker (or is the legacy default sandbox).
set -euo pipefail
dir="${1:-$HOME/.mirante-spike/sandbox}"

refuse() {
  echo "make-sandbox: refusing to use '$dir': $1" >&2
  exit 1
}

[ -n "$dir" ] || refuse "empty path"
case "$dir" in /*) ;; *) dir="$PWD/$dir" ;; esac
dir="${dir%/}"
[ -n "$dir" ] || refuse "resolves to /"
case "/$dir/" in */../* | */./*) refuse "contains '.' or '..' components" ;; esac

# Resolve symlinks in the parent (the target itself may not exist yet).
parent="$(dirname "$dir")"
name="$(basename "$dir")"
if [ -d "$parent" ]; then
  parent="$(cd "$parent" && pwd -P)"
fi
resolved="${parent%/}/$name"
spike_root="$HOME/.mirante-spike"
if [ -d "$spike_root" ]; then
  spike_root="$(cd "$spike_root" && pwd -P)"
fi

case "$resolved" in
  "$spike_root"/?* | /tmp/?* | /private/tmp/?* | /var/folders/?* | /private/var/folders/?*) ;;
  *) refuse "resolved path '$resolved' is not under $spike_root/ or a temp dir" ;;
esac

# Only ever wipe a directory this script created (it carries the marker file).
# Exception: the default sandbox predates the marker (Task 8) and is ours.
if [ -e "$resolved" ] && [ ! -f "$resolved/.mirante-sandbox" ]; then
  if [ "$resolved" != "$spike_root/sandbox" ]; then
    refuse "'$resolved' exists but has no .mirante-sandbox marker (not created by this script)"
  fi
  echo "make-sandbox: legacy default sandbox without marker, recreating it" >&2
fi

rm -rf "$resolved"
mkdir -p "$resolved"
cd "$resolved"
echo "created by mirante scripts/make-sandbox.sh" > .mirante-sandbox
git init -q
echo ".mirante-sandbox" >> .git/info/exclude
echo "# mirante spike sandbox" > README.md
git add README.md
git -c user.email=spike@local -c user.name=spike commit -qm init
echo "$resolved"
