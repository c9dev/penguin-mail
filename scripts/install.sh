#!/usr/bin/env bash
# Builds Penguin Mail and installs it for the current user under ~/.local.
#   NO_AUTOSTART=1 scripts/install.sh   skips starting Penguin Mail at login
#   PREFIX=/opt/penguin-mail scripts/install.sh   installs somewhere else
# Upgrading from mailrs removes its binaries, launcher, and icons, and carries
# its login item over. The app moves its config and mail on first start.
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build --release -p mailrs -p mailrs-cli

tree=$(mktemp -d)
trap 'rm -rf "$tree"' EXIT
scripts/stage.sh "$tree"
scripts/install-files.sh "$tree"
