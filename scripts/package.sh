#!/usr/bin/env bash
# Packs a tree laid out by stage.sh into the files a release publishes, in
# <out>: a .deb that installs under /usr, a tarball and a zip that install
# under ~/.local through install-files.sh, and SHA256SUMS over the three.
#
#   scripts/package.sh <tree> <version> <out>
set -euo pipefail
# Packages must not inherit a group-writable umask from whoever builds them.
umask 022

cd "$(dirname "$0")/.."
tree=${1:?usage: scripts/package.sh <tree> <version> <out>}
version=${2:?usage: scripts/package.sh <tree> <version> <out>}
out=${3:?usage: scripts/package.sh <tree> <version> <out>}
tree=$(realpath "$tree")
mkdir -p "$out"
out=$(realpath "$out")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The .deb. dpkg-shlibdeps reads the binaries' library needs and names the
# Ubuntu packages that hold them, so Depends follows whatever the build
# linked. It insists on a debian/control, so give it a minimal one.
root="$work/deb"
mkdir -p "$root/usr" "$root/DEBIAN"
cp -r "$tree/." "$root/usr/"
# The apt repository's key and entry, so a person who installs this .deb
# once gets later versions from `apt upgrade`. The entry sits under /etc,
# so it is a conffile: dpkg keeps it if the person edits or removes it.
install -Dm644 "packaging/apt/penguin-mail-archive-keyring.gpg" \
    "$root/usr/share/keyrings/penguin-mail-archive-keyring.gpg"
install -Dm644 "packaging/apt/penguin-mail.sources" \
    "$root/etc/apt/sources.list.d/penguin-mail.sources"
echo /etc/apt/sources.list.d/penguin-mail.sources > "$root/DEBIAN/conffiles"
mkdir -p "$work/shlibs/debian"
printf 'Source: penguin-mail\n\nPackage: penguin-mail\nArchitecture: amd64\n' \
    > "$work/shlibs/debian/control"
depends=$(cd "$work/shlibs" && dpkg-shlibdeps -O \
    "$root/usr/bin/penguin-mail" "$root/usr/bin/penguin-mail-cli" \
    | sed -n 's/^shlibs:Depends=//p')
size=$(du -sk --exclude=DEBIAN "$root" | cut -f1)
cat > "$root/DEBIAN/control" <<CONTROL
Package: penguin-mail
Version: $version
Architecture: amd64
Maintainer: Pivotd <penguin@pivotd.com>
Installed-Size: $size
Depends: $depends
Recommends: gnupg, gpgsm, gnome-shell-extension-appindicator
Section: mail
Priority: optional
Homepage: https://github.com/c9dev/penguin-mail
Description: Gmail client for GNOME
 Reads, sorts and sends mail for several Gmail accounts, keeps them in
 sync from the system tray, and signs and encrypts with OpenPGP or S/MIME.
CONTROL
cat > "$root/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    update-desktop-database -q /usr/share/applications || true
    gtk-update-icon-cache -q -f -t /usr/share/icons/hicolor || true
fi
POSTINST
cp "$root/DEBIAN/postinst" "$root/DEBIAN/postrm"
sed -i 's/= configure/= remove/' "$root/DEBIAN/postrm"
chmod 755 "$root/DEBIAN/postinst" "$root/DEBIAN/postrm"
deb="penguin-mail_${version}_amd64.deb"
dpkg-deb --root-owner-group -Zxz --build "$root" "$out/$deb" >/dev/null

# The tarball and the zip hold the same folder: the tree, the script that
# installs it, and the licence.
name="penguin-mail-$version-x86_64"
mkdir -p "$work/$name"
cp -r "$tree/." "$work/$name/"
cp scripts/install-files.sh LICENSE "$work/$name/"
cat > "$work/$name/README" <<README
Penguin Mail $version

Install for your user under ~/.local:
    ./install-files.sh .

It needs GTK 4, libadwaita 1.8 and WebKitGTK 6.0, as on Ubuntu 26.04:
    sudo apt install libgtk-4-1 libadwaita-1-0 libwebkitgtk-6.0-4
README
tar -C "$work" -czf "$out/$name.tar.gz" "$name"
(cd "$work" && zip -qr "$out/$name.zip" "$name")

(cd "$out" && sha256sum "$deb" "$name.tar.gz" "$name.zip" > SHA256SUMS)
echo "Packed into $out:"
(cd "$out" && ls -1 "$deb" "$name.tar.gz" "$name.zip" SHA256SUMS)
