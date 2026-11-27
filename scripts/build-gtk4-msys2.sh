#!/usr/bin/env bash
# Builds MSYS2's UCRT64 gtk4 package with packaging/windows/gtk4-zero-monitor-popup.patch added,
# leaves it in OUT_DIR and installs it over the stock package.
#
#   scripts/build-gtk4-msys2.sh MINGW_PACKAGES_CHECKOUT [OUT_DIR]
#
# Run it from an MSYS2 UCRT64 shell with base-devel installed. The checkout is msys2/MINGW-packages
# at the commit the CI pins. A package already in OUT_DIR is installed without building again.
set -euo pipefail
cd "$(dirname "$0")/.."

recipes=${1:?usage: build-gtk4-msys2.sh MINGW_PACKAGES_CHECKOUT [OUT_DIR]}
out=$(mkdir -p "${2:-dist/gtk4-package}" && cd "${2:-dist/gtk4-package}" && pwd)
patch_file=$PWD/packaging/windows/gtk4-zero-monitor-popup.patch
# The SHA-256 of the GTK 4.24.2 tarball, which the patch was checked against. A recipe that moves
# GTK 4 to another version stops here until the patch is checked against it.
gtk4_hash=05561abc25d1b1400136bf79ab68830cb4b053d1922ae97e551b3f19755bf3cb

package() { ls "$out"/mingw-w64-ucrt-x86_64-gtk4-[0-9]*.pkg.tar.zst 2>/dev/null | head -1; }

if [ -z "$(package)" ]; then
    work=$(mktemp -d)
    cp -r "$recipes/mingw-w64-gtk4/." "$work/"
    grep -q "'$gtk4_hash'" "$work/PKGBUILD" || {
        echo "the gtk4 recipe no longer builds GTK 4.24.2; check the patch against its GTK and update gtk4_hash" >&2
        exit 1
    }
    cp "$patch_file" "$work/"
    # makepkg sources the recipe, so the patch goes in by wrapping the recipe's own prepare().
    # The .1 release marks the build as ours in pacman's database.
    cat >> "$work/PKGBUILD" <<EOF

source+=("gtk4-zero-monitor-popup.patch")
sha256sums+=('$(sha256sum "$patch_file" | cut -d' ' -f1)')
pkgrel=\${pkgrel}.1
eval "_stock_\$(declare -f prepare)"
prepare() {
  _stock_prepare
  cd "\${srcdir}/gtk-\${pkgver}"
  patch -Np1 -i ../gtk4-zero-monitor-popup.patch
}
EOF
    (cd "$work" && MINGW_ARCH=ucrt64 makepkg-mingw --noconfirm --syncdeps --nocheck --cleanbuild --force)
    cp "$work"/mingw-w64-ucrt-x86_64-gtk4-[0-9]*.pkg.tar.zst "$out/"
    rm -rf "$work"
fi

pacman -U --noconfirm "$(package)"
