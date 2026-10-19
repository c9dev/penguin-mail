#!/usr/bin/env bash
# Measures Penguin Mail's startup time, memory, sync time and store size,
# on a hidden display, so the numbers can be taken again after a change
# and compared.
#
#   scripts/measure.sh                      # everything, about 50 minutes
#   scripts/measure.sh --only startup,rest  # skip the large mailbox
#   scripts/measure.sh --out report.json --messages 20000 --runs 5
#
# What it measures, each over --runs launches (3 unless given) and given
# as the median with the lowest and highest:
#
# - Startup: the time from starting the release binary to its first frame
#   that shows more than the flat background the X server paints when the
#   window maps. The X DAMAGE extension says when the window changes, and
#   a read of its pixels says whether that change is the app's own frame.
#   One warm-up launch comes first, so files are in the page cache. Taken
#   for --demo and for an empty profile, which keeps the store its warm-up
#   made.
# - Memory at rest: RSS and PSS from /proc/<pid>/smaps_rollup after
#   --idle seconds (30 unless given) with the window open. "tree" adds
#   every process under the app, WebKit's web and network processes
#   among them. Sum PSS across processes, not RSS: PSS splits each
#   shared page between the processes that map it.
# - A large mailbox: Dovecot in Docker, seeded over IMAP with --messages
#   generated messages (100000 unless given) across Inbox, Archive and
#   Sent, dated over the last 28 days so the default 30-day sync window
#   reaches all of them. Each --sync-runs profile gets an IMAP account
#   for it (the rows Add Account writes, and the password in a keyring of
#   the sandbox's own), and the first sync counts from launch to the last
#   change in the store's message and thread counts, once the store marks
#   the backfill done and the counts have held for 10 s. On the last
#   profile it then reads memory after the sync, after scrolling All
#   Inboxes from top to bottom with the mouse wheel (it opens Archive and
#   All Inboxes from the sidebar first, as a list left open through the
#   first sync loads no further pages), after opening 20 conversations
#   with the Down key, and after --idle seconds more. Last, it relaunches
#   that profile --runs times with nothing new on the server and times
#   how long the app takes to go quiet (under 3% of a core over 3 s),
#   with its CPU seconds, and reports the store's size on disk.
# - The size of the release binary.
#
# The JSON report goes to --out (measure-<date>.json unless given) and
# progress to the terminal. --keep-logs DIR copies the app's own logs
# there before the sandbox goes; --no-build measures the binary as it is.
#
# Nothing here opens a window on your screen or reads your own mail: the
# app runs on Xvfb under a session bus, home, data, config and cache
# folders, a GnuPG home and a keyring of the run's own, and everything the
# run started goes down when it ends, the Dovecot container and its volume
# included. The renderer is cairo (GSK_RENDERER), so the numbers do not
# depend on the computer's GPU; set GSK_RENDERER yourself to measure
# another. The machine should be otherwise idle while this runs.
#
# Needs Xvfb, dbus-run-session, at-spi2-core, gnome-keyring, secret-tool
# (libsecret-tools), openssl, python3 with PyGObject and AT-SPI, the X
# DAMAGE and XTest libraries, and Docker with the Dovecot image from
# scripts/test-images.sh for the large mailbox:
#   Debian, Ubuntu  sudo apt install xvfb dbus-daemon at-spi2-core \
#                       gnome-keyring libsecret-tools openssl python3-gi \
#                       gir1.2-atspi-2.0 libxdamage1 libxtst6
#   Fedora          sudo dnf install xorg-x11-server-Xvfb dbus-daemon \
#                       at-spi2-core gnome-keyring libsecret openssl \
#                       python3-gobject gobject-introspection libXdamage libXtst
#   Arch            sudo pacman -S xorg-server-xvfb dbus at-spi2-core \
#                       gnome-keyring libsecret openssl python-gobject \
#                       libxdamage libxtst
#   openSUSE        sudo zypper install xorg-x11-server-Xvfb dbus-1-daemon \
#                       at-spi2-core gnome-keyring secret-tool openssl \
#                       python3-gobject libXdamage1 libXtst6
set -euo pipefail

cd "$(dirname "$0")/.."

out=measure-$(date +%Y-%m-%d).json
pass=()
build=1
keep_logs=
while [ $# -gt 0 ]; do
    case $1 in
        --out) out=$2; shift 2 ;;
        --no-build) build=0; shift ;;
        --keep-logs) keep_logs=$2; shift 2 ;;
        --only|--runs|--idle|--messages|--days|--sync-runs|--sync-timeout|--open-pause)
            pass+=("$1" "$2"); shift 2 ;;
        *) echo "measure.sh: unknown option $1" >&2; exit 2 ;;
    esac
