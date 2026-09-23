#!/usr/bin/env bash
# Rebuilds po/penguin-mail.pot from the source, brings every po/*.po up to
# it, and compiles the catalogues into target/locale so a build-tree copy
# of the app shows them.
#   scripts/update-po.sh              rebuild the template and the .po files
#   scripts/update-po.sh --check      say whether the template is stale
# Needs xtr, which reads Rust where xgettext only guesses:
#   cargo install xtr
set -euo pipefail

cd "$(dirname "$0")/.."
pot=po/penguin-mail.pot
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

# `gettext`, `ngettext` and `pgettext` are what the C library offers;
# `fill_plural` takes the two forms `ngettext` would and is named beside them.
keywords=(-kgettext -kngettext:1,2 -kpgettext:1c,2 -kfill_plural:1,2)

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

listed=$(grep -v '^#' po/POTFILES.in | grep -v '^$' | grep '\.rs$' | LC_ALL=C sort)

# One xtr per file, rather than one per crate. xtr walks a crate from its
# root, but two modules that share a name, such as `ui::invitation` and
# `ui::window::invitation`, leave it reading only one of them. Naming each
# file makes every one of them a root, and msgcat throws the repeats away.
number=0
while read -r file; do
    number=$((number + 1))
    xtr "${keywords[@]}" -o "$(printf '%s/1-%03d.pot' "$work" "$number")" "$file"
done <<< "$listed"
# The desktop entry and the AppStream metainfo are not Rust at all. xgettext
# reads the metainfo through the ITS rules AppStream installs, which leave
# out the release notes marked translate="no".
xgettext --from-code=UTF-8 -L Desktop \
    -o "$work/2-desktop.pot" app/data/io.github.c9dev.PenguinMail.desktop
xgettext --from-code=UTF-8 \
    -o "$work/3-metainfo.pot" app/data/io.github.c9dev.PenguinMail.metainfo.xml

msgcat --use-first --sort-by-file -o "$work/joined.pot" "$work"/[123]-*.pot
# msgcat needs a header on its inputs; ours replaces it, so drop theirs.
sed '1,/^$/d' "$work/joined.pot" > "$work/merged.pot"
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
    if ! diff -q "$pot" "$work/penguin-mail.pot" >/dev/null; then
        echo "$pot is stale; run scripts/update-po.sh" >&2
        exit 1
    fi
    # British English is written from the template, so it goes stale with
    # it: a string added since the last run has no British spelling yet.
    python3 scripts/en-gb.py "$pot" "$work/en_GB.po"
    if ! diff -q po/en_GB.po "$work/en_GB.po" >/dev/null 2>&1; then
        echo "po/en_GB.po is stale; run scripts/update-po.sh" >&2
        exit 1
    fi
    echo "$pot and po/en_GB.po are up to date."
    exit 0
fi

mv "$work/penguin-mail.pot" "$pot"
# British English comes from the template and the spelling rules in
# scripts/en-gb.py, never by hand, so every string has it.
python3 scripts/en-gb.py "$pot" po/en_GB.po

# POTFILES.in is what the extraction reads, so a file left out of it is a
# file whose words nobody can translate. Say so loudly.
grep -rlE '(gettext|fill_plural)\(' --include='*.rs' app/src domain/src sync/src |
    LC_ALL=C sort > "$work/rust.txt"
forgotten=$(comm -13 <(echo "$listed") "$work/rust.txt")
if [ -n "$forgotten" ]; then
    echo "po/POTFILES.in is missing, and their words go untranslated:" >&2
    echo "$forgotten" | sed 's/^/  /' >&2
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
    # en_GB was written from this template a moment ago.
    if [ "$lang" != en_GB ]; then
        msgmerge --quiet --update --backup=none --previous "$po" "$pot"
    fi
    install -Dm644 /dev/null "target/locale/$lang/LC_MESSAGES/penguin-mail.mo"
    msgfmt --check --statistics -o "target/locale/$lang/LC_MESSAGES/penguin-mail.mo" "$po"
done

echo "Wrote $pot and compiled po/*.po into target/locale."
