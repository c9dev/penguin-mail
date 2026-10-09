#!/usr/bin/env bash
# Builds the signed Flatpak repository served beside the apt and dnf repos.
#   scripts/flatpak-repo.sh <bundles> <out> <signing-key>
# The Pages workflow passes the newest release bundle and uses the same
# signing key as the other package repositories.
set -euo pipefail

bundles_dir=$1
out=$2
key=$3
repo=$out/repo
url=https://c9dev.github.io/penguin-mail/flatpak/repo/

shopt -s nullglob
bundles=("$bundles_dir"/*.flatpak)
if [ ${#bundles[@]} -ne 1 ]; then
    echo "flatpak-repo.sh: expected one release bundle in $bundles_dir, found ${#bundles[@]}" >&2
    exit 1
fi

rm -rf "$out"
mkdir -p "$out"
gpg --batch --export "$key" > "$out/penguin-mail.gpg"
if [ ! -s "$out/penguin-mail.gpg" ]; then
    echo "flatpak-repo.sh: no public key for $key" >&2
    exit 1
fi
public_key=$(base64 --wrap=0 < "$out/penguin-mail.gpg")

ostree init --repo="$repo" --mode=archive-z2
flatpak build-import-bundle --gpg-sign="$key" --update-appstream \
    "$repo" "${bundles[0]}"
if ! ostree --repo="$repo" refs | grep -Fxq \
    'app/io.github.c9dev.penguin-mail/x86_64/stable'; then
    echo "flatpak-repo.sh: the bundle has no x86_64 stable Penguin Mail app" >&2
    exit 1
fi
flatpak build-update-repo --gpg-sign="$key" \
    --title="Penguin Mail" \
    --comment="Mail and calendar for Linux" \
    --description="Penguin Mail releases" \
    --homepage="https://github.com/c9dev/penguin-mail" \
    --default-branch=stable \
    --generate-static-deltas --static-delta-jobs=2 --prune "$repo"

cat > "$out/penguin-mail.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Penguin Mail
Url=$url
Homepage=https://github.com/c9dev/penguin-mail
Comment=Mail and calendar for Linux
Description=Penguin Mail releases
GPGKey=$public_key
EOF

cat > "$out/penguin-mail.flatpakref" <<EOF
[Flatpak Ref]
Name=io.github.c9dev.penguin-mail
Branch=stable
Title=Penguin Mail
Url=$url
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
IsRuntime=false
GPGKey=$public_key
EOF
