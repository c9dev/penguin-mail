#!/usr/bin/env bash
# Builds Penguin Mail for Arch Linux and packs it into a .pkg.tar.zst that
# installs under /usr. makepkg reads pacman's own database to work out
# what a build needs, so run this on Arch, or in an archlinux container.
#
#   scripts/package-arch.sh <version> <out>
#
# The build turns on the packaging-arch feature, so the app leaves updates
# to pacman instead of offering to install a .deb; see package-rpm.sh,
# which does the same for dnf. Besides the staged tree, the PKGBUILD this
# script writes lists the Arch package names for what the binaries link
# against and what they run as external programs (gnupg, and bubblewrap
# for skill scripts).
set -euo pipefail
# Packages must not inherit a group-writable umask from whoever builds them.
umask 022

cd "$(dirname "$0")/.."
version=${1:?usage: scripts/package-arch.sh <version> <out>}
out=${2:?usage: scripts/package-arch.sh <version> <out>}
mkdir -p "$out"
out=$(realpath "$out")
here=$(pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cargo build --release --locked -p mailrs -p mailrs-cli --features mailrs/packaging-arch
tree="$work/tree"
scripts/stage.sh "$tree"

# makepkg reads its instructions from a file named PKGBUILD in the
# directory it runs in. This one has no source of its own: package()
# copies the tree scripts/stage.sh just laid out, the same as the rpm's
# %install and the .deb's DEBIAN/control. pacman has no equivalent of
# dpkg-shlibdeps or rpm's automatic library scan, so every runtime
# dependency below is named by hand; namcap checks the result.
build="$work/build"
mkdir -p "$build"
cat > "$build/PKGBUILD" <<PKGBUILD
# Maintainer: Pivotd <penguin@pivotd.com>
pkgname=penguin-mail
pkgver=$version
pkgrel=1
pkgdesc="Gmail client for GNOME"
arch=('x86_64')
url="https://github.com/c9dev/penguin-mail"
license=('GPL-3.0-or-later')
depends=('gtk4' 'libadwaita' 'webkitgtk-6.0' 'gnupg' 'hicolor-icon-theme')
optdepends=('gnome-shell-extension-appindicator: tray icon on GNOME Shell'
            'bubblewrap: run assistant skill scripts')
# desktop-file-utils is not listed: pacman runs its update-desktop-database
# hook whenever that package happens to be on the system, not only when a
# package installing a .desktop file depends on it, so making it a hard
# dependency here would only be for a hook this package does not own.
options=('!debug')
source=()
sha256sums=()

package() {
    cp -r "$tree"/. "\$pkgdir/usr/"
    install -Dm644 "$here/LICENSE" "\$pkgdir/usr/share/licenses/\$pkgname/LICENSE"
}
PKGBUILD

(
    cd "$build"
    # makepkg refuses to run as root, on the sound theory that a PKGBUILD
    # is arbitrary code; the archlinux container the release workflow
    # builds in starts as root and has no other user, so give it one for
    # this step alone. --nodeps skips makepkg's own dependency check,
    # which would otherwise want the Depends above installed on the
    # builder too, even though package() only copies files that are
    # already built.
    if [ "$(id -u)" -eq 0 ]; then
        useradd -m -s /bin/bash builder
        # mktemp made $work searchable by its owner alone, so builder
        # could not even reach $build without this, despite owning it.
        chmod 711 "$work"
        chown -R builder:builder "$build"
        runuser -u builder -- makepkg --nodeps --noconfirm
    else
        makepkg --nodeps --noconfirm
    fi
)
pkg=$(cd "$build" && ls -- *.pkg.tar.zst)
mv "$build/$pkg" "$out/$pkg"
echo "Packed $out/$pkg"
