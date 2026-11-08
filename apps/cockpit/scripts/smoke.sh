#!/usr/bin/env bash
# Smoke test of the built app against the scripted fake daemon: the app must start, its
# frontend must subscribe through the bridge, receive the snapshot over the Tauri channel
# (proved by the follow-up calls the store makes only after a snapshot arrives) and poll
# daemon.status. Opens the app window for a few seconds. macOS; needs a prior
# `pnpm tauri build --debug --bundles app` and `cargo build -p mirante-bridge --features fake`.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(cd "$here/../.." && pwd)"
app="$here/src-tauri/target/debug/bundle/macos/Mirante.app/Contents/MacOS/mirante-cockpit"
fake="${CARGO_TARGET_DIR:-$root/target}/debug/mirante-fake-daemon"
[ -x "$app" ] || { echo "missing $app: run pnpm tauri build --debug --bundles app"; exit 1; }
[ -x "$fake" ] || { echo "missing $fake: run cargo build -p mirante-bridge --features fake"; exit 1; }
home="$(mktemp -d /tmp/mr-smoke-XXXX)"
log="$home/fake.log"
"$fake" "$home/daemon.sock" >"$log" 2>&1 &
fake_pid=$!
MIRANTE_HOME="$home" "$app" >"$home/app.log" 2>&1 &
app_pid=$!
cleanup() {
  kill "$app_pid" "$fake_pid" 2>/dev/null || true
  wait "$app_pid" "$fake_pid" 2>/dev/null || true
  rm -rf "$home"
}
trap cleanup EXIT
ok=0
for _ in $(seq 1 60); do
  if grep -q "call events.subscribe" "$log" && grep -q "call permission.list" "$log" \
    && grep -q "call task.events" "$log" && grep -q "call daemon.status" "$log"; then
    ok=1
    break
  fi
  sleep 0.5
done
sort "$log" | uniq -c
if [ "$ok" = 1 ]; then echo "SMOKE PASS"; else echo "SMOKE FAIL"; cat "$home/app.log"; exit 1; fi
