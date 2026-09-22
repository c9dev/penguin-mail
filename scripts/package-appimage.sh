#!/usr/bin/env bash
# Packs a tree laid out by stage.sh into an AppImage: one file that runs
# with nothing installed, carrying GTK, libadwaita, WebKitGTK and the
# libraries under them. Run it on the oldest system the app builds on,
# since the AppImage runs on that glibc and newer ones only.
#
#   scripts/package-appimage.sh <tree> <version> <out>
#
# The tree's binaries must be built with --features packaging-appimage, so
# the app updates itself by replacing the AppImage. Needs curl, file,
# patchelf, dpkg-architecture, glib-compile-schemas,
# gdk-pixbuf-query-loaders, and a GTK 4 input method module such as
# ibus-gtk4's, since the GTK plugin stops when GTK's module folder is
# missing. linuxdeploy, its GTK plugin and appimagetool are downloaded
# into $APPIMAGE_TOOLS, or a scratch folder.
#
# GnuPG stays out of the image: signing and encryption use the system's
# gpg and gpgsm, and the app says so when they are missing, as it does
# anywhere else.
set -euo pipefail
umask 022

cd "$(dirname "$0")/.."
tree=${1:?usage: scripts/package-appimage.sh <tree> <version> <out>}
version=${2:?usage: scripts/package-appimage.sh <tree> <version> <out>}
out=${3:?usage: scripts/package-appimage.sh <tree> <version> <out>}
tree=$(realpath "$tree")
mkdir -p "$out"
out=$(realpath "$out")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
id=io.github.c9dev.PenguinMail
name="penguin-mail-$version-x86_64.AppImage"

tools=${APPIMAGE_TOOLS:-$work/tools}
mkdir -p "$tools"
fetch() {
    [ -x "$tools/$1" ] && return
    curl -fsSL -o "$tools/$1" "$2"
    chmod +x "$tools/$1"
}
fetch linuxdeploy \
    https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage
fetch linuxdeploy-plugin-gtk.sh \
    https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh
fetch appimagetool \
    https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
export PATH="$tools:$PATH"
# A build container has no FUSE to mount the tools with.
export APPIMAGE_EXTRACT_AND_RUN=1

appdir="$work/AppDir"
mkdir -p "$appdir/usr"
cp -r "$tree/." "$appdir/usr/"

# WebKitGTK runs each page in helper processes it finds in a folder fixed
# at its own build time, under /usr. Copy the helpers into the same place
# inside the image; the library is pointed at them further down.
libdir=$(pkg-config --variable=libdir webkitgtk-6.0)
rel=${libdir#/usr/}
mkdir -p "$appdir/usr/$rel"
cp -r "$libdir/webkitgtk-6.0" "$appdir/usr/$rel/"
rm -f "$appdir/usr/$rel/webkitgtk-6.0/MiniBrowser"
# libsoup, in the network process, speaks TLS through GIO's GnuTLS module,
# which nothing links to, so linuxdeploy would not find it.
mkdir -p "$appdir/usr/$rel/gio/modules"
cp "$libdir/gio/modules/libgiognutls.so" "$appdir/usr/$rel/gio/modules/"
# GTK draws its symbolic icons from the Adwaita theme, which a desktop
# other than GNOME may lack.
mkdir -p "$appdir/usr/share/icons"
cp -r /usr/share/icons/Adwaita "$appdir/usr/share/icons/"

mkdir -p "$appdir/apprun-hooks"
cat > "$appdir/apprun-hooks/penguin-mail.sh" <<HOOK
# This runs after the GTK plugin's hook. That hook forces GTK's own
# Adwaita theme, which fights libadwaita's stylesheet, and X11, which
# GTK 4 has no need of on Wayland. libadwaita follows the desktop's light
# or dark setting through the portal by itself.
unset GTK_THEME GDK_BACKEND
# The copied libwebkitgtk looks for its helpers at ././/$rel/webkitgtk-6.0,
# a path relative to the working folder, so start from usr/.
cd "\$APPDIR/usr" || exit 1
export WEBKIT_INJECTED_BUNDLE_PATH="\$APPDIR/usr/$rel/webkitgtk-6.0/injected-bundle"
export GIO_MODULE_DIR="\$APPDIR/usr/$rel/gio/modules"
# WebKit's own sandbox runs its helpers under the system's bubblewrap,
# which mounts the system's /usr and not this image, so the helpers would
# find none of their libraries. Pages still load with scripts off and
# remote content blocked, as everywhere else.
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
if ! command -v gpg >/dev/null 2>&1; then
    echo "penguin-mail: gpg is not installed; signing and encryption need GnuPG from your distribution" >&2
fi
HOOK

DEPLOY_GTK_VERSION=4 linuxdeploy --appdir "$appdir" \
    --executable "$appdir/usr/bin/penguin-mail" \
    --executable "$appdir/usr/bin/penguin-mail-cli" \
    --desktop-file "$appdir/usr/share/applications/$id.desktop" \
    --icon-file "$appdir/usr/share/icons/hicolor/scalable/apps/$id.svg" \
    --deploy-deps-only "$appdir/usr/$rel/webkitgtk-6.0" \
    --deploy-deps-only "$appdir/usr/$rel/webkitgtk-6.0/injected-bundle" \
    --deploy-deps-only "$appdir/usr/$rel/gio/modules" \
    --plugin gtk

# The same trick every WebKitGTK AppImage uses: swap /usr for ././ in the
# library's compiled-in helper path. The string keeps its length, and the
# hook above makes the relative path land inside the image.
webkit=$(find "$appdir/usr/lib" -name 'libwebkitgtk-6.0.so.*' -type f | head -n 1)
[ -n "$webkit" ] || { echo "package-appimage.sh: linuxdeploy did not copy libwebkitgtk" >&2; exit 1; }
sed -i "s|/usr/$rel/webkitgtk-6.0|././/$rel/webkitgtk-6.0|g" "$webkit"
grep -q "././/$rel/webkitgtk-6.0" "$webkit" || {
    echo "package-appimage.sh: the helper path in $webkit did not change" >&2
    exit 1
}

# appimagetool writes the update information into the image, so
# AppImageUpdate and similar tools find newer releases, and writes the
# .zsync file they read beside it.
ARCH=x86_64 VERSION=$version appimagetool --no-appstream \
    -u "gh-releases-zsync|c9dev|penguin-mail|latest|penguin-mail-*-x86_64.AppImage.zsync" \
    "$appdir" "$out/$name"
if [ -e "$name.zsync" ]; then
    mv "$name.zsync" "$out/"
fi
echo "Packed $out/$name"
