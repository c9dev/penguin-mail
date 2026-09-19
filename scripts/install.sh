#!/usr/bin/env bash
# Builds Penguin Mail and installs it for the current user under ~/.local.
#   NO_AUTOSTART=1 scripts/install.sh   skips starting Penguin Mail at login
#   PREFIX=/opt/penguin-mail scripts/install.sh   installs somewhere else
# Upgrading from mailrs removes its binaries, launcher, and icons, and carries
# its login item over. The app moves its config and mail on first start.
set -euo pipefail

cd "$(dirname "$0")/.."
prefix="${PREFIX:-$HOME/.local}"
apps="$prefix/share/applications"
icons="$prefix/share/icons/hicolor"
autostart="$HOME/.config/autostart"
id=dev.penguinmail.PenguinMail
old_id=dev.mailrs.Mailrs

cargo build --release -p mailrs -p mailrs-cli

install -Dm755 target/release/penguin-mail "$prefix/bin/penguin-mail"
install -Dm755 target/release/penguin-mail-cli "$prefix/bin/penguin-mail-cli"
install -Dm644 "app/data/icons/scalable/apps/$id.svg" "$icons/scalable/apps/$id.svg"
install -Dm644 "app/data/icons/scalable/apps/$id-symbolic.svg" "$icons/symbolic/apps/$id-symbolic.svg"
mkdir -p "$apps"
sed "s|^Exec=penguin-mail|Exec=$prefix/bin/penguin-mail|" "app/data/$id.desktop" > "$apps/$id.desktop"

rm -f "$prefix/bin/mailrs" "$prefix/bin/mailrs-cli" \
    "$apps/$old_id.desktop" \
    "$icons/scalable/apps/$old_id.svg" \
    "$icons/symbolic/apps/$old_id-symbolic.svg"

# An existing login item is the user's choice, on or off: keep it. One left
# by mailrs moves to the new name and starts the new binary.
if [ -e "$autostart/$old_id.desktop" ]; then
    if [ ! -e "$autostart/$id.desktop" ]; then
        sed -e "s|^Name=.*|Name=Penguin Mail|" \
            -e "s|^Exec=.*|Exec=$prefix/bin/penguin-mail --background|" \
            -e "s|^Icon=.*|Icon=$id|" \
            "$autostart/$old_id.desktop" > "$autostart/$id.desktop"
    fi
    rm -f "$autostart/$old_id.desktop"
elif [ "${NO_AUTOSTART:-}" != 1 ] && [ ! -e "$autostart/$id.desktop" ]; then
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
