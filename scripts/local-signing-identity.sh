#!/usr/bin/env bash
# Create, unlock, and report the local code signing identity the native bundle
# is signed with.
#
#   bash scripts/local-signing-identity.sh            create if absent, then print it
#   bash scripts/local-signing-identity.sh --check    print it if usable; never create
#   bash scripts/local-signing-identity.sh --remove    delete the keychain and the password
#
# Why this exists at all: macOS records a TCC grant against the *designated
# requirement* of whatever asked for it, and an ad hoc signature has nothing
# durable to name itself by, so its requirement is the code hash:
#
#     designated => cdhash H"777e36…"
#
# Change one line of Rust and that hash changes, so the microphone and
# accessibility grants stop matching and the app is silently back to having
# neither. Signing with a certificate -- any certificate, it need not be
# Apple's -- moves the requirement onto the certificate instead:
#
#     designated => identifier "io.laisy.openflow" and certificate root = H"…"
#
# which every later build satisfies. The certificate is self-signed and lives
# only on this machine; it asserts nothing to anyone else and is not a
# substitute for a Developer ID when the app is eventually distributed. It only
# has to stay the same from one build to the next.
#
# It deliberately does not touch trust settings. Under the code signing policy
# an untrusted self-signed certificate is already a valid signing identity, and
# writing trust settings is the one step here that would raise a GUI
# authorisation prompt.
set -euo pipefail

KEYCHAIN="$HOME/Library/Keychains/openflow-signing.keychain-db"
SECRET_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/openflow"
SECRET="$SECRET_DIR/signing-keychain-password"
IDENTITY="OpenFlow Local Signing"

MODE=create
case "${1-}" in
  "") ;;
  --check) MODE=check ;;
  --remove) MODE=remove ;;
  *) echo "Unknown option: $1" >&2; exit 2 ;;
esac

if [ "$MODE" = "remove" ]; then
  # Drop it from the search list first, or every later `security` call warns
  # about a keychain that is no longer there.
  remaining="$(security list-keychains -d user | tr -d ' "' | grep -v "^${KEYCHAIN}$" || true)"
  # shellcheck disable=SC2086
  [ -n "$remaining" ] && security list-keychains -d user -s $remaining
  security delete-keychain "$KEYCHAIN" 2>/dev/null || true
  rm -f "$SECRET"
  echo "Removed $KEYCHAIN. The next bundle falls back to ad hoc signing." >&2
  exit 0
fi

# A keychain whose password is not the login password is not unlocked at login,
# so an existing identity still needs unlocking on the first build after a boot.
# That is not a reason to refuse under --check: only *creating* is the step a
# build should never take on its own.
if [ -f "$KEYCHAIN" ] && [ -f "$SECRET" ]; then
  security unlock-keychain -p "$(cat "$SECRET")" "$KEYCHAIN"
elif [ "$MODE" = "check" ]; then
  exit 1
fi

# Not `find-identity -v`: its validity filter runs a trust evaluation, which a
# self-signed certificate fails by definition, so -v reports zero identities
# for one codesign signs with perfectly happily. The unfiltered list, and the
# CSSMERR_TP_NOT_TRUSTED it prints beside the name, is the honest answer.
if ! security find-identity -p codesigning "$KEYCHAIN" 2>/dev/null | grep -qF "$IDENTITY"; then
  if [ "$MODE" = "check" ]; then
    exit 1
  fi

  # Start from nothing rather than repairing a half-made keychain: everything
  # here is regenerable, and the only cost of a fresh certificate is granting
  # microphone and accessibility once more.
  security delete-keychain "$KEYCHAIN" 2>/dev/null || true

  mkdir -p "$SECRET_DIR"
  chmod 700 "$SECRET_DIR"
  # The password guards a certificate that can only sign local builds, but it
  # is still a key, so it stays out of the repository and off the process
  # command line, readable by this user alone.
  umask 077
  # Not `tr -dc ... | head -c`: head closes the pipe first and pipefail turns
  # tr's SIGPIPE into a silent early exit from this script.
  /usr/bin/openssl rand -base64 24 | tr -d '\n/+=' > "$SECRET"

  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
  password="$(cat "$SECRET")"

  cat > "$WORK/codesign.cnf" <<'CNF'
[ req ]
distinguished_name = dn
prompt = no
x509_extensions = v3_codesign

[ dn ]
CN = OpenFlow Local Signing
O  = OpenFlow

[ v3_codesign ]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
CNF

  # /usr/bin/openssl is LibreSSL, whose PKCS#12 output `security import` reads
  # without the -legacy dance a Homebrew OpenSSL 3 would need.
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -config "$WORK/codesign.cnf" -keyout "$WORK/key.pem" -out "$WORK/cert.pem" 2>/dev/null
  /usr/bin/openssl pkcs12 -export -out "$WORK/identity.p12" \
    -inkey "$WORK/key.pem" -in "$WORK/cert.pem" -name "$IDENTITY" -passout "pass:$password"

  security create-keychain -p "$password" "$KEYCHAIN"
  security set-keychain-settings "$KEYCHAIN"   # no idle timeout, no lock on sleep
  security unlock-keychain -p "$password" "$KEYCHAIN"
  security import "$WORK/identity.p12" -k "$KEYCHAIN" -P "$password" \
    -T /usr/bin/codesign -T /usr/bin/security > /dev/null
  # Without this codesign reaches the key through the ACL and macOS raises the
  # "wants to sign using key" panel on every single build.
  security set-key-partition-list -S apple-tool:,apple:,codesign: \
    -s -k "$password" "$KEYCHAIN" > /dev/null 2>&1
fi

# codesign resolves an identity by name through the search list, not from the
# keychain file, so the keychain has to be in it -- appended, never replacing.
if ! security list-keychains -d user | tr -d ' "' | grep -qx "$KEYCHAIN"; then
  # shellcheck disable=SC2046
  security list-keychains -d user -s $(security list-keychains -d user | tr -d ' "') "$KEYCHAIN"
fi

echo "$IDENTITY"
