#!/usr/bin/env bash
# Copies a tree laid out by stage.sh into a prefix, ~/.local unless PREFIX
# says otherwise. A release tarball ships this script beside its tree, so
# installing one needs no Rust and no gettext.
#
#   scripts/install-files.sh <tree>
#   NO_AUTOSTART=1 ...   skips starting Penguin Mail at login
#   PREFIX=/opt/penguin-mail ...   installs somewhere else
#
# Upgrading from an install under an earlier name (mailrs, or Penguin Mail
# before it took the io.github.c9dev ID) removes the old launcher and icons
# and carries the login item over. The app moves mailrs's config and mail on
# first start.
set -euo pipefail

tree=${1:?usage: install-files.sh <tree>}
prefix="${PREFIX:-$HOME/.local}"
apps="$prefix/share/applications"
icons="$prefix/share/icons/hicolor"
autostart="$HOME/.config/autostart"
id=io.github.c9dev.PenguinMail
old_ids=(dev.penguinmail.PenguinMail dev.mailrs.Mailrs)

# A running copy keeps its old file open. Each binary lands beside the old
# one and is renamed over it, so a copy that fails halfway leaves the old
# binary whole, and the running copy restarts into the new one.
mkdir -p "$prefix/bin"
for name in penguin-mail penguin-mail-cli; do
    install -m755 "$tree/bin/$name" "$prefix/bin/$name.new"
    mv -f "$prefix/bin/$name.new" "$prefix/bin/$name"
done

mkdir -p "$prefix/share"
cp -r "$tree/share/icons" "$prefix/share/"
if [ -d "$tree/share/locale" ]; then
    cp -r "$tree/share/locale" "$prefix/share/"
fi
mkdir -p "$apps"
# The staged entry runs penguin-mail from PATH, and ~/.local/bin is not on
# the PATH a desktop session starts with. Name the installed file instead.
sed "s|^Exec=penguin-mail|Exec=$prefix/bin/penguin-mail|" \
    "$tree/share/applications/$id.desktop" > "$apps/$id.desktop"
chmod 644 "$apps/$id.desktop"

rm -f "$prefix/bin/mailrs" "$prefix/bin/mailrs-cli"
for old_id in "${old_ids[@]}"; do
    rm -f "$apps/$old_id.desktop" \
        "$icons/scalable/apps/$old_id.svg" \
        "$icons/symbolic/apps/$old_id-symbolic.svg"
done

# An existing login item is the user's choice, on or off: keep it. One left
# under an earlier name moves to the new one and starts the new binary.
carried=
for old_id in "${old_ids[@]}"; do
    [ -e "$autostart/$old_id.desktop" ] || continue
    if [ ! -e "$autostart/$id.desktop" ]; then
        sed -e "s|^Name=.*|Name=Penguin Mail|" \
            -e "s|^Exec=.*|Exec=$prefix/bin/penguin-mail --background|" \
            -e "s|^Icon=.*|Icon=$id|" \
            "$autostart/$old_id.desktop" > "$autostart/$id.desktop"
    fi
    rm -f "$autostart/$old_id.desktop"
    carried=1
done
if [ -z "$carried" ] && [ "${NO_AUTOSTART:-}" != 1 ] && [ ! -e "$autostart/$id.desktop" ]; then
    mkdir -p "$autostart"
    cat > "$autostart/$id.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Penguin Mail
Comment=Keeps Gmail in sync from the system tray
Exec=$prefix/bin/penguin-mail --background
Icon=$id
NoDisplay=true
X-GNOME-Autostart-enabled=true
DESKTOP
fi

update-desktop-database "$apps" >/dev/null 2>&1 || true
gtk-update-icon-cache -q -f -t "$icons" >/dev/null 2>&1 || true

echo "Installed Penguin Mail to $prefix/bin."
if [ -e "$autostart/$id.desktop" ] && ! grep -q 'X-GNOME-Autostart-enabled=false' "$autostart/$id.desktop"; then
    echo "It starts in the tray at your next login; run 'penguin-mail' now to open it."
fi
echo "To make it your mail handler: xdg-mime default $id.desktop x-scheme-handler/mailto"
