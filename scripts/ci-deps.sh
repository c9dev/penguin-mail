#!/usr/bin/env bash
# Installs what building, testing and packaging Penguin Mail needs on a bare
# Ubuntu 26.04 or Fedora, such as the containers the GitHub workflows run
# in. Run as root. Rust comes from rustup at the version Cargo.toml asks
# for. xtr, which scripts/update-po.sh needs, is left to the workflow: it
# installs xtr after restoring the cargo cache, which usually holds it.
#
#   scripts/ci-deps.sh              the system packages, then Rust
#   scripts/ci-deps.sh --rust-only  Rust alone, as any user, for a job that
#                                   builds only crates needing no system
#                                   libraries, such as the Docker suite
set -euo pipefail

cd "$(dirname "$0")/.."

if [ "${1:-}" = --rust-only ]; then
    :
elif command -v apt-get >/dev/null; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git pkg-config \
        libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libglib2.0-dev-bin \
        gettext dpkg-dev file zip appstream desktop-file-utils \
        gnupg gpgsm openssl \
        xvfb dbus-daemon at-spi2-core python3-gi gir1.2-atspi-2.0 libxtst6
elif command -v dnf >/dev/null; then
    dnf install -y \
        gcc curl git pkgconf-pkg-config \
        gtk4-devel libadwaita-devel webkitgtk6.0-devel glib2-devel \
        gettext rpm-build rpm-sign appstream desktop-file-utils \
        gnupg2 gnupg2-smime openssl \
        xorg-x11-server-Xvfb dbus-daemon
else
    echo "ci-deps.sh knows apt and dnf only" >&2
    exit 1
fi

# rustup-init comes from a fixed release, checked against the sums written
# here. A new rustup changes nothing until someone updates both, and a file
# that does not match stops the build before it runs.
rustup_version=1.29.1
case $(uname -m) in
x86_64)
    target=x86_64-unknown-linux-gnu
    rustup_sum=dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71
    ;;
aarch64)
    target=aarch64-unknown-linux-gnu
    rustup_sum=15f6e4ce9f583b929c996c91562bad6d4454f3281de858b02cdfdef615fac433
    ;;
*)
    echo "ci-deps.sh has no rustup-init sum for $(uname -m)" >&2
    exit 1
    ;;
esac
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -sSfL --retry 3 -o "$work/rustup-init" \
    "https://static.rust-lang.org/rustup/archive/$rustup_version/$target/rustup-init"
if ! echo "$rustup_sum  $work/rustup-init" | sha256sum -c --quiet -; then
    echo "rustup-init $rustup_version for $target does not match its sum" >&2
    exit 1
fi
chmod +x "$work/rustup-init"
rust=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)
"$work/rustup-init" -y --no-modify-path --profile minimal \
    --default-toolchain "$rust" --component clippy
