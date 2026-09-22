#!/usr/bin/env bash
# Removes what install.sh added, and what an install under an earlier name left.
# Your mail cache, config, and keyring entries stay.
set -euo pipefail

prefix="${PREFIX:-$HOME/.local}"
for id in io.github.c9dev.PenguinMail dev.penguinmail.PenguinMail dev.mailrs.Mailrs; do
    rm -f "$prefix/share/applications/$id.desktop" \
        "$prefix/share/icons/hicolor/scalable/apps/$id.svg" \
        "$prefix/share/icons/hicolor/symbolic/apps/$id-symbolic.svg" \
        "$HOME/.config/autostart/$id.desktop"
done
rm -f "$prefix/bin/penguin-mail" "$prefix/bin/penguin-mail-cli" \
    "$prefix/bin/mailrs" "$prefix/bin/mailrs-cli"
update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
echo "Removed Penguin Mail. Delete ~/.local/share/penguin-mail and ~/.config/penguin-mail too if you want your data gone."
