#!/usr/bin/env bash
# Packs a tree laid out by stage.sh into an rpm that installs under /usr,
# for Fedora and the distributions built from it. Run it on the oldest
# Fedora the rpm should install on, after building there: rpm reads the
# libraries the binaries link against and requires each one, so they must
# be Fedora's own.
#
#   scripts/package-rpm.sh <tree> <version> <out>
#   RPM_SIGN_KEY=<fingerprint> ...   also signs the rpm with that key,
#                                    which must be in gpg's keyring
#
# Besides the tree, the rpm carries the repository's public key and a
# .repo file, so a person who installs it once gets later versions from
# `dnf upgrade`, as the .deb does with apt.
set -euo pipefail
# Packages must not inherit a group-writable umask from whoever builds them.
umask 022

cd "$(dirname "$0")/.."
tree=${1:?usage: scripts/package-rpm.sh <tree> <version> <out>}
version=${2:?usage: scripts/package-rpm.sh <tree> <version> <out>}
out=${3:?usage: scripts/package-rpm.sh <tree> <version> <out>}
tree=$(realpath "$tree")
mkdir -p "$out"
out=$(realpath "$out")
here=$(pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/SPECS" "$work/RPMS" "$work/BUILD"
cat > "$work/SPECS/penguin-mail.spec" <<SPEC
# The binaries arrive built, so there is nothing to compile and no
# debug information to split out.
%global debug_package %{nil}
%global _build_id_links none

Name:           penguin-mail
Version:        $version
Release:        1
Summary:        Gmail client for GNOME
License:        GPL-3.0-or-later
URL:            https://github.com/c9dev/penguin-mail
ExclusiveArch:  x86_64

Requires:       gtk4 >= 4.20
Requires:       libadwaita >= 1.8
Requires:       webkitgtk6.0
Requires:       gnupg2
Recommends:     gnupg2-smime
Recommends:     bubblewrap
Recommends:     gnome-shell-extension-appindicator

%description
Reads, sorts and sends mail for several Gmail accounts, keeps them in
sync from the system tray, and signs and encrypts with OpenPGP or S/MIME.

%install
mkdir -p %{buildroot}/usr
cp -r $tree/. %{buildroot}/usr/
install -Dm644 $here/packaging/apt/penguin-mail-archive-keyring.asc \\
    %{buildroot}/etc/pki/rpm-gpg/RPM-GPG-KEY-penguin-mail
install -Dm644 $here/packaging/rpm/penguin-mail.repo \\
    %{buildroot}/etc/yum.repos.d/penguin-mail.repo
install -Dm644 $here/LICENSE %{buildroot}/usr/share/licenses/penguin-mail/LICENSE

%files
%license /usr/share/licenses/penguin-mail/LICENSE
/usr/bin/penguin-mail
/usr/bin/penguin-mail-cli
/usr/share/applications/io.github.c9dev.PenguinMail.desktop
/usr/share/metainfo/io.github.c9dev.PenguinMail.metainfo.xml
/usr/share/icons/hicolor/scalable/apps/io.github.c9dev.PenguinMail.svg
/usr/share/icons/hicolor/symbolic/apps/io.github.c9dev.PenguinMail-symbolic.svg
/usr/share/locale/*/LC_MESSAGES/penguin-mail.mo
/etc/pki/rpm-gpg/RPM-GPG-KEY-penguin-mail
# dnf keeps a .repo file the person edited or removed, as dpkg keeps a
# conffile.
%config(noreplace) /etc/yum.repos.d/penguin-mail.repo
SPEC

rpmbuild --quiet -bb \
    --define "_topdir $work" \
    --define "_rpmdir $work/RPMS" \
    "$work/SPECS/penguin-mail.spec"
rpm="penguin-mail-$version-1.x86_64.rpm"
mv "$work/RPMS/x86_64/$rpm" "$out/$rpm"

if [ -n "${RPM_SIGN_KEY:-}" ]; then
    rpmsign --addsign \
        --define "_gpg_name $RPM_SIGN_KEY" \
        --define "_gpg_sign_cmd_extra_args --pinentry-mode loopback --passphrase ''" \
        "$out/$rpm" >/dev/null
fi

echo "Packed $out/$rpm"
