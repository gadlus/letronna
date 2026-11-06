#!/usr/bin/env bash
# Wraps the release binary in target/Letronna.app. macOS reads the icon, the
# display name and the menu bar title from a bundle, so a bare executable shows
# a generic icon and a lowercase name however it is launched. Run after
# `cargo build --release`. Pass --dmg to also package it for download.
#
# MACOS_SIGN_IDENTITY   a "Developer ID Application" identity to sign with
# MACOS_NOTARY_PROFILE  a `notarytool store-credentials` profile; with it the
#                       app, the disk image and the terminal are notarized
set -euo pipefail
cd "$(dirname "$0")/.."
bin=target/release/letronna-gui
cli=target/release/letronna
[ -x "$bin" ] || { echo "build it first: cargo build --release" >&2; exit 1; }
if [ -n "${MACOS_NOTARY_PROFILE-}" ] && [ -z "${MACOS_SIGN_IDENTITY-}" ]; then
  echo "MACOS_NOTARY_PROFILE needs MACOS_SIGN_IDENTITY: Apple notarizes only Developer ID code" >&2
  exit 1
fi
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

app=target/Letronna.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/Letronna"

# icon.ico is the same artwork Windows uses, with the rounded corners already
# baked in, which is what macOS expects too. Its largest layer is 256px, so the
# bigger slots are upscaled; they only show in Finder at large icon sizes.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sips -s format png crates/gui/assets/icon.ico --out "$tmp/icon.png" >/dev/null
mkdir "$tmp/Letronna.iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size "$tmp/icon.png" --out "$tmp/Letronna.iconset/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) "$tmp/icon.png" \
    --out "$tmp/Letronna.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$tmp/Letronna.iconset" -o "$app/Contents/Resources/Letronna.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Letronna</string>
  <key>CFBundleDisplayName</key><string>Letronna</string>
  <key>CFBundleExecutable</key><string>Letronna</string>
  <key>CFBundleIdentifier</key><string>com.gadlus.letronna</string>
  <key>CFBundleIconFile</key><string>Letronna</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>10.15.7</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Letronna listens only while you record a message for it to type out.</string>
  <key>NSAppleEventsUsageDescription</key><string>A command Letronna runs for you is asking to control this app.</string>
</dict>
</plist>
PLIST

# Apple checks signed code and vouches for it, which is what stops Gatekeeper
# saying it "could not verify Letronna is free of malware". The release
# workflow notarizes the disk image with an App Store Connect key; a release
# built on a Mac names a keychain profile instead (see CONTRIBUTING). Ask
# for the log when it fails: it names the file Apple refused and why.
notarize() {
  local out status id
  out=$(xcrun notarytool submit "$1" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait --output-format json) || true
  status=$(printf '%s' "$out" | plutil -extract status raw -o - - 2>/dev/null) || status=unknown
  if [ "$status" != Accepted ]; then
    echo "Apple did not notarize $1: $status" >&2
    id=$(printf '%s' "$out" | plutil -extract id raw -o - - 2>/dev/null) || id=
    [ -z "$id" ] || xcrun notarytool log "$id" --keychain-profile "$MACOS_NOTARY_PROFILE" >&2 || true
    exit 1
  fi
}

# Signed with a Developer ID when MACOS_SIGN_IDENTITY names one, with the
# hardened runtime notarization asks for. Otherwise ad-hoc: an unsigned
# bundle has no stable identity, so macOS can keep serving a stale icon from
# its cache, and ad-hoc is enough for that. Under the hardened runtime the
# microphone stays silent without its entitlement, so voice input would
# upload nothing but silence, and a command the app runs (osascript, say)
# could not drive another app without the Apple Events one.
if [ -n "${MACOS_SIGN_IDENTITY-}" ]; then
  cat > "$tmp/entitlements.plist" <<ENT
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.device.audio-input</key><true/>
  <key>com.apple.security.automation.apple-events</key><true/>
</dict>
</plist>
ENT
  codesign --force --options runtime --timestamp --entitlements "$tmp/entitlements.plist" \
    --sign "$MACOS_SIGN_IDENTITY" "$app"
  # The terminal is downloaded on its own too, so it carries the same name.
  if [ -x "$cli" ]; then
    codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$cli"
  fi
else
  codesign --force --sign - "$app" >/dev/null 2>&1 || true
fi

# The app is notarized on its own and its ticket stapled before it goes in
# the disk image, so a copy dragged out opens offline too. Apple cannot
# staple a bare file, so the terminal's ticket is looked up online when it
# first runs.
if [ -n "${MACOS_NOTARY_PROFILE-}" ]; then
  ditto -c -k --keepParent "$app" "$tmp/Letronna.zip"
  notarize "$tmp/Letronna.zip"
  xcrun stapler staple -q "$app"
  if [ -x "$cli" ]; then
    ditto -c -k --keepParent "$cli" "$tmp/letronna.zip"
    notarize "$tmp/letronna.zip"
  fi
fi
touch "$app"
echo "built $app ($version)"

[ "${1-}" = "--dmg" ] || exit 0

# hdiutil ships with macOS, so this needs nothing installed. The Applications
# symlink is what makes the window a drag-to-install target; UDZO compresses.
dmg=target/letronna-macos-$(uname -m)-$version.dmg
stage="$tmp/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -volname Letronna -srcfolder "$stage" -format UDZO -quiet "$dmg"
if [ -n "${MACOS_SIGN_IDENTITY-}" ]; then
  codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$dmg"
fi
if [ -n "${MACOS_NOTARY_PROFILE-}" ]; then
  notarize "$dmg"
  xcrun stapler staple -q "$dmg"
fi
echo "built $dmg"
