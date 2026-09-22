#!/usr/bin/env bash
# Builds the signed apt repository Penguin Mail publishes on GitHub Pages.
#   scripts/apt-repo.sh <debs> <out>
# <debs>: a folder of penguin-mail_*.deb files, every version the
#         repository should offer.
# <out>:  the site to publish. It gets pool/, dists/stable/, the public key
#         and an index page saying how to add the repository.
# Signs with the key in $GNUPGHOME, which must hold only the repository's
# signing key; the release workflow imports it there from a secret.
set -euo pipefail

debs=$1
out=$2
here="$(cd "$(dirname "$0")/.." && pwd)"
suite=stable
component=main
arch=amd64

rm -rf "$out"
pool="$out/pool/$component/p/penguin-mail"
binary="$out/dists/$suite/$component/binary-$arch"
mkdir -p "$pool" "$binary"

shopt -s nullglob
packages=("$debs"/penguin-mail_*_"$arch".deb)
if [ ${#packages[@]} -eq 0 ]; then
    echo "apt-repo.sh: no penguin-mail_*_$arch.deb in $debs" >&2
    exit 1
fi

# One stanza per package: its own control fields, then where it sits and
# its hashes, as dpkg-scanpackages writes them.
: > "$binary/Packages"
for deb in "${packages[@]}"; do
    name=$(basename "$deb")
    cp "$deb" "$pool/$name"
    {
        dpkg-deb -f "$deb"
        echo "Filename: pool/$component/p/penguin-mail/$name"
        echo "Size: $(stat -c %s "$deb")"
        echo "MD5sum: $(md5sum "$deb" | cut -d' ' -f1)"
        echo "SHA256: $(sha256sum "$deb" | cut -d' ' -f1)"
        echo
    } >> "$binary/Packages"
done
gzip -9 -n -k "$binary/Packages"

# The Release file lists every index with its size and hashes. apt checks
# the signature on it, then each index against it.
release="$out/dists/$suite/Release"
{
    echo "Origin: Penguin Mail"
    echo "Label: Penguin Mail"
    echo "Suite: $suite"
    echo "Codename: $suite"
    echo "Architectures: $arch"
    echo "Components: $component"
    echo "Description: Penguin Mail, a Gmail client for GNOME"
    echo "Date: $(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S UTC')"
    for sum in MD5Sum SHA256; do
        echo "$sum:"
        for index in Packages Packages.gz; do
            file="$binary/$index"
            case $sum in
                MD5Sum) hash=$(md5sum "$file" | cut -d' ' -f1) ;;
                SHA256) hash=$(sha256sum "$file" | cut -d' ' -f1) ;;
            esac
            printf ' %s %s %s\n' "$hash" "$(stat -c %s "$file")" \
                "$component/binary-$arch/$index"
        done
    done
} > "$release"

gpg --batch --yes --pinentry-mode loopback --passphrase '' \
    --clearsign -o "$out/dists/$suite/InRelease" "$release"
gpg --batch --yes --pinentry-mode loopback --passphrase '' \
    --armor --detach-sign -o "$out/dists/$suite/Release.gpg" "$release"

cp "$here/packaging/apt/penguin-mail-archive-keyring.gpg" \
    "$here/packaging/apt/penguin-mail-archive-keyring.asc" "$out/"
cp "$here/packaging/apt/penguin-mail.sources" "$out/"
cp "$here/packaging/apt/index.html" "$out/"
# Pages would otherwise run the site through Jekyll, which skips nothing
# here but costs a build.
touch "$out/.nojekyll"
