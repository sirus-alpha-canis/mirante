#!/usr/bin/env bash
# Creates a throwaway git repo for spikes. Prints its path.
set -euo pipefail
dir="${1:-$HOME/.mirante-spike/sandbox}"
rm -rf "$dir"
mkdir -p "$dir"
cd "$dir"
git init -q
echo "# mirante spike sandbox" > README.md
git add README.md
git -c user.email=spike@local -c user.name=spike commit -qm init
echo "$dir"
