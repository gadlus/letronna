#!/bin/sh
# Letronna for Linux.
#
#   curl -fsSL https://letronna.ai/install.sh | sh
#
# This only delivers the program. It downloads the latest release into a
# temporary folder and hands over to `letronna setup`, which asks your name,
# signs you in, picks a provider and a model, and puts the window and the
# terminal where you choose. Nothing needs root, and nothing goes outside
# your own home directory.
#
# LETRONNA_DOWNLOAD  a different archive, as a URL or a local path
# LETRONNA_NO_SETUP  set to anything to unpack and stop, for unattended use

set -eu

source="${LETRONNA_DOWNLOAD:-https://github.com/gadlus/letronna/releases/latest/download/letronna-linux-x64.tar.gz}"

printf '\n  \033[38;5;69mLetronna\033[0m  \033[2mfetching the latest release\033[0m\n'

staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
archive="$staging/letronna.tar.gz"

if [ -f "$source" ]; then
    cp "$source" "$archive"
elif command -v curl >/dev/null 2>&1; then
    curl -fsSL "$source" -o "$archive"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$archive" "$source"
else
    echo "  Need curl or wget to download Letronna." >&2
    exit 1
fi

tar -xzf "$archive" -C "$staging"
exe="$(find "$staging" -type f -name letronna | head -n 1)"
if [ -z "$exe" ]; then
    echo "  The download did not contain letronna." >&2
    exit 1
fi
chmod +x "$exe"
[ -f "$(dirname "$exe")/letronna-gui" ] && chmod +x "$(dirname "$exe")/letronna-gui"

if [ -n "${LETRONNA_NO_SETUP:-}" ]; then
    echo "  Unpacked to $(dirname "$exe")"
    trap - EXIT
    exit 0
fi

# This script arrived on standard input, so setup has to be pointed at the
# terminal to have anywhere to read the answers from.
"$exe" setup < /dev/tty
