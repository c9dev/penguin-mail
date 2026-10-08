#!/usr/bin/env bash
# Lays out a built Penguin Mail as the tree it installs as, under <dir>:
# bin/, share/applications/, share/icons/, share/locale/, share/metainfo/.
# The binaries come from target/release, so build first. install-files.sh
# copies the tree into a prefix, and the package scripts pack it for a
# release.
#
#   scripts/stage.sh <dir> [app-id]
#
# app-id names the installed desktop file, icons and metainfo, and every
# mention of the ID inside them. Only the Flatpak passes one: Flathub
# wants io.github.c9dev.penguin-mail, from the repository's name, while
# every other package keeps io.github.c9dev.PenguinMail. The binary must
# be built with the same ID (the packaging-flatpak feature).
set -euo pipefail
# Packages must not inherit a group-writable umask from whoever builds them.
umask 022

cd "$(dirname "$0")/.."
dir=${1:?usage: scripts/stage.sh <dir> [app-id]}
id=io.github.c9dev.PenguinMail
to=${2:-$id}
bin=${CARGO_TARGET_DIR:-target}/release

install -Dm755 "$bin/penguin-mail" "$dir/bin/penguin-mail"
install -Dm755 "$bin/penguin-mail-cli" "$dir/bin/penguin-mail-cli"
install -Dm644 "app/data/icons/scalable/apps/$id.svg" \
    "$dir/share/icons/hicolor/scalable/apps/$to.svg"
install -Dm644 "app/data/icons/scalable/apps/$id-symbolic.svg" \
    "$dir/share/icons/hicolor/symbolic/apps/$to-symbolic.svg"
# A drawing of its own for 16 px, on whole pixels, so menus and lists show a
# sharp icon rather than the large one scaled down to a blur. A size folder
# takes a PNG, rendered from the SVG beside it with
# `rsvg-convert -w 16 -h 16`; Flathub's linter refuses an SVG there.
install -Dm644 "app/data/icons/16x16/apps/$id.png" \
    "$dir/share/icons/hicolor/16x16/apps/$to.png"
mkdir -p "$dir/share/applications" "$dir/share/metainfo"
metainfo="$dir/share/metainfo/$to.metainfo.xml"

# The translations need msgfmt from gettext. Without it the app still runs,
# in English, so say so and carry on.
if command -v msgfmt >/dev/null; then
    for po in po/*.po; do
        lang=$(basename "$po" .po)
        mkdir -p "$dir/share/locale/$lang/LC_MESSAGES"
        msgfmt -o "$dir/share/locale/$lang/LC_MESSAGES/penguin-mail.mo" "$po"
    done
    msgfmt --desktop --template="app/data/$id.desktop" -d po \
        -o "$dir/share/applications/$to.desktop"
    chmod 644 "$dir/share/applications/$to.desktop"
    # msgfmt reads the metainfo through the ITS rules AppStream installs.
    # Without them the store listing stays in English, as below.
    if ! msgfmt --xml --template="app/data/$id.metainfo.xml" -d po -o "$metainfo" 2>/dev/null; then
        install -Dm644 "app/data/$id.metainfo.xml" "$metainfo"
    fi
    chmod 644 "$metainfo"
else
    echo "msgfmt is missing, so Penguin Mail will speak English only."
    echo "Install gettext and run this again for the other languages."
    install -Dm644 "app/data/$id.desktop" "$dir/share/applications/$to.desktop"
    install -Dm644 "app/data/$id.metainfo.xml" "$metainfo"
fi

# The ID inside the files follows their names: the launcher's icon and
# window class, the metainfo's id and launchable.
if [ "$to" != "$id" ]; then
    old=${id//./\\.}
    sed -i "s/$old/$to/g" "$dir/share/applications/$to.desktop" "$metainfo"
fi
