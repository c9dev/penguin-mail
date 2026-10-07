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
PENGUIN_MAIL_DRIVE=$script PENGUIN_MAIL_DRIVE_SHOTS=$(cd "$shots" && pwd) \
    ./target/debug/penguin-mail --demo 2>&1 | grep '^drive:' &
runner=$!
# A script that never says quit still ends: two minutes is plenty.
( sleep 120; pkill -f 'target/debug/penguin-mail --demo' ) 2>/dev/null &
watchdog=$!
wait "$runner" || true
kill "$watchdog" 2>/dev/null || true
