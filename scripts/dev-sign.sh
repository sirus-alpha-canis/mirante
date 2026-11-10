#!/usr/bin/env bash
# Signs a local Mirante build (macOS) with a stable self-signed identity, so macOS privacy
# permissions (TCC: "would like to access files in your Documents folder") are remembered
# across rebuilds. An unsigned (ad-hoc, linker-signed) build is identified by its cdhash,
# which changes on every build: each rebuild looks like a new app and asks again.
#
#   scripts/dev-sign.sh [--check] [<path>...]
#
#   <path>...  what to sign (default: the four binaries in target/release and
#              apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app, those present)
#   --check    only say whether the identity exists and how each path is signed now
#
# What it does, once (when no identity named "Mirante Local Signing" is in the keychain):
#   1. makes an RSA-2048 key and a self-signed certificate (CN=Mirante Local Signing,
#      extendedKeyUsage=codeSigning, 10 years) with /usr/bin/openssl in a private temp dir;
#   2. imports both into your login keychain with `security import -T /usr/bin/codesign`
#      (only codesign may use the key without asking), then deletes the temp files.
# It never changes trust settings (codesign signs with an untrusted certificate, and TCC
# only needs the signature to stay the same) and never touches another identity. Making the
# identity asks nothing (unless the login keychain is locked). The first signing does: macOS
# asks whether codesign may use the key "Mirante Local Signing"; type your login password
# and click Always Allow, once.
# Every run then signs each path with that identity (`codesign --force --options runtime`):
# the designated requirement becomes `identifier "<id>" and certificate leaf = H"<sha1>"`,
# the same on every rebuild.
#
# Undo: in Keychain Access (login keychain, My Certificates) delete "Mirante Local Signing"
# (the certificate and its key);
# `cargo build` / `tauri build` produce ad-hoc signed files again.
#
# Environment: MIRANTE_SIGN_IDENTITY (default "Mirante Local Signing"),
# MIRANTE_SIGN_KEYCHAIN (default: the login keychain).
set -euo pipefail

say() { printf '==> %s\n' "$*"; }
die() {
  printf 'dev-sign.sh: %s\n' "$*" >&2
  exit 1
}
usage() { sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'; }

[ "$(uname -s)" = Darwin ] || die "code signing is macOS-only"

name="${MIRANTE_SIGN_IDENTITY:-Mirante Local Signing}"
keychain="${MIRANTE_SIGN_KEYCHAIN:-$HOME/Library/Keychains/login.keychain-db}"
check=0
paths=()
while [ $# -gt 0 ]; do
  case "$1" in
    --check)
      check=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*) die "unknown option $1 (see --help)" ;;
    *)
      paths+=("$1")
      shift
      ;;
  esac
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
BINS=(mirante mirante-daemon mirante-hook mirante-mcp)
APP="$root/apps/cockpit/src-tauri/target/release/bundle/macos/Mirante.app"
if [ ${#paths[@]} -eq 0 ]; then
  for b in "${BINS[@]}"; do
    [ -x "$root/target/release/$b" ] && paths+=("$root/target/release/$b")
  done
  [ -d "$APP" ] && paths+=("$APP")
fi

# The SHA-1 of the identity named $name (untrusted ones included), empty when none.
identity_hash() {
  security find-identity -p codesigning "$keychain" 2>/dev/null |
    awk -v n="\"$name\"" '$1 ~ /^[0-9]+\)$/ && index($0, n) { print $2; exit }'
}

create_identity() {
  if security find-certificate -c "$name" "$keychain" >/dev/null 2>&1; then
    die "the keychain has a certificate named \"$name\" without its private key; delete it in Keychain Access, then run this again"
  fi
  say "Creating the code-signing identity \"$name\" in $keychain (once)"
  local tmp pass
  tmp="$(mktemp -d)"
  chmod 700 "$tmp"
  # shellcheck disable=SC2064 # expand $tmp now: it is local
  trap "rm -rf '$tmp'" EXIT
  cat >"$tmp/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
EOF
  # /usr/bin/openssl (LibreSSL) writes a PKCS#12 that every macOS `security` imports.
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$tmp/cert.cnf" \
    -keyout "$tmp/key.pem" -out "$tmp/cert.pem" 2>/dev/null || die "openssl could not make the certificate"
  pass="$(/usr/bin/openssl rand -hex 16)"
  /usr/bin/openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" -name "$name" \
    -out "$tmp/id.p12" -passout "pass:$pass" || die "openssl could not package the identity"
  security import "$tmp/id.p12" -k "$keychain" -f pkcs12 -P "$pass" -T /usr/bin/codesign >/dev/null ||
    die "security could not import the identity into $keychain"
  rm -rf "$tmp"
  trap - EXIT
}

describe() {
  local p="$1" dr
  dr="$(codesign -d -r- "$p" 2>&1 | sed -n 's/^.*designated => //p')"
  printf '    %s\n      designated => %s\n' "$p" "${dr:-unsigned}"
}

hash="$(identity_hash)"
if [ "$check" = 1 ]; then
  if [ -n "$hash" ]; then
    say "identity \"$name\": $hash"
  else
    say "identity \"$name\": none (scripts/dev-sign.sh creates it)"
  fi
  for p in "${paths[@]}"; do describe "$p"; done
  exit 0
fi

if [ -z "$hash" ]; then
  create_identity
  hash="$(identity_hash)"
  [ -n "$hash" ] || die "the identity \"$name\" is still not found after the import"
fi
[ ${#paths[@]} -gt 0 ] || die "nothing to sign: build first (cargo build --release; scripts/install.sh --app)"

# codesign looks identities up in the keychain search list; say so when $keychain is not on it.
case "$(security list-keychains -d user)" in
  *"$keychain"*) ;;
  *) die "$keychain is not on your keychain search list (security list-keychains -d user)" ;;
esac

say "Signing with \"$name\" ($hash)"
say "(the first time, macOS asks whether codesign may use this key: login password, then Always Allow)"
for p in "${paths[@]}"; do
  [ -e "$p" ] || die "$p does not exist"
  if [ -d "$p" ]; then
    # An app bundle: its identifier is CFBundleIdentifier. WebKit's JIT runs in WebKit's own
    # processes, so the hardened runtime needs no entitlement here.
    codesign --force --deep --options runtime --timestamp=none --sign "$hash" "$p"
  else
    # A bare binary: name it, or codesign keeps the linker's per-build identifier. Signed as
    # a copy, then moved over: a daemon running from the old file keeps its own (inode).
    cp -p "$p" "$p.signing"
    codesign --force --options runtime --timestamp=none --identifier "dev.mirante.$(basename "$p")" \
      --sign "$hash" "$p.signing" || {
      rm -f "$p.signing"
      die "codesign failed on $p"
    }
    mv -f "$p.signing" "$p"
  fi
  codesign --verify --strict "$p" || die "$p does not verify after signing"
  describe "$p"
done
