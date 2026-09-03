#!/usr/bin/env bash
# Assemble target/OpenFlow.app around the native binary, sign it ad hoc, and
# optionally wrap it in a DMG.
#
#   bash scripts/bundle-native.sh            build + bundle + sign
#   bash scripts/bundle-native.sh --dmg      also produce target/OpenFlow.dmg
#   bash scripts/bundle-native.sh --skip-build
#
# The signature matters more than it looks: macOS binds microphone and
# accessibility grants to the signature's designated requirement, and an ad hoc
# one can only name itself by code hash, so every rebuild that changes a byte
# asks for both permissions again. Signed with a certificate the requirement
# names the certificate instead and the grants survive. Run
# scripts/local-signing-identity.sh once to install a local one; set
# OPENFLOW_SIGN_IDENTITY to sign with something else.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP="$ROOT/target/OpenFlow.app"
BINARY="$ROOT/target/release/openflow-native"
IDENTIFIER="io.laisy.openflow"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' crates/openflow-native/Cargo.toml | head -1)"

BUILD=1
DMG=0
for argument in "$@"; do
  case "$argument" in
    --dmg) DMG=1 ;;
    --skip-build) BUILD=0 ;;
    *) echo "Unknown option: $argument" >&2; exit 2 ;;
  esac
done

if [ "$BUILD" = "1" ]; then
  cargo build -p openflow-native --release
fi
if [ ! -x "$BINARY" ]; then
  echo "No binary at $BINARY. Run without --skip-build." >&2
  exit 1
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# The bundle executable keeps the plain name; only the cargo target is suffixed.
cp "$BINARY" "$APP/Contents/MacOS/openflow"
cp "$ROOT/src-tauri/icons/icon.icns" "$APP/Contents/Resources/icon.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Info.plist: the usage strings and LSUIElement come from src-tauri/Info.plist,
# which Tauri merges into its own bundle, so the two builds ask for the same
# permissions with the same words. The bundle keys Tauri generates are added
# here because a hand-assembled bundle has no generator to add them.
USAGE_KEYS="$(/usr/libexec/PlistBuddy -x -c 'Print' "$ROOT/src-tauri/Info.plist" \
  | sed -n '/<dict>/,/<\/dict>/p' | sed '1d;$d')"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>OpenFlow</string>
    <key>CFBundleDisplayName</key>
    <string>OpenFlow</string>
    <key>CFBundleIdentifier</key>
    <string>${IDENTIFIER}</string>
    <key>CFBundleExecutable</key>
    <string>openflow</string>
    <key>CFBundleIconFile</key>
    <string>icon.icns</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
${USAGE_KEYS}
</dict>
</plist>
PLIST

/usr/bin/plutil -lint "$APP/Contents/Info.plist" > /dev/null

# An explicit override first, then the local identity, then ad hoc -- which
# still produces a working bundle, just one whose permissions expire with the
# build. Note the ordering: this never *creates* an identity, because a build
# quietly minting a signing certificate is worse than a build that says why it
# could not.
IDENTITY_NAME="${OPENFLOW_SIGN_IDENTITY-}"
if [ -z "$IDENTITY_NAME" ]; then
  IDENTITY_NAME="$(bash "$ROOT/scripts/local-signing-identity.sh" --check 2>/dev/null || true)"
fi
if [ -z "$IDENTITY_NAME" ]; then
  IDENTITY_NAME="-"
  echo "warning: no signing identity, falling back to ad hoc. Microphone and" >&2
  echo "         accessibility will have to be granted again after this build." >&2
  echo "         Run: bash scripts/local-signing-identity.sh" >&2
fi

codesign --force --deep --sign "$IDENTITY_NAME" \
  --entitlements "$ROOT/src-tauri/entitlements.plist" \
  --options runtime \
  "$APP"

# The requirement is the whole point of the exercise, so say what came out:
# `certificate root` keeps its grants across rebuilds, `cdhash` does not. An ad
# hoc signature has no stored requirement, only the one codesign derives, which
# it marks with a leading "# " -- hence the optional prefix in the match.
codesign --display --requirements - "$APP" 2>/dev/null \
  | sed -n 's/^#\{0,1\} *designated => /signed: /p' >&2

if [ "$DMG" = "1" ]; then
  STAGE="$(mktemp -d)"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  rm -f "$ROOT/target/OpenFlow.dmg"
  hdiutil create -volname OpenFlow -srcfolder "$STAGE" -ov -format UDZO \
    "$ROOT/target/OpenFlow.dmg" > /dev/null
  rm -rf "$STAGE"
  echo "$ROOT/target/OpenFlow.dmg"
fi

echo "$APP"
