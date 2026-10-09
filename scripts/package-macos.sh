#!/bin/bash
# Builds a portable app, signs it when configured, and writes a release zip.
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-dist}
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
APP_DIR="$work" scripts/install-macos.sh
app="$work/Penguin Mail.app"
python3 scripts/bundle-macos.py "$app"
scripts/sign-macos.sh "$app" "$out"
