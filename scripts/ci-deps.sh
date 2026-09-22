#!/usr/bin/env bash
# Installs what building, testing and packaging Penguin Mail needs on a bare
# Ubuntu 26.04 or Fedora, such as the containers the GitHub workflows run
# in. Run as root. Rust comes from rustup at the version Cargo.toml asks
# for.
set -euo pipefail

cd "$(dirname "$0")/.."

if command -v apt-get >/dev/null; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git pkg-config \
        libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libglib2.0-dev-bin \
        gettext dpkg-dev file zip appstream desktop-file-utils \
        gnupg gpgsm \
        xvfb dbus-daemon at-spi2-core python3-gi gir1.2-atspi-2.0
elif command -v dnf >/dev/null; then
    dnf install -y \
        gcc curl git pkgconf-pkg-config \
        gtk4-devel libadwaita-devel webkitgtk6.0-devel glib2-devel \
        gettext rpm-build rpm-sign appstream desktop-file-utils \
        gnupg2 gnupg2-smime \
        xorg-x11-server-Xvfb dbus-tools
else
    echo "ci-deps.sh knows apt and dnf only" >&2
    exit 1
fi

rust=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
    --default-toolchain "$rust" --component clippy
# update-po.sh reads the Rust sources with xtr.
"$HOME/.cargo/bin/cargo" install --locked xtr --version 0.1.11
