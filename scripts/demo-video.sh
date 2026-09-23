#!/usr/bin/env bash
# Records the Penguin Mail demo video: a tour of the demo accounts in a
# real GNOME Shell, with captions and title cards.
#
#   scripts/demo-video.sh          target/demo-video/penguin-mail.mp4 and .webm
#   scripts/demo-video.sh probe    dump the accessible tree at each stop
#
# GNOME Shell runs headless in a throwaway session with its own home,
# D-Bus, PipeWire and settings, and records its virtual monitor through
# Mutter's ScreenCast interface. Nothing reaches your own session or
# Google: the demo runs on sample accounts, and the assistant answers from
# a scripted model on 127.0.0.1. scripts/demo-video/tour.py holds the
# tour, and scripts/demo-video/compose.py cuts the result together.
#
# Needs gnome-shell, pipewire, wireplumber, GStreamer with pipewiresrc and
# x264enc, ffmpeg with libx264 and libvpx-vp9, rsvg-convert, and python3
# with the GObject bindings and AT-SPI.
set -euo pipefail

cd "$(dirname "$0")/.."
here=$PWD/scripts/demo-video
mode=${1:-record}

for tool in gnome-shell pipewire wireplumber gst-launch-1.0 ffmpeg rsvg-convert dbus-run-session python3; do
    if ! command -v "$tool" >/dev/null; then
        echo "demo-video.sh needs $tool; see the comment at the top" >&2
        exit 2
    fi
done

cargo build --quiet --release -p mailrs
app=$PWD/target/release/penguin-mail
out=$PWD/target/demo-video
mkdir -p "$out"

# The session's processes all carry this runtime directory, which is how
# take_down finds them without touching anything of yours.
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
trap 'take_down; sleep 1; rm -rf "$sandbox"' EXIT
export HOME="$sandbox/home"
export XDG_RUNTIME_DIR="$sandbox/run"
share=$HOME/.local/share
mkdir -p "$XDG_RUNTIME_DIR" "$share/applications" "$share/icons/hicolor/scalable/apps" \
    "$share/gnome-shell/extensions/tour@penguinmail.dev" "$share/backgrounds"
chmod 700 "$XDG_RUNTIME_DIR"
cp "$here"/extension/* "$share/gnome-shell/extensions/tour@penguinmail.dev/"
cp app/data/icons/scalable/apps/*.svg "$share/icons/hicolor/scalable/apps/"
# The dock and the overview match a window to its app by these files. The
# demo runs under its own app ID, so it gets one too.
for id in io.github.c9dev.PenguinMail io.github.c9dev.PenguinMail.Demo; do
    sed "s|^Exec=.*|Exec=$app --demo|" app/data/io.github.c9dev.PenguinMail.desktop \
        >"$share/applications/$id.desktop"
done
for variant in light dark; do
    rsvg-convert -w 1920 -h 1080 "$here/art/wall-$variant.svg" -o "$share/backgrounds/wall-$variant.png"
done
unset WAYLAND_DISPLAY DISPLAY DBUS_SESSION_BUS_ADDRESS PIPEWIRE_REMOTE
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME
export PENGUIN_MAIL_LOCALE_DIR="$PWD/target/locale"
# The video is in English whatever the machine speaks.
export LANG=en_US.UTF-8 LANGUAGE=en_US LC_ALL=en_US.UTF-8

# The demo runs under its own app ID, so the dock and the window match it.
app_id=io.github.c9dev.PenguinMail.Demo

inside="
set -e
gsettings set org.gnome.shell disable-user-extensions false
gsettings set org.gnome.shell enabled-extensions \"['tour@penguinmail.dev']\"
gsettings set org.gnome.shell welcome-dialog-last-shown-version '999'
gsettings set org.gnome.shell favorite-apps \"['org.gnome.Nautilus.desktop', '$app_id.desktop', 'org.gnome.Calendar.desktop']\"
gsettings set org.gnome.desktop.interface color-scheme 'default'
gsettings set org.gnome.desktop.interface accent-color 'orange'
gsettings set org.gnome.desktop.interface enable-hot-corners false
gsettings set org.gnome.desktop.interface clock-show-weekday true
gsettings set org.gnome.desktop.background picture-uri 'file://$share/backgrounds/wall-light.png'
gsettings set org.gnome.desktop.background picture-uri-dark 'file://$share/backgrounds/wall-dark.png'
gsettings set org.gnome.desktop.notifications show-banners true
pipewire >'$out/pipewire.log' 2>&1 &
sleep 1
wireplumber >'$out/wireplumber.log' 2>&1 &
gnome-shell --headless --wayland --virtual-monitor 1920x1080 >'$out/shell.log' 2>&1 &
for _ in \$(seq 120); do
    gdbus introspect --session --dest org.gnome.Shell --object-path /dev/penguinmail/Tour 2>/dev/null |
        grep -qF 'interface dev.penguinmail.Tour' && break
    sleep 0.5
done
export WAYLAND_DISPLAY=wayland-0
python3 '$here/tour.py' '$app' '$out' '$sandbox' $mode
"
dbus-run-session -- bash -c "$inside"

if [ "$mode" = record ]; then
    version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
    python3 "$here/compose.py" "$out" "$version"
fi
