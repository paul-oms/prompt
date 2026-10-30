#!/usr/bin/env bash
# Package a release build as a macOS .app and compressed .dmg.
#
# Environment:
#   VERSION        Release version, e.g. 0.0.1 (required)
#   MACOS_VERSION  Minimum macOS major version, e.g. 26 or 27 (required)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-}"
MACOS_VERSION="${MACOS_VERSION:-}"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "VERSION must look like 0.0.1 (got '${VERSION}')" >&2
  exit 2
fi
if [[ ! "$MACOS_VERSION" =~ ^[0-9]+$ ]]; then
  echo "MACOS_VERSION must be a major version such as 26 (got '${MACOS_VERSION}')" >&2
  exit 2
fi

BIN="$ROOT/target/release/prompt"
if [[ ! -x "$BIN" ]]; then
  echo "missing release binary: $BIN" >&2
  echo "run: cargo build --release --locked" >&2
  exit 1
fi

ARCH="$(uname -m)"
case "$ARCH" in
  arm64|x86_64) ;;
  *)
    echo "unsupported macOS architecture: $ARCH" >&2
    exit 1
    ;;
esac

OUT="$ROOT/target/macos"
APP="$OUT/Prompt.app"
DMG="$OUT/Prompt-macos-$MACOS_VERSION-$ARCH.dmg"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/prompt-dmg.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

rm -rf "$APP" "$DMG" "$DMG.sha256"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

install -m 0755 "$BIN" "$APP/Contents/MacOS/prompt"
install -m 0644 "$ROOT/LICENSE" "$APP/Contents/Resources/LICENSE"
install -m 0644 "$ROOT/THIRD_PARTY.md" "$APP/Contents/Resources/THIRD_PARTY.md"
install -m 0644 "$ROOT/vendor/ghostty-vt/LICENSE" "$APP/Contents/Resources/GHOSTTY-LICENSE"

sed \
  -e "s/__VERSION__/$VERSION/g" \
  -e "s/__MACOS_VERSION__/$MACOS_VERSION/g" \
  "$ROOT/packaging/macos/Info.plist" > "$APP/Contents/Info.plist"
chmod 0644 "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# There is no Developer ID certificate in CI yet. Ad-hoc signing keeps the
# bundle internally consistent; users still need to approve the first launch
# because the DMG is not notarized.
codesign --force --sign - "$APP"
codesign --verify --deep --strict "$APP"

# Include the usual drag-to-Applications shortcut in the disk image.
ditto "$APP" "$STAGE/Prompt.app"
ln -s /Applications "$STAGE/Applications"

hdiutil create \
  -volname "Prompt $VERSION" \
  -srcfolder "$STAGE" \
  -ov \
  -format UDZO \
  "$DMG"
hdiutil verify "$DMG"

(
  cd "$OUT"
  shasum -a 256 "$(basename "$DMG")" > "$(basename "$DMG").sha256"
)

echo "Created $DMG"
echo "Created $DMG.sha256"
