#!/usr/bin/env bash
# Retakes the README screenshots from the demo, on a hidden display.
#
#   scripts/screenshots.sh              every shot in docs/screenshots
#   scripts/screenshots.sh dark rules   only the shots named
#
# Each shot starts the demo afresh with its own settings, drives it into
# one state through the accessibility tree and the keyboard, and saves the
# window. scripts/screenshots.py holds the list of shots and what each one
# does. A full run takes about four minutes. A shot that goes wrong saves
# what was on screen as target/failed-N.png and stops the run.
#
# Nothing talks to Google: the demo runs on sample accounts in a throwaway
# store, the welcome shot runs with an empty home directory, and the
# assistant shot answers from a scripted model on 127.0.0.1.
#
# Needs Xvfb, dbus-run-session, at-spi2-core, ImageMagick's import, and
# python3 with the GObject and Xlib bindings:
#   sudo apt install xvfb dbus-daemon at-spi2-core imagemagick \
#       python3-gi python3-xlib
set -euo pipefail

cd "$(dirname "$0")/.."

for tool in Xvfb xvfb-run dbus-run-session python3 import; do
    if ! command -v "$tool" >/dev/null; then
        echo "screenshots.sh needs $tool; see the comment at the top" >&2
        exit 2
    fi
done
if ! python3 -c 'import Xlib, gi' 2>/dev/null; then
    echo "screenshots.sh needs python3-xlib and python3-gi" >&2
    exit 2
fi
registry=/usr/libexec/at-spi2-registryd
launcher=/usr/libexec/at-spi-bus-launcher
if [ ! -x "$registry" ] || [ ! -x "$launcher" ]; then
    echo "screenshots.sh needs at-spi2-core installed" >&2
    exit 2
fi

cargo build --quiet -p mailrs

# The same sandbox a11y-names.sh uses: its own home and runtime directory,
# so the settings and keyring of the copy you use stay out of the run.
sandbox=$(mktemp -d)
take_down() {
    for proc in /proc/[0-9]*; do
        pid=${proc#/proc/}
        [ "$pid" = "$$" ] && continue
        if { tr '\0' '\n' <"$proc/environ"; } 2>/dev/null |
            grep -qxF "XDG_RUNTIME_DIR=$sandbox/run"; then
            kill "$pid" 2>/dev/null
        fi
    done
}
trap 'take_down; rm -rf "$sandbox"' EXIT
export HOME="$sandbox/home"
export XDG_RUNTIME_DIR="$sandbox/run"
mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
# GSettings in a file inside the sandbox, which the desktop portal reads
# too: the accent colour and the light or dark preference it hands the app
# come from here, not from the machine the script runs on.
export GSETTINGS_BACKEND=keyfile
mkdir -p "$HOME/.config/glib-2.0/settings"
cat >"$HOME/.config/glib-2.0/settings/keyfile" <<'KEYFILE'
[org/gnome/desktop/interface]
color-scheme='default'
accent-color='purple'
KEYFILE
export PENGUIN_MAIL_LOCALE_DIR="$PWD/target/locale"
# The screenshots are in English whatever the machine speaks.
export LANG=en_US.UTF-8 LANGUAGE=en_US LC_ALL=en_US.UTF-8

driver=$PWD/scripts/screenshots.py
app=$PWD/target/debug/penguin-mail
out=$PWD/docs/screenshots
inside="
$launcher --launch-immediately &
sleep 1
$registry &
sleep 1
python3 '$driver' '$app' '$out' '$sandbox' $*
"
xvfb-run -a --server-args="-screen 0 1600x1000x24" \
    dbus-run-session -- bash -c "$inside"
