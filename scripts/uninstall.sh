#!/usr/bin/env bash
# Removes what install.sh added. Your mail cache, config, and keyring entries stay.
set -euo pipefail

prefix="${PREFIX:-$HOME/.local}"
rm -f "$prefix/bin/mailrs" "$prefix/bin/mailrs-cli" \
    "$prefix/share/applications/dev.mailrs.Mailrs.desktop" \
    "$prefix/share/icons/hicolor/scalable/apps/dev.mailrs.Mailrs.svg" \
    "$prefix/share/icons/hicolor/symbolic/apps/dev.mailrs.Mailrs-symbolic.svg" \
    "$HOME/.config/autostart/dev.mailrs.Mailrs.desktop"
update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
echo "Removed mailrs. Delete ~/.local/share/mailrs and ~/.config/mailrs too if you want your data gone."
