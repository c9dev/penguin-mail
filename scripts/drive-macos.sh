#!/bin/sh
# Runs the demo on macOS and drives it with a script of clicks, keys and
# scrolls, without touching the person's own mouse or keyboard; see
# app/src/drive.rs for the steps. Screenshots land beside the script, or
# in the folder given second.
#
#   scripts/drive-macos.sh steps.txt [shots-folder]
set -eu

script=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
shots=${2:-$(dirname "$script")}
mkdir -p "$shots"
cd "$(dirname "$0")/.."
cargo build -q -p mailrs --ignore-rust-version
cleanup() {
    [ -z "${runner:-}" ] || kill "$runner" 2>/dev/null || true
    [ -z "${watchdog:-}" ] || kill "$watchdog" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
PENGUIN_MAIL_DRIVE=$script PENGUIN_MAIL_DRIVE_SHOTS=$(cd "$shots" && pwd) \
    ./target/debug/penguin-mail --demo &
runner=$!
# A script that never says quit still ends: two minutes is plenty.
(
    sleep 120 &
    timer=$!
    trap 'kill "$timer" 2>/dev/null || true' EXIT
    trap 'exit 0' INT TERM
    wait "$timer"
    kill "$runner" 2>/dev/null || true
) &
watchdog=$!
status=0
wait "$runner" || status=$?
runner=
exit "$status"
