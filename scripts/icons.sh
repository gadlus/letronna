#!/usr/bin/env bash
# Pulls Phosphor Icons (Light weight) from npm into assets/icons.
# File names follow what gpui-component's IconName loads (Lucide-style names such as
# panel-left-close.svg), so the map below is "our name:phosphor name". Re-run to refresh.
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
curl -sL -o "$tmp/p.tgz" https://registry.npmjs.org/@phosphor-icons/core/-/core-2.1.1.tgz
tar -xzf "$tmp/p.tgz" -C "$tmp"
src="$tmp/package/assets/light"
mkdir -p assets/icons
map="plus:plus close:x window-close:x window-minimize:minus window-maximize:square window-restore:browsers
minus:minus settings:gear arrow-up:arrow-up copy:copy panel-left-close:sidebar-simple panel-left-open:sidebar-simple
chevron-down:caret-down chevron-right:caret-right check:check loader-circle:circle-notch loader:circle-notch bot:robot mic:microphone chevrons-up-down:caret-up-down
inbox:tray send:paper-plane-tilt sparkles:sparkle message-square:chat-teardrop file:file file-text:file-text clock:clock
search:magnifying-glass pin:push-pin folder:folder image:image square-pen:note-pencil list-filter:funnel-simple
info:info plugs:plugs palette:palette trash:trash export:export pencil:pencil-simple envelope:envelope-simple play:play stop:stop
circle:circle list-checks:list-checks
circle-x:x-circle circle-check:check-circle triangle-alert:warning shield:shield-check puzzle:puzzle-piece circle-user:user-circle"
for pair in $map; do
  cp "$src/${pair#*:}-light.svg" "assets/icons/${pair%%:*}.svg"
done

# Brand marks for the Messaging channels, from Simple Icons (the SVGs are CC0;
# the brands remain their owners' trademarks). Single-path and monochrome, so
# gpui tints them with the channel colour.
curl -sL -o "$tmp/si.tgz" https://registry.npmjs.org/simple-icons/-/simple-icons-13.21.0.tgz
tar -xzf "$tmp/si.tgz" -C "$tmp"
mkdir -p assets/icons/brands
for brand in telegram discord slack whatsapp; do
  cp "$tmp/package/icons/$brand.svg" "assets/icons/brands/$brand.svg"
done

rm -rf "$tmp"
echo "icons updated: $(ls assets/icons | wc -l) + $(ls assets/icons/brands | wc -l) brands"
