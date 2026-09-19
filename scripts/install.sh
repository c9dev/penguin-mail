#!/usr/bin/env bash
# Builds mailrs and installs it for the current user under ~/.local.
#   NO_AUTOSTART=1 scripts/install.sh   skips starting mailrs at login
#   PREFIX=/opt/mailrs scripts/install.sh   installs somewhere else
set -euo pipefail

cd "$(dirname "$0")/.."
prefix="${PREFIX:-$HOME/.local}"
apps="$prefix/share/applications"
icons="$prefix/share/icons/hicolor"

cargo build --release -p mailrs -p mailrs-cli

install -Dm755 target/release/mailrs "$prefix/bin/mailrs"
install -Dm755 target/release/mailrs-cli "$prefix/bin/mailrs-cli"
install -Dm644 app/data/icons/scalable/apps/dev.mailrs.Mailrs.svg "$icons/scalable/apps/dev.mailrs.Mailrs.svg"
install -Dm644 app/data/icons/scalable/apps/dev.mailrs.Mailrs-symbolic.svg "$icons/symbolic/apps/dev.mailrs.Mailrs-symbolic.svg"
mkdir -p "$apps"
sed "s|^Exec=mailrs|Exec=$prefix/bin/mailrs|" app/data/dev.mailrs.Mailrs.desktop > "$apps/dev.mailrs.Mailrs.desktop"

if [ "${NO_AUTOSTART:-}" != 1 ]; then
    mkdir -p "$HOME/.config/autostart"
    cat > "$HOME/.config/autostart/dev.mailrs.Mailrs.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=mailrs
Comment=Keeps Gmail in sync from the system tray
Exec=$prefix/bin/mailrs --background
Icon=dev.mailrs.Mailrs
NoDisplay=true
X-GNOME-Autostart-enabled=true
DESKTOP
fi

update-desktop-database "$apps" >/dev/null 2>&1 || true
gtk-update-icon-cache -q -f -t "$icons" >/dev/null 2>&1 || true

echo "Installed mailrs to $prefix/bin."
[ "${NO_AUTOSTART:-}" != 1 ] && echo "It starts in the tray at your next login; run 'mailrs' now to open it."
echo "To make it your mail handler: xdg-mime default dev.mailrs.Mailrs.desktop x-scheme-handler/mailto"
