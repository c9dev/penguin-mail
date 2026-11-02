#!/usr/bin/env bash
# Builds Penguin Mail on Debian 13 and packs it as a .deb that installs
# there, in <out> (dist-debian by default):
#
#   scripts/package-debian.sh [out]
#
# Debian 13 ships GTK 4.18 and libadwaita 1.7, two releases behind what the
# Ubuntu build uses, so this build leaves the app's `gtk-4-20` and
# `adwaita-1-8` features off; the code keeps a path for each. The .deb's
# dependencies come from the libraries the build linked, so it has to be
# built on Debian 13 itself, with the packages CONTRIBUTING.md lists plus
# libjavascriptcoregtk-6.0-dev and libsoup-3.0-dev. The package carries no
# apt repository entry: the repository serves Ubuntu builds.
#
# Then: sudo apt install ./dist-debian/penguin-mail_<version>~deb13_amd64.deb
set -euo pipefail

cd "$(dirname "$0")/.."
out=${1:-dist-debian}

# The project's Google and Microsoft clients, for the owner's own builds,
# as install.sh reads them.
if [ -f packaging/secrets.env ]; then
    set -a
    # shellcheck source=/dev/null
    . packaging/secrets.env
    set +a
fi

if ! grep -qs '^VERSION_ID="13"' /etc/os-release || ! grep -qs '^ID=debian' /etc/os-release; then
    echo "package-debian.sh builds on Debian 13; the .deb takes its library versions from the build computer" >&2
    exit 1
fi

cargo build --release --locked --no-default-features -p mailrs -p mailrs-cli \
    --features mailrs/packaging-debian

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
tree=$(mktemp -d)
trap 'rm -rf "$tree"' EXIT
scripts/stage.sh "$tree"
scripts/package.sh "$tree" "$version" "$out" debian
