#!/usr/bin/env bash
# Installs what building, testing and packaging Penguin Mail needs on a bare
# Ubuntu 26.04, such as the container the GitHub workflows run in. Run as
# root. Rust comes from rustup at the version Cargo.toml asks for.
set -euo pipefail

cd "$(dirname "$0")/.."

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl git pkg-config \
    libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libglib2.0-dev-bin \
    gettext dpkg-dev file zip appstream desktop-file-utils \
    gnupg gpgsm \
    xvfb dbus-daemon at-spi2-core python3-gi gir1.2-atspi-2.0

rust=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
    --default-toolchain "$rust" --component clippy
# update-po.sh reads the Rust sources with xtr.
"$HOME/.cargo/bin/cargo" install --locked xtr --version 0.1.11
