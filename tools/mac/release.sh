#!/usr/bin/env bash
# Builds, signs and notarizes dist/Annex-mac-arm64.dmg on a Mac with the
# Developer ID certificate in the login keychain. Credentials never leave the
# Mac; only the .dmg is uploaded to Apple for notarization.
#
# One-time setup (stores an app-specific password in the keychain):
#   xcrun notarytool store-credentials annex --apple-id <apple id> --team-id <team id>
#
# Usage: tools/mac/release.sh ["Developer ID Application: Name (TEAMID)"]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROFILE="${NOTARY_PROFILE:-annex}"
IDENTITY="${1:-${APPLE_SIGNING_IDENTITY:-}}"
if [ -z "$IDENTITY" ]; then
  IDENTITY="$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | head -1)"
fi
if [ -z "$IDENTITY" ]; then
  echo "No Developer ID Application certificate in the keychain." >&2
  exit 1
fi
echo "Signing as: $IDENTITY"

if cargo tauri --version >/dev/null 2>&1; then
  TAURI=(cargo tauri)
elif command -v npx >/dev/null 2>&1; then
  TAURI=(npx --yes @tauri-apps/cli@2)
else
  cargo install tauri-cli --version "^2" --locked
  TAURI=(cargo tauri)
fi

rustup target add aarch64-apple-darwin >/dev/null 2>&1 || true
# Keep the builder's home folder (and so their user name) out of the source
# paths the binary carries for panic messages.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build"
(cd "$ROOT/apps/annex-mac-app" && APPLE_SIGNING_IDENTITY="$IDENTITY" "${TAURI[@]}" build --target aarch64-apple-darwin --bundles dmg)

DMG="$(ls -t "$ROOT"/target/aarch64-apple-darwin/release/bundle/dmg/*.dmg | head -1)"
OUT="$ROOT/dist/Annex-mac-arm64.dmg"
mkdir -p "$ROOT/dist"
cp "$DMG" "$OUT"

codesign --force --timestamp --sign "$IDENTITY" "$OUT"
xcrun notarytool submit "$OUT" --keychain-profile "$PROFILE" --wait
xcrun stapler staple "$OUT"
spctl --assess --type open --context context:primary-signature --verbose "$OUT"
echo "Done: $OUT"
