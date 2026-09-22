#!/usr/bin/env bash
# Wraps the release binary in target/Letronna.app. macOS reads the icon, the
# display name and the menu bar title from a bundle, so a bare executable shows
# a generic icon and a lowercase name however it is launched. Run after
# `cargo build --release`. Pass --dmg to also package it for download.
set -euo pipefail
cd "$(dirname "$0")/.."
bin=target/release/letronna
[ -x "$bin" ] || { echo "build it first: cargo build --release" >&2; exit 1; }
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
sips -s format png assets/icon.ico --out "$tmp/icon.png" >/dev/null
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
</dict>
</plist>
PLIST

# An unsigned bundle has no stable identity, so macOS can keep serving a stale
# icon from its cache. Ad-hoc signing is enough to give it one.
codesign --force --sign - "$app" >/dev/null 2>&1 || true
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
echo "built $dmg"
