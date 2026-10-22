#!/usr/bin/env bash
# Builds, signs and notarizes dist/Annex-mac-arm64.dmg with the Developer ID
# certificate in the keychain. The Release workflow runs it on a macOS runner;
# it also works by hand on a Mac (see docs/releasing.md).
#
# Notarization signs in with the first of these that is set:
#   - an App Store Connect API key: NOTARY_KEY_PATH (the .p8 file),
#     NOTARY_KEY_ID and, for a team key, NOTARY_ISSUER
#   - an app-specific password: NOTARY_APPLE_ID and NOTARY_PASSWORD (the team
#     ID is read from the signing identity)
#   - the keychain profile NOTARY_PROFILE (default "annex"), stored once with
#       xcrun notarytool store-credentials annex --key <.p8> --key-id <id> --issuer <issuer>
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

if [ -n "${NOTARY_KEY_PATH:-}" ]; then
  NOTARY_AUTH=(--key "$NOTARY_KEY_PATH" --key-id "${NOTARY_KEY_ID:?NOTARY_KEY_ID is needed with NOTARY_KEY_PATH}")
  # Team keys need the issuer; individual keys (Xcode 26+) must not send one.
  if [ -n "${NOTARY_ISSUER:-}" ]; then NOTARY_AUTH+=(--issuer "$NOTARY_ISSUER"); fi
elif [ -n "${NOTARY_APPLE_ID:-}" ]; then
  TEAM_ID="$(sed -n 's/.*(\([A-Z0-9]*\))$/\1/p' <<<"$IDENTITY")"
  NOTARY_AUTH=(--apple-id "$NOTARY_APPLE_ID" --password "${NOTARY_PASSWORD:?NOTARY_PASSWORD is needed with NOTARY_APPLE_ID}" --team-id "$TEAM_ID")
else
  NOTARY_AUTH=(--keychain-profile "$PROFILE")
fi

# The CLI runs while the signing certificate is available, so it is one exact
# version, never "the newest 2.x". Keep in step with TAURI_CLI_VERSION in
# .github/workflows/ci.yml.
TAURI_CLI_VERSION="2.12.4"
if [ "$(cargo tauri --version 2>/dev/null)" = "tauri-cli $TAURI_CLI_VERSION" ]; then
  TAURI=(cargo tauri)
elif command -v npx >/dev/null 2>&1; then
  TAURI=(npx --yes "@tauri-apps/cli@$TAURI_CLI_VERSION")
else
  cargo install tauri-cli --version "=$TAURI_CLI_VERSION" --locked
  TAURI=(cargo tauri)
fi

rustup target add aarch64-apple-darwin >/dev/null 2>&1 || true
# Keep the builder's home folder (and so their user name) out of the source
# paths the binary carries for panic messages.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/build"
# Tauri skips the Finder step that lays out the .dmg window (background, icon
# positions) when CI=true; a release needs it.
export TAURI_BUNDLER_DMG_IGNORE_CI=true
# --locked (passed on to cargo): build exactly the dependency versions in Cargo.lock.
(cd "$ROOT/apps/annex-mac-app" && APPLE_SIGNING_IDENTITY="$IDENTITY" "${TAURI[@]}" build --target aarch64-apple-darwin --bundles dmg -- --locked)

DMG="$(ls -t "$ROOT"/target/aarch64-apple-darwin/release/bundle/dmg/*.dmg | head -1)"
OUT="$ROOT/dist/Annex-mac-arm64.dmg"
mkdir -p "$ROOT/dist"
cp "$DMG" "$OUT"

codesign --force --timestamp --sign "$IDENTITY" "$OUT"
xcrun notarytool submit "$OUT" "${NOTARY_AUTH[@]}" --wait
xcrun stapler staple "$OUT"
spctl --assess --type open --context context:primary-signature --verbose "$OUT"
echo "Done: $OUT"
