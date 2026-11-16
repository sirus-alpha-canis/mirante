#!/usr/bin/env bash
# CI only: turns a `cargo test` log into GitHub annotations, so a failing test's name and
# panic are visible on the run's public page (its logs need a sign-in).
#   scripts/ci-test-annotations.sh <cargo-test.log> [error|warning]
set -euo pipefail

log="${1:?usage: ci-test-annotations.sh <cargo-test.log> [error|warning]}"
level="${2:-error}"
[ -f "$log" ] || { echo "no test log at $log"; exit 0; }

# Escapes for a workflow command's message (%, CR, LF), and a cap on its length; a
# property (the title) also escapes ':' and ','.
esc() { local s="${1:0:900}"; s="${s//'%'/%25}"; s="${s//$'\r'/%0D}"; printf '%s' "${s//$'\n'/%0A}"; }
prop() { local s; s="$(esc "$1")"; s="${s//:/%3A}"; printf '%s' "${s//,/%2C}"; }

name="" where="" n=0
while IFS= read -r line; do
  line="${line%$'\r'}"
  if [[ "$line" =~ ^----\ (.+)\ stdout\ ----$ ]]; then
    name="${BASH_REMATCH[1]}" where=""
  elif [[ -n "$name" && -z "$where" && "$line" == *"panicked at "* ]]; then
    where="${line##*panicked at }"
  elif [[ -n "$where" ]]; then
    # The line after "panicked at" is the panic's message.
    echo "::$level title=$(prop "$name")::$(esc "$where $line")"
    n=$((n + 1)) name="" where=""
  elif [[ "$line" == "error: test failed"* || "$line" == *"(signal: "* ]]; then
    echo "::$level title=cargo test::$(esc "$line")"
    n=$((n + 1))
  fi
done < <(sed -e $'s/\x1b\\[[0-9;]*m//g' "$log") # without colours (CARGO_TERM_COLOR=always)
[ "$n" -gt 0 ] || echo "::$level title=cargo test::failed, but no failing test was found in the log"
