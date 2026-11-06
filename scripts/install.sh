#!/bin/sh
# Letronna for Linux and macOS.
#
#   curl -fsSL https://letronna.ai/install.sh | sh
#
# This only delivers the program. On Linux it downloads the latest release
# into a temporary folder and hands over to `letronna setup`, which asks your
# name, signs you in, picks a provider and a model, and puts the window and
# the terminal where you choose. On a Mac it puts Letronna.app in
# Applications and the terminal in ~/.local/share/letronna/bin, then opens
# the app, which asks the same questions the first time it starts. Nothing
# needs root.
#
# LETRONNA_DOWNLOAD           a different archive (on a Mac, disk image), as
#                             a URL or a local path
# LETRONNA_DOWNLOAD_TERMINAL  on a Mac, a different terminal, likewise
# LETRONNA_APPLICATIONS       on a Mac, the folder the app goes in; by default
#                             /Applications, or ~/Applications when that is
#                             not yours to write to
# LETRONNA_NO_SETUP           set to anything to unpack and stop, for
#                             unattended use

set -eu

printf '\n  \033[38;5;69mLetronna\033[0m  \033[2mfetching the latest release\033[0m\n'

staging="$(mktemp -d)"
cleanup() {
    if [ -d "$staging/disk" ]; then
        hdiutil detach -quiet -force "$staging/disk" 2>/dev/null || true
    fi
    rm -rf "$staging"
}
trap cleanup EXIT

# fetch <URL or path> <where to put it>
fetch() {
    if [ -f "$1" ]; then
        cp "$1" "$2"
    elif command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        echo "  Need curl or wget to download Letronna." >&2
        exit 1
    fi
}

latest=https://github.com/gadlus/letronna/releases/latest/download

if [ "$(uname -s)" = Darwin ]; then
    # Asked of the hardware: a shell under Rosetta says x86_64 on Apple
    # silicon too.
    if [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" != 1 ]; then
        echo "  Letronna for Mac needs Apple silicon (M1 or later)." >&2
        exit 1
    fi
    fetch "${LETRONNA_DOWNLOAD:-$latest/letronna-macos-arm64.dmg}" "$staging/letronna.dmg"
    fetch "${LETRONNA_DOWNLOAD_TERMINAL:-$latest/letronna-macos-arm64}" "$staging/letronna"

    # An administrator's account can write to /Applications without sudo;
    # anyone else gets their own Applications folder.
    apps="${LETRONNA_APPLICATIONS:-/Applications}"
    if [ -z "${LETRONNA_APPLICATIONS:-}" ] && [ ! -w "$apps" ]; then
        apps="$HOME/Applications"
    fi
    mkdir -p "$apps"
    app="$apps/Letronna.app"

    # Copied in beside the old one and swapped by renaming, so a Letronna
    # that is open keeps running from what it started with, and an old copy
    # macOS will not let go of is left whole rather than half deleted.
    new="$apps/.Letronna.app.new"
    old="$apps/.Letronna.app.old"
    mkdir "$staging/disk"
    hdiutil attach -nobrowse -noautoopen -readonly -quiet -mountpoint "$staging/disk" "$staging/letronna.dmg"
    if [ ! -d "$staging/disk/Letronna.app" ]; then
        echo "  The download did not contain Letronna.app." >&2
        exit 1
    fi
    rm -rf "$new" "$old" 2>/dev/null || true
    ditto "$staging/disk/Letronna.app" "$new"
    hdiutil detach -quiet "$staging/disk"
    rmdir "$staging/disk"
    if [ -e "$app" ] && ! mv "$app" "$old" 2>/dev/null; then
        rm -rf "$new"
        echo "  macOS would not let this replace $app. Allow your terminal under" >&2
        echo "  System Settings › Privacy & Security › App Management, or delete" >&2
        echo "  the old Letronna yourself, then run this again." >&2
        exit 1
    fi
    mv "$new" "$app"
    if [ -e "$old" ] && ! rm -rf "$old" 2>/dev/null; then
        echo "  The old Letronna is left at $old; delete it once it is closed."
    fi

    # The same place `letronna setup` puts it, which is where updates look.
    bin="$HOME/.local/share/letronna/bin"
    mkdir -p "$bin"
    cp "$staging/letronna" "$bin/.letronna.new"
    chmod +x "$bin/.letronna.new"
    mv -f "$bin/.letronna.new" "$bin/letronna"

    # zsh, the Mac's shell, reads ~/.zprofile and not ~/.profile. Making a
    # ~/.bash_profile would hide ~/.profile from bash, so that only gets
    # the line when it is already there.
    case ":$PATH:" in
    *":$bin:"*) ;;
    *)
        case "$(basename "${SHELL:-zsh}")" in
        zsh) profile="$HOME/.zprofile" ;;
        bash) if [ -f "$HOME/.bash_profile" ]; then profile="$HOME/.bash_profile"; else profile="$HOME/.profile"; fi ;;
        *) profile="$HOME/.profile" ;;
        esac
        line="export PATH=\"\$PATH:$bin\""
        grep -qsxF "$line" "$profile" || printf '\n# Letronna\n%s\n' "$line" >> "$profile"
        ;;
    esac

    printf '  app       %s\n  terminal  %s\n' "$app" "$bin/letronna"
    if [ -n "${LETRONNA_NO_SETUP:-}" ]; then
        exit 0
    fi
    open "$app"
    printf '  Letronna is opening. In a new terminal, `letronna` starts it there too.\n\n'
    exit 0
fi

archive="$staging/letronna.tar.gz"
fetch "${LETRONNA_DOWNLOAD:-$latest/letronna-linux-x64.tar.gz}" "$archive"

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
