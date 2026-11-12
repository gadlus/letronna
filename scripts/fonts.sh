#!/usr/bin/env bash
# Pulls the three type families the window bundles into crates/gui/assets/fonts,
# each cut down to the scripts the window's own words use. Text in any other
# script falls back to the system's fonts, as it did before these were bundled.
# All three are under the SIL Open Font License, kept beside them. Re-run to
# refresh. Needs curl, unzip and fonttools (`pip install fonttools`).
set -euo pipefail
cd "$(dirname "$0")/.."
out=crates/gui/assets/fonts
tmp=$(mktemp -d)
mkdir -p "$out"

curl -sSL -o "$tmp/inter.zip" https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip
curl -sSL -o "$tmp/mono.zip" https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip
curl -sSL -o "$tmp/Newsreader.ttf" "https://raw.githubusercontent.com/google/fonts/main/ofl/newsreader/Newsreader%5Bopsz%2Cwght%5D.ttf"
curl -sSL -o "$tmp/Newsreader-OFL.txt" https://raw.githubusercontent.com/google/fonts/main/ofl/newsreader/OFL.txt
unzip -qo "$tmp/inter.zip" -d "$tmp/inter"
unzip -qo "$tmp/mono.zip" -d "$tmp/mono"

# Newsreader comes as one variable file, and the window's text systems pick a
# face by its weight, not along an axis: the one weight the masthead is set in,
# at the optical size its titles are drawn near.
python3 -I -m fontTools varLib.instancer "$tmp/Newsreader.ttf" wght=500 opsz=24 -o "$tmp/Newsreader-Medium.ttf" -q
# The cut keeps the variable file's names, "Newsreader 16pt" among them: named
# plainly, so every system files it under the one family the window asks for.
python3 -I - "$tmp/Newsreader-Medium.ttf" <<'PY'
import sys
from fontTools.ttLib import TTFont
font = TTFont(sys.argv[1])
names = {1: "Newsreader", 2: "Regular", 4: "Newsreader Medium", 6: "Newsreader-Medium", 16: "Newsreader", 17: "Medium"}
for record in list(font["name"].names):
    if record.nameID in names:
        font["name"].setName(names[record.nameID], record.nameID, record.platformID, record.platEncID, record.langID)
    elif record.nameID in (3, 21, 22, 25):
        font["name"].removeNames(record.nameID)
font.save(sys.argv[1])
PY

# Latin with its extensions, Greek, Cyrillic, punctuation, currency, arrows,
# maths, the key symbols, box drawing and the shapes a terminal prints.
keep="U+0020-007E,U+00A0-024F,U+0259,U+02B0-02FF,U+0300-036F,U+0370-03FF,U+0400-04FF,U+1E00-1EFF,U+2000-206F,U+2070-209F,U+20A0-20BF,U+2100-214F,U+2150-218F,U+2190-21FF,U+2200-22FF,U+2300-23FF,U+2500-259F,U+25A0-25FF,U+2713-2717,U+FB00-FB06,U+FFFD"
# The features text is set with by default, and the figures that line up in
# a column; the stylistic sets nobody can switch on would keep every
# alternate glyph in the file.
trim() {
  python3 -I -m fontTools subset "$1" --unicodes="$keep" --layout-features+=tnum,case,zero \
    --name-IDs='*' --notdef-outline --output-file="$out/$(basename "$1")"
}
for face in Regular Italic Medium SemiBold Bold; do
  trim "$tmp/inter/extras/ttf/Inter-$face.ttf"
done
# The NL cut, with no ligatures: code is read letter for letter, and `->`
# drawn as an arrow is not what the file holds.
for face in Regular Italic Bold; do
  trim "$tmp/mono/fonts/ttf/JetBrainsMonoNL-$face.ttf"
done
trim "$tmp/Newsreader-Medium.ttf"

cp "$tmp/inter/LICENSE.txt" "$out/Inter-OFL.txt"
cp "$tmp/mono/OFL.txt" "$out/JetBrainsMono-OFL.txt"
cp "$tmp/Newsreader-OFL.txt" "$out/Newsreader-OFL.txt"

rm -rf "$tmp"
echo "fonts updated: $(ls "$out"/*.ttf | wc -l) faces, $(du -ch "$out"/*.ttf | tail -1 | cut -f1)"
