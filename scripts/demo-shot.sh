#!/usr/bin/env bash
# Opens the demo on a hidden display and saves a screenshot of it.
#
#   scripts/demo-shot.sh out.png
#   scripts/demo-shot.sh out.png --size 1400x900 --wait 8 --run drive.py
#
# --run takes a Python script that runs once the window is up, with the
# accessibility bus and DISPLAY set, to click or type before the shot (see
# the walk in scripts/a11y-names.sh for how to find widgets and send XTest
# events). The screenshot waits for it to exit.
#
# Everything this starts, the accessibility registry included, goes down
# when it ends. The registry forks away from the pid it was started under,
# and a screenshot run that only kills its own children leaves one behind
# for every run, so take agents here rather than to a recipe of their own.
#
# Needs Xvfb, dbus-run-session, at-spi2-core and ImageMagick's import:
#   sudo apt install xvfb dbus-daemon at-spi2-core imagemagick
set -euo pipefail

cd "$(dirname "$0")/.."

out=${1:?usage: scripts/demo-shot.sh out.png [--size WxH] [--wait seconds] [--run script.py]}
shift
size=1400x900
wait=8
run=
while [ $# -gt 0 ]; do
    case $1 in
        --size) size=$2; shift 2 ;;
        --wait) wait=$2; shift 2 ;;
        --run) run=$(realpath "$2"); shift 2 ;;
        *) echo "demo-shot.sh: unknown option $1" >&2; exit 2 ;;
    esac
done
out=$(realpath -m "$out")
mkdir -p "$(dirname "$out")"

for tool in Xvfb dbus-run-session import; do
    if ! command -v "$tool" >/dev/null; then
        echo "demo-shot.sh needs $tool; see the comment at the top" >&2
        exit 2
    fi
done
registry=/usr/libexec/at-spi2-registryd
launcher=/usr/libexec/at-spi-bus-launcher

cargo build --quiet -p mailrs

# The same sandbox as scripts/a11y-names.sh: its own home and runtime
# directory, so the run touches no settings of the copy you use, and the
# path that says which processes to take down at the end.
sandbox=$(mktemp -d)
take_down() {
    for proc in /proc/[0-9]*; do
        pid=${proc#/proc/}
        [ "$pid" = "$$" ] && continue
        if tr '\0' '\n' 2>/dev/null <"$proc/environ" |
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
# A GnuPG home of the run's own. With only HOME moved, gpg still takes
# $HOME/.gnupg for the default home and talks to the agent already running
# for the person's own keys, so a secret key the run imports would land in
# their keyring. A home by another name gets an agent of its own.
export GNUPGHOME="$sandbox/gnupg"
mkdir -m 700 "$GNUPGHOME"
export GSETTINGS_BACKEND=memory
export PENGUIN_MAIL_LOCALE_DIR="$PWD/target/locale"

if ! bwrap --ro-bind / / true 2>/dev/null; then
    export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
fi
# The paths reach the hidden session as variables, quoted where it uses
# them, so a space or a `;` in a file name stays part of the name.
export DEMO_LAUNCHER=$launcher DEMO_REGISTRY=$registry DEMO_APP=$PWD/target/debug/penguin-mail
export DEMO_LOG=$sandbox/app.log DEMO_WAIT=$wait DEMO_RUN=$run DEMO_OUT=$out
# shellcheck disable=SC2016 # The variables expand in the inner shell.
inside='
"$DEMO_LAUNCHER" --launch-immediately &
sleep 1
"$DEMO_REGISTRY" &
sleep 1
"$DEMO_APP" --demo >"$DEMO_LOG" 2>&1 &
sleep "$DEMO_WAIT"
if [ -n "$DEMO_RUN" ]; then python3 "$DEMO_RUN"; fi
import -window root "$DEMO_OUT"
'
xvfb-run -a --server-args="-screen 0 ${size}x24" \
    dbus-run-session -- bash -c "$inside"
echo "wrote $out"
