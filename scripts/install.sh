#!/usr/bin/env bash
# Builds Penguin Mail and installs it for the current user under ~/.local.
#   NO_AUTOSTART=1 scripts/install.sh   skips starting Penguin Mail at login
#   PREFIX=/opt/penguin-mail scripts/install.sh   installs somewhere else
# Upgrading from mailrs removes its binaries, launcher, and icons, and carries
# its login item over. The app moves its config and mail on first start.
set -euo pipefail

cd "$(dirname "$0")/.."

# The project's Google and Microsoft clients, for the owner's own builds.
# The file is gitignored; without it the build cannot add Google accounts.
if [ -f packaging/secrets.env ]; then
    set -a
    # shellcheck source=/dev/null
    . packaging/secrets.env
    set +a
fi

cargo build --release -p mailrs -p mailrs-cli

tree=$(mktemp -d)
trap 'rm -rf "$tree"' EXIT
scripts/stage.sh "$tree"
scripts/install-files.sh "$tree"
