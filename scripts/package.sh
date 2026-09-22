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
# A binary package's control file has no License field. Debian keeps the
# licence in the copyright file under /usr/share/doc instead, and lintian
# and the package managers look for it there.
install -d "$root/usr/share/doc/penguin-mail"
cat > "$root/usr/share/doc/penguin-mail/copyright" <<COPYRIGHT
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: Penguin Mail
Upstream-Contact: https://github.com/c9dev/penguin-mail/issues
Source: https://github.com/c9dev/penguin-mail

Files: *
Copyright: 2026 The Penguin Mail authors
License: GPL-3.0+

License: GPL-3.0+
 This program is free software: you can redistribute it and/or modify it
 under the terms of the GNU General Public License as published by the Free
 Software Foundation, either version 3 of the License, or (at your option)
 any later version.
 .
 This program is distributed in the hope that it will be useful, but WITHOUT
 ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
 FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
 more details.
 .
 On Debian systems, the full text of the GNU General Public License version
 3 can be found in /usr/share/common-licenses/GPL-3.
COPYRIGHT
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
# dpkg drops the files an earlier .deb shipped under the old app ID. The
# rm catches the same names when something else left them, such as
# install-files.sh run with PREFIX=/usr, so the menu shows one Penguin Mail.
cat > "$root/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
if [ "$1" = configure ]; then
    rm -f /usr/share/applications/dev.penguinmail.PenguinMail.desktop \
        /usr/share/icons/hicolor/scalable/apps/dev.penguinmail.PenguinMail.svg \
        /usr/share/icons/hicolor/symbolic/apps/dev.penguinmail.PenguinMail-symbolic.svg
    update-desktop-database -q /usr/share/applications || true
    gtk-update-icon-cache -q -f -t /usr/share/icons/hicolor || true
fi
POSTINST
cat > "$root/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
set -e
if [ "$1" = remove ]; then
    update-desktop-database -q /usr/share/applications || true
    gtk-update-icon-cache -q -f -t /usr/share/icons/hicolor || true
fi
POSTRM
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