done
out=$(realpath -m "$out")

for tool in Xvfb dbus-run-session gnome-keyring-daemon secret-tool openssl python3; do
    if ! command -v "$tool" >/dev/null; then
        echo "measure.sh needs $tool; see the comment at the top" >&2
        exit 2
    fi
done
. scripts/at-spi.sh
find_at_spi measure.sh

if [ "$build" = 1 ]; then
    cargo build --release --quiet -p mailrs --bin penguin-mail
fi
target=${CARGO_TARGET_DIR:-$PWD/target}
app=$target/release/penguin-mail

# A short path: sockets under XDG_RUNTIME_DIR must stay under 108 bytes.
sandbox=$(mktemp -d "${TMPDIR:-/tmp}/pm-measure.XXXXXX")
take_down() {
    # Every process the run started carries this XDG_RUNTIME_DIR, the
    # registry and the keyring daemon included, which fork away from the
    # pids they were started under. Nothing else on the computer has it.
    for proc in /proc/[0-9]*; do
        pid=${proc#/proc/}
        [ "$pid" = "$$" ] && continue
        if tr '\0' '\n' 2>/dev/null <"$proc/environ" |
            grep -qxF "XDG_RUNTIME_DIR=$sandbox/run"; then
            kill "$pid" 2>/dev/null || true
        fi
    done
    if command -v docker >/dev/null; then
        docker ps -aq --filter label=io.github.c9dev.penguin-mail.measure |
            xargs -r docker rm -f -v >/dev/null 2>&1 || true
    fi
    if [ -n "$keep_logs" ]; then
        mkdir -p "$keep_logs"
        cp -r "$sandbox/logs" "$sandbox/session.log" "$keep_logs/" 2>/dev/null || true
    fi
    rm -rf "$sandbox"
}
trap take_down EXIT

# Docker finds its context (Docker Desktop's socket, say) under the real
# home, which the next lines move away from.
export DOCKER_CONFIG=${DOCKER_CONFIG:-$HOME/.docker}
export HOME=$sandbox/home
export XDG_RUNTIME_DIR=$sandbox/run
export XDG_DATA_HOME=$HOME/.local/share XDG_CONFIG_HOME=$HOME/.config XDG_CACHE_HOME=$HOME/.cache
export GNUPGHOME=$sandbox/gnupg
mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
mkdir -m 700 "$GNUPGHOME"
export GSETTINGS_BACKEND=memory
export GSK_RENDERER=${GSK_RENDERER:-cairo}
export PENGUIN_MAIL_LOCALE_DIR=$PWD/target/locale
# GTK would take a Wayland session's display over the hidden one; a name
# nothing listens on sends it to X11.
unset DISPLAY
export WAYLAND_DISPLAY=penguin-mail-measure-none GDK_BACKEND=x11
if ! bwrap --ro-bind / / true 2>/dev/null; then
    export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
fi

Xvfb -displayfd 3 -screen 0 1400x900x24 3>"$sandbox/display" >/dev/null 2>&1 &
for _ in $(seq 100); do
    [ -s "$sandbox/display" ] && break
    sleep 0.1
done
export DISPLAY=:$(cat "$sandbox/display")

export MEASURE_LAUNCHER=$launcher MEASURE_REGISTRY=$registry MEASURE_APP=$app
export MEASURE_SANDBOX=$sandbox MEASURE_OUT=$out MEASURE_PY=$PWD/scripts/measure.py
# shellcheck disable=SC2016 # The variables expand in the inner shell.
inside='
"$MEASURE_LAUNCHER" --launch-immediately >/dev/null 2>&1 &
sleep 1
"$MEASURE_REGISTRY" >/dev/null 2>&1 &
# An unlocked keyring with a login collection, so the app stores and reads
# the IMAP password with no dialog.
printf "measure" | gnome-keyring-daemon --replace --unlock --components=secrets >/dev/null
sleep 1
python3 "$MEASURE_PY" --app "$MEASURE_APP" --sandbox "$MEASURE_SANDBOX" "$@" >"$MEASURE_OUT" 2>&3
'
# The bus and the portals it starts write a lot to stderr; that goes to a
# log, and only the measuring's progress reaches the terminal.
dbus-run-session -- bash -c "$inside" measure "${pass[@]}" 3>&2 2>"$sandbox/session.log"
echo "wrote $out"
