#!/usr/bin/env bash
# Exercise the real imported-inventory Keychain item across process restarts.
# Requires a signing identity authorised by embedded.provisionprofile.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
ENTITLEMENTS="$REPO_ROOT/contrib/release/macos/coincube.entitlements"
PROFILE="$REPO_ROOT/contrib/release/macos/embedded.provisionprofile"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/imported-anchor-native.XXXXXX")"
BUNDLE="$WORK/Probe.app"
RUN_ID="$(uuidgen | tr '[:upper:]' '[:lower:]')"
cleanup() {
    if [ -x "$BUNDLE/Contents/MacOS/Probe" ]; then
        "$BUNDLE/Contents/MacOS/Probe" delete "$RUN_ID" || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

test -f "$PROFILE" || { echo "missing embedded profile" >&2; exit 1; }
test -f "$ENTITLEMENTS" || { echo "missing release entitlements" >&2; exit 1; }
test -n "${SIGN_IDENTITY:-}" || { echo "set SIGN_IDENTITY to the profile-authorised signing identity" >&2; exit 1; }

"$REPO_ROOT/contrib/release/macos/check-profile-authorises-cert.sh" \
    "$PROFILE" --identity "$SIGN_IDENTITY"

cd "$REPO_ROOT"
cargo build -p coincube-gui --example imported_anchor_native --locked
mkdir -p "$BUNDLE/Contents/MacOS"
BUILD_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
cp "$BUILD_TARGET_DIR/debug/examples/imported_anchor_native" "$BUNDLE/Contents/MacOS/Probe"
cp "$PROFILE" "$BUNDLE/Contents/embedded.provisionprofile"
cat > "$BUNDLE/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>io.coincube.tenshu</string>
  <key>CFBundleExecutable</key><string>Probe</string>
  <key>CFBundleName</key><string>Imported Anchor Probe</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>1.0</string>
</dict></plist>
PLIST
codesign -f -s "$SIGN_IDENTITY" --options runtime --entitlements "$ENTITLEMENTS" "$BUNDLE"
codesign --verify --strict "$BUNDLE"
"$REPO_ROOT/contrib/release/macos/check-profile-authorises-cert.sh" \
    "$PROFILE" --signed-bundle "$BUNDLE"
codesign -d --entitlements - "$BUNDLE" 2>&1 | grep -q '8UVR249AD5.io.coincube.tenshu'

# Every invocation is a fresh process. Stopping after each durable transition
# probes read-back and restart; sudden power loss still needs a separate test.
for phase in create pending commit clear verify; do
    "$BUNDLE/Contents/MacOS/Probe" "$phase" "$RUN_ID"
done
