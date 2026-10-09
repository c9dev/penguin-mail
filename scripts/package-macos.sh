#!/bin/bash
# Builds a portable app, signs it when configured, and writes a release zip.
set -euo pipefail
cd "$(dirname "$0")/.."
out=${1:-dist}
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/Penguin Mail.app"
(
    unset MACOS_CERTIFICATE_P12 MACOS_CERTIFICATE_PASSWORD APPLE_ID APPLE_TEAM_ID APPLE_APP_SPECIFIC_PASSWORD
    APP_DIR="$work" scripts/install-macos.sh
    python3 scripts/bundle-macos.py "$app"
)
scripts/sign-macos.sh "$app" "$out"
