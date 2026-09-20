#!/usr/bin/env bash
# Rebuilds po/penguin-mail.pot from the source, brings every po/*.po up to
# it, and compiles the catalogues into target/locale so a build-tree copy
# of the app shows them.
#   scripts/update-po.sh              rebuild the template and the .po files
#   scripts/update-po.sh --check      say whether the template is stale
# Needs xtr, which reads Rust properly where xgettext does not:
#   cargo install xtr
set -euo pipefail

cd "$(dirname "$0")/.."
pot=po/penguin-mail.pot
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

if ! command -v xtr >/dev/null; then
    echo "update-po.sh needs xtr: cargo install xtr" >&2
    exit 1
fi
if ! command -v xgettext >/dev/null; then
    echo "update-po.sh needs xgettext from gettext" >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# xtr walks a crate from its root module, so one call covers every file
# under it. Naming a keyword drops the ones it looks for by default, so
# `gettext` and `ngettext` are named again beside `fill_plural`, which
# takes the two forms `ngettext` would. The desktop entry is not Rust and
# goes through xgettext.
for crate in app/src/main.rs domain/src/lib.rs sync/src/lib.rs; do
    xtr -kgettext -kngettext:1,2 -kfill_plural:1,2 \
        -o "$work/$(echo "$crate" | tr / -).pot" "$crate"
done
xgettext --from-code=UTF-8 -L Desktop \
    -o "$work/desktop.pot" app/data/dev.penguinmail.PenguinMail.desktop

msgcat --use-first --sort-by-file -o "$work/joined.pot" "$work"/*.pot
# msgcat needs a header on its inputs; ours replaces it, so drop theirs.
sed '1,/^$/d' "$work/joined.pot" > "$work/merged.pot"
# The header msgcat leaves behind names no package, so write our own.
cat > "$work/header.pot" <<HEADER
# Penguin Mail, a Gmail client for the GNOME desktop.
# This file is distributed under the same licence as Penguin Mail.
#
msgid ""
msgstr ""
"Project-Id-Version: penguin-mail $version\\n"
"Report-Msgid-Bugs-To: https://github.com/c9dev/penguin-mail/issues\\n"
"MIME-Version: 1.0\\n"
"Content-Type: text/plain; charset=UTF-8\\n"
"Content-Transfer-Encoding: 8bit\\n"
"Plural-Forms: nplurals=2; plural=(n != 1);\\n"

HEADER
cat "$work/header.pot" "$work/merged.pot" > "$work/penguin-mail.pot"

if [ "${1:-}" = --check ]; then
    if diff -q <(grep -v '^"POT-Creation-Date' "$pot") \
        <(grep -v '^"POT-Creation-Date' "$work/penguin-mail.pot") >/dev/null; then
        echo "$pot is up to date."
        exit 0
    fi
    echo "$pot is stale; run scripts/update-po.sh" >&2
    exit 1
fi

mv "$work/penguin-mail.pot" "$pot"

# POTFILES.in is the list a translator reads to know where the words come
# from. Nothing builds from it, so say when it has drifted.
listed=$(grep -v '^#' po/POTFILES.in | grep -v '^$' | LC_ALL=C sort)
found=$(sed -n 's/^#: //p' "$pot" | tr ' ' '\n' | sed 's/:[0-9]*$//' |
    grep -v '^$' | LC_ALL=C sort -u)
missing=$(comm -13 <(echo "$listed") <(echo "$found") || true)
if [ -n "$missing" ]; then
    echo "po/POTFILES.in is missing:" >&2
    echo "$missing" | sed 's/^/  /' >&2
fi

# LINGUAS names the languages that exist. msgfmt reads it to put the
# translated Name and Comment into the desktop entry.
printf '# The languages po/ holds, one per line. update-po.sh writes this.\n' \
    > po/LINGUAS
for po in po/*.po; do
    [ -e "$po" ] || continue
    basename "$po" .po >> po/LINGUAS
done

mkdir -p target/locale
for po in po/*.po; do
    [ -e "$po" ] || continue
    lang=$(basename "$po" .po)
    msgmerge --quiet --update --backup=none --previous "$po" "$pot"
    install -Dm644 /dev/null "target/locale/$lang/LC_MESSAGES/penguin-mail.mo"
    msgfmt --check --statistics -o "target/locale/$lang/LC_MESSAGES/penguin-mail.mo" "$po"
done

echo "Wrote $pot and compiled po/*.po into target/locale."
