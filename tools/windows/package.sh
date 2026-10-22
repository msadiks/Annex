#!/usr/bin/env bash
# Packages a built annex.exe for release:
#   dist/Annex-windows-x64-setup.exe     installer (NSIS)
#   dist/Annex-windows-x64-portable.zip  Annex/Annex.exe + portable.txt
#
# Usage: tools/windows/package.sh [path/to/annex.exe]
# Needs makensis and zip (apt: nsis zip).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXE="$(realpath "${1:-$ROOT/target/x86_64-pc-windows-gnu/release/annex.exe}")"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
DIST="$ROOT/dist"
mkdir -p "$DIST"

makensis -V2 -DVERSION="$VERSION" -DEXE="$EXE" -DOUTFILE="$DIST/Annex-windows-x64-setup.exe" \
  "$ROOT/tools/windows/installer.nsi"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir "$STAGE/Annex"
cp "$EXE" "$STAGE/Annex/Annex.exe"
cp "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$STAGE/Annex/"
cat > "$STAGE/Annex/portable.txt" <<'TXT'
Annex portable

This file keeps Annex in portable mode: its settings, pairing and logs are
stored in the "data" folder next to Annex.exe, so the whole folder can move
between PCs or live on a USB drive. Delete this file to use the normal
per-user folders instead (%APPDATA%\Annex and %LOCALAPPDATA%\Annex).

When Annex first opens, Windows Firewall asks whether it may use the
network. Allow it on private networks, or your Mac can't connect. If you
missed the prompt, the Annex window offers to fix it.
TXT
sed -i 's/$/\r/' "$STAGE/Annex/portable.txt"
rm -f "$DIST/Annex-windows-x64-portable.zip"
(cd "$STAGE" && zip -qr9 "$DIST/Annex-windows-x64-portable.zip" Annex)

echo "Annex $VERSION:"
ls -l "$DIST/Annex-windows-x64-setup.exe" "$DIST/Annex-windows-x64-portable.zip"
