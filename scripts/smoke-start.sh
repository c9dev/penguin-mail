#!/usr/bin/env bash
# Starts an installed Penguin Mail with --demo on a hidden display and
# checks it is still running fifteen seconds later. The package jobs run
# it after installing what they built, so a library or a runtime
# permission the package forgot shows up as a crash here, not on
# someone's desktop.
#
#   scripts/smoke-start.sh <command...>
#   scripts/smoke-start.sh /usr/bin/penguin-mail
#   scripts/smoke-start.sh flatpak run io.github.c9dev.PenguinMail
#
# Needs Xvfb and dbus-run-session.
set -euo pipefail

[ $# -gt 0 ] || { echo "usage: scripts/smoke-start.sh <command...>" >&2; exit 2; }
display=:${SMOKE_DISPLAY:-94}
log=$(mktemp)
Xvfb "$display" -screen 0 1280x800x24 >/dev/null 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true; rm -f "$log"' EXIT
for _ in $(seq 50); do
    [ -e "/tmp/.X11-unix/X${display#:}" ] && break
    sleep 0.1
done

status=0
DISPLAY=$display timeout 15 dbus-run-session -- "$@" --demo >"$log" 2>&1 || status=$?
# timeout answers 124 when it had to stop the app, which is the pass.
if [ "$status" -ne 124 ]; then
    echo "Penguin Mail stopped with status $status within 15 seconds:" >&2
    cat "$log" >&2
    exit 1
fi
echo "Penguin Mail ran for 15 seconds on a hidden display."
