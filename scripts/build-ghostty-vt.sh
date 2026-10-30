#!/usr/bin/env bash
# Build Ghostty's embeddable VT C library for the host platform and vendor it
# into vendor/ghostty-vt. Requires network access; Zig is downloaded into the
# build cache, not installed system-wide.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ZIG_VERSION="${ZIG_VERSION:-0.17.0}"
GHOSTTY_REF="${GHOSTTY_REF:-main}"
CACHE="${PROMPT_BUILD_CACHE:-${TMPDIR:-/tmp}/prompt-ghostty-build}"

uname_s="$(uname -s)"
uname_m="$(uname -m)"
case "$uname_s/$uname_m" in
  Linux/x86_64)   ZIG_TARGET="x86_64-linux" ;;
  Linux/aarch64|Linux/arm64) ZIG_TARGET="aarch64-linux" ;;
  Darwin/x86_64)  ZIG_TARGET="x86_64-macos" ;;
  Darwin/arm64)   ZIG_TARGET="aarch64-macos" ;;
  *)
    echo "unsupported platform: $uname_s/$uname_m" >&2
    exit 1
    ;;
esac

mkdir -p "$CACHE"
ZIG_DIR="$CACHE/zig-$ZIG_TARGET-$ZIG_VERSION"
if [[ ! -x "$ZIG_DIR/zig" ]]; then
  echo "==> downloading Zig $ZIG_VERSION for $ZIG_TARGET"
  rm -rf "$ZIG_DIR"
  curl -fsSL "https://ziglang.org/download/$ZIG_VERSION/zig-$ZIG_TARGET-$ZIG_VERSION.tar.xz" \
    | tar -xJ -C "$CACHE"
fi
ZIG="$ZIG_DIR/zig"

SRC="$CACHE/ghostty-$GHOSTTY_REF"
if [[ ! -f "$SRC/build.zig" ]]; then
  echo "==> downloading Ghostty ($GHOSTTY_REF)"
  rm -rf "$SRC" "$CACHE/ghostty-src"
  mkdir -p "$CACHE/ghostty-src"
  curl -fsSL "https://github.com/ghostty-org/ghostty/archive/refs/heads/$GHOSTTY_REF.tar.gz" \
    | tar -xz -C "$CACHE/ghostty-src" --strip-components=1
  mv "$CACHE/ghostty-src" "$SRC"
fi

echo "==> building libghostty-vt"
(
  cd "$SRC"
  "$ZIG" build -Demit-lib-vt -Doptimize=ReleaseFast
)

echo "==> installing into vendor/ghostty-vt"
rm -rf "$ROOT/vendor/ghostty-vt/include" "$ROOT/vendor/ghostty-vt/lib"
cp -R "$SRC/zig-out/include" "$ROOT/vendor/ghostty-vt/include"
mkdir -p "$ROOT/vendor/ghostty-vt/lib"
cp "$SRC/zig-out/lib/libghostty-vt.a" "$ROOT/vendor/ghostty-vt/lib/"
cp "$SRC/LICENSE" "$ROOT/vendor/ghostty-vt/LICENSE"
{
  echo "Ghostty ref: $GHOSTTY_REF"
  echo "Zig: $ZIG_VERSION"
  echo "Built: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "Host: $uname_s/$uname_m"
} > "$ROOT/vendor/ghostty-vt/BUILD_INFO.txt"

echo
echo "Done. vendor/ghostty-vt is ready."
