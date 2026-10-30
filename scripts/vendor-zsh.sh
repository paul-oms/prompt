#!/usr/bin/env bash
# Copy an installed zsh into vendor/zsh so Prompt can ship it next to the
# executable. Prompt detects vendor/zsh/bin/zsh and fixes up module/function
# paths at runtime.
#
# Usage: scripts/vendor-zsh.sh [/usr]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="${1:-/usr}"

if [[ ! -x "$PREFIX/bin/zsh" ]]; then
  echo "no zsh at $PREFIX/bin/zsh" >&2
  exit 1
fi

DEST="$ROOT/vendor/zsh"
rm -rf "$DEST"
mkdir -p "$DEST/bin"
cp -a "$PREFIX/bin/zsh" "$DEST/bin/zsh"

for dir in lib/zsh share/zsh; do
  if [[ -d "$PREFIX/$dir" ]]; then
    mkdir -p "$DEST/$(dirname "$dir")"
    cp -a "$PREFIX/$dir" "$DEST/$dir"
  fi
done

for license in \
  "$PREFIX/share/licenses/zsh/LICENCE" \
  "$PREFIX/share/licenses/zsh/LICENSE" \
  "$PREFIX/share/doc/zsh/LICENCE" \
  "$PREFIX/share/doc/zsh/LICENSE"; do
  if [[ -f "$license" ]]; then
    cp "$license" "$DEST/LICENCE"
    break
  fi
done

cat > "$DEST/README" <<'EOF'
zsh bundled with Prompt.

Installed by scripts/vendor-zsh.sh. zsh remains under its own licence; see
LICENCE if present. Prompt launches this binary when it is present and uses
the lib/zsh and share/zsh trees for modules and functions.

Note: a binary copied from the build machine may depend on that machine's
libc. For redistributable releases, build zsh against a conservative target
(or statically) instead of copying the host's binary.
EOF

echo "Bundled zsh into $DEST"
"$DEST/bin/zsh" --version || true
