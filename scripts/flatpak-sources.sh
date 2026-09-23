#!/usr/bin/env bash
# Writes packaging/flatpak/cargo-sources.json from Cargo.lock, the list of
# crates flatpak-builder downloads before a build, since Flathub builds
# with no network. Run it whenever Cargo.lock changes a dependency.
#
#   scripts/flatpak-sources.sh                 rewrite cargo-sources.json
#   scripts/flatpak-sources.sh --check         say whether it matches Cargo.lock
#   scripts/flatpak-sources.sh --flathub vX.Y.Z <dir>
#       also write the files for the flathub/io.github.c9dev.PenguinMail
#       repository into <dir>: the manifest building that tag, and the
#       two files beside it. The manifest gets the Google client from
#       PENGUIN_MAIL_GOOGLE_CLIENT_ID and _SECRET, read from the
#       environment or from the gitignored packaging/secrets.env.
#
# flatpak-cargo-generator comes from flatpak-builder-tools at a fixed
# commit, and runs in a Python environment under ~/.cache made on first
# use.
set -euo pipefail

cd "$(dirname "$0")/.."
flatpak=packaging/flatpak
manifest=$flatpak/io.github.c9dev.PenguinMail.yml
tools_commit=41c20aa10819cdb2a4f3ca171758a96d1955c018
mode=${1:-}

cache=${XDG_CACHE_HOME:-$HOME/.cache}/penguin-mail/flatpak-cargo-generator
generator=$cache/flatpak-cargo-generator-$tools_commit.py
if [ ! -e "$generator" ]; then
    python3 -m venv "$cache/venv"
    "$cache/venv/bin/pip" install --quiet 'aiohttp>=3.9.5,<4' 'tomlkit>=0.13.3,<1'
    curl -fsSL -o "$generator.part" \
        "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$tools_commit/cargo/flatpak-cargo-generator.py"
    mv "$generator.part" "$generator"
fi
generate() { "$cache/venv/bin/python" "$generator" "$1" -o "$2"; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
generate Cargo.lock "$work/cargo-sources.json"

case $mode in
--check)
    if diff -q "$flatpak/cargo-sources.json" "$work/cargo-sources.json" >/dev/null; then
        echo "$flatpak/cargo-sources.json matches Cargo.lock."
        exit 0
    fi
    echo "$flatpak/cargo-sources.json is stale; run scripts/flatpak-sources.sh" >&2
    exit 1
    ;;
--flathub)
    tag=${2:?usage: scripts/flatpak-sources.sh --flathub vX.Y.Z <dir>}
    out=${3:?usage: scripts/flatpak-sources.sh --flathub vX.Y.Z <dir>}
    commit=$(git rev-parse "$tag^{commit}")
    # Flathub builds on its own servers, where GitHub's secrets do not
    # reach, so its manifest carries the Google client in the open. Google
    # treats a desktop app's client secret as public, and the .deb holds
    # both values already. The manifest in this repository stays empty.
    if { [ -z "${PENGUIN_MAIL_GOOGLE_CLIENT_ID:-}" ] || [ -z "${PENGUIN_MAIL_GOOGLE_CLIENT_SECRET:-}" ]; } &&
        [ -f packaging/secrets.env ]; then
        set -a
        # shellcheck source=/dev/null
        . packaging/secrets.env
        set +a
    fi
    if [ -z "${PENGUIN_MAIL_GOOGLE_CLIENT_ID:-}" ] || [ -z "${PENGUIN_MAIL_GOOGLE_CLIENT_SECRET:-}" ]; then
        echo "The Flathub manifest needs PENGUIN_MAIL_GOOGLE_CLIENT_ID and" >&2
        echo "PENGUIN_MAIL_GOOGLE_CLIENT_SECRET, in the environment or in" >&2
        echo "packaging/secrets.env; without them Flathub's build cannot sign in to Google." >&2
        exit 1
    fi
    export PENGUIN_MAIL_GOOGLE_CLIENT_ID PENGUIN_MAIL_GOOGLE_CLIENT_SECRET
    # Cargo.lock at the tag is what Flathub builds, so its crates are the
    # ones to list.
    git show "$tag:Cargo.lock" > "$work/Cargo.lock"
    mkdir -p "$out"
    generate "$work/Cargo.lock" "$out/cargo-sources.json"
    cp "$flatpak/flathub.json" "$out/"
    # The checkout source becomes the tag on GitHub. There is no
    # x-checker-data: Flathub's checker would move the tag and commit but
    # not cargo-sources.json, and a release that changed Cargo.lock would
    # then fail to build. Each release runs this script and opens the pull
    # request by hand instead.
    python3 - "$manifest" "$out/io.github.c9dev.PenguinMail.yml" "$tag" "$commit" <<'PY'
import json
import os
import sys

source, target, tag, commit = sys.argv[1:]
text = open(source, encoding="utf-8").read()
for name in ("PENGUIN_MAIL_GOOGLE_CLIENT_ID", "PENGUIN_MAIL_GOOGLE_CLIENT_SECRET"):
    empty = f'{name}: ""'
    if text.count(empty) != 1:
        sys.exit(f"{source} has no single empty {name}")
    # A JSON string is a YAML double-quoted string too.
    text = text.replace(empty, f"{name}: {json.dumps(os.environ[name])}")
start = text.index("      - type: dir\n")
git = (
    "      - type: git\n"
    "        url: https://github.com/c9dev/penguin-mail.git\n"
    f"        tag: {tag}\n"
    f"        commit: {commit}\n"
)
header_end = text.index("id: ")
header = (
    "# Penguin Mail on Flathub. Written by scripts/flatpak-sources.sh --flathub\n"
    "# in https://github.com/c9dev/penguin-mail from the manifest there.\n"
)
open(target, "w", encoding="utf-8").write(header + text[header_end:start] + git)
PY
    echo "Wrote the Flathub files for $tag into $out."
    ;;
"")
    mv "$work/cargo-sources.json" "$flatpak/cargo-sources.json"
    echo "Wrote $flatpak/cargo-sources.json."
    ;;
*)
    echo "usage: scripts/flatpak-sources.sh [--check | --flathub vX.Y.Z <dir>]" >&2
    exit 2
    ;;
esac
