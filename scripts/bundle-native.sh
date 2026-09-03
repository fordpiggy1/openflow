#!/usr/bin/env bash
# Assemble target/OpenFlow.app around the native binary, sign it ad hoc, and
# optionally wrap it in a DMG.
#
#   bash scripts/bundle-native.sh            build + bundle + sign
#   bash scripts/bundle-native.sh --dmg      also produce target/OpenFlow.dmg
#   bash scripts/bundle-native.sh --skip-build
#
# The signature matters more than it looks: macOS binds microphone and
# accessibility grants to a code signature, so an unsigned rebuild asks for both
# permissions again. Ad hoc signing at least keeps one build's grants stable;
# Milestone C swaps in a real identity so they survive rebuilds.
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

codesign --force --deep --sign - \
  --entitlements "$ROOT/src-tauri/entitlements.plist" \
  --options runtime \
  "$APP" 2>/dev/null

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
