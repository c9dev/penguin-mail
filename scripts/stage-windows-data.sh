#!/bin/bash
# Adds the data the Windows app reads from beside its exe to an install tree:
# Hunspell dictionaries (share/hunspell), their licences, and the iso-codes
# language and country names (share/iso-codes/json plus the .mo catalogues).
# Every download is checked against a SHA-256 pinned here or in
# packaging/windows/dictionaries.sha256 before it is used.
#
#   scripts/stage-windows-data.sh STAGE_DIR MSGFMT [CACHE_DIR]
#
# STAGE_DIR is the tree that holds bin/ and share/. MSGFMT is the msgfmt to
# compile the catalogues with. CACHE_DIR keeps the downloads between runs.
set -euo pipefail
cd "$(dirname "$0")/.."

stage=${1:?usage: stage-windows-data.sh STAGE_DIR MSGFMT [CACHE_DIR]}
msgfmt=${2:?usage: stage-windows-data.sh STAGE_DIR MSGFMT [CACHE_DIR]}
cache=${3:-${XDG_CACHE_HOME:-$HOME/.cache}/penguin-mail/windows-data}

# A tag of github.com/LibreOffice/dictionaries is a moving name, so the
# release tag libreoffice-26.8.3.1 is pinned by its commit.
dictionaries_commit=c357b1c5c418f1e4cfd05756778b245462ed43b2
iso_version=4.20.1
iso_sha256=5d551f3ddb32548c4321e9011720fd97751af0107592f79ebffc939bd32f2268

# Windows' own curl consults the system certificate store; an MSYS2 curl
# with an unpopulated CA bundle cannot verify the download.
curl=/c/Windows/System32/curl.exe
[ -x "$curl" ] || curl=curl

# fetch URL SHA256 FILE: downloads FILE unless the cached copy already matches.
fetch() {
    local url=$1 sha=$2 file=$3
    if ! echo "$sha  $file" | sha256sum -c - >/dev/null 2>&1; then
        mkdir -p "$(dirname "$file")"
        "$curl" -fsSL --proto '=https' --tlsv1.2 -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
    echo "$sha  $file" | sha256sum -c - >/dev/null 2>&1 || {
        echo "$file does not match its pinned SHA-256; deleted" >&2
        rm -f "$file"
        exit 1
    }
}

# Hunspell: LANGUAGE:FOLDER:FILE-STEM, where FOLDER and FILE-STEM name the
# .aff and .dic in the dictionaries repo. They are copied as LANGUAGE.aff and
# LANGUAGE.dic, the names composer/spell.rs looks for. japanese and chinese
# have no dictionary there.
pairs=(
    de_DE:de:de_DE_frami el_GR:el_GR:el_GR en_US:en:en_US en_GB:en:en_GB
    es_ES:es:es_ES fr_FR:fr_FR/dictionaries:fr id_ID:id:id_ID it_IT:it_IT:it_IT
    ko_KR:ko_KR:ko_KR pl_PL:pl_PL:pl_PL pt_PT:pt_PT:pt_PT ro_RO:ro:ro_RO
    ru_RU:ru_RU:ru_RU tr_TR:tr_TR:tr_TR uk_UA:uk_UA:uk_UA vi_VN:vi:vi_VN
)
declare -A sha=()
while read -r hash path; do
    sha[$path]=$hash
    fetch "https://raw.githubusercontent.com/LibreOffice/dictionaries/$dictionaries_commit/$path" \
        "$hash" "$cache/hunspell/$dictionaries_commit/$path"
done < packaging/windows/dictionaries.sha256

hunspell=$stage/share/hunspell
mkdir -p "$hunspell"
for pair in "${pairs[@]}"; do
    IFS=: read -r lang folder base <<<"$pair"
    for ext in aff dic; do
        [ -n "${sha[$folder/$base.$ext]:-}" ] || { echo "no pinned SHA-256 for $folder/$base.$ext" >&2; exit 1; }
        cp "$cache/hunspell/$dictionaries_commit/$folder/$base.$ext" "$hunspell/$lang.$ext"
    done
    # The licence and readme files of the same folder travel with the pair.
    licences=$stage/share/licenses/hunspell/$lang
    mkdir -p "$licences"
    for path in "${!sha[@]}"; do
        [ "$(dirname "$path")" = "$folder" ] || continue
        case "$path" in
            *.aff | *.dic) ;;
            *) cp "$cache/hunspell/$dictionaries_commit/$path" "$licences/" ;;
        esac
    done
done

# iso-codes: the two lists language_names.rs reads, and the catalogues that
# translate their names into the app's languages.
tarball=$cache/iso-codes_$iso_version.orig.tar.xz
fetch "https://deb.debian.org/debian/pool/main/i/iso-codes/iso-codes_$iso_version.orig.tar.xz" \
    "$iso_sha256" "$tarball"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
top=iso-codes-$iso_version
tar -xJf "$tarball" -C "$work" --strip-components=1 \
    "$top/data/iso_639-2.json" "$top/data/iso_3166-1.json" \
    "$top/iso_639-2" "$top/iso_3166-1" "$top/LICENSES/LGPL-2.1-or-later.txt"
mkdir -p "$stage/share/iso-codes/json" "$stage/share/licenses/iso-codes"
cp "$work/data/iso_639-2.json" "$work/data/iso_3166-1.json" "$stage/share/iso-codes/json/"
cp "$work/LICENSES/LGPL-2.1-or-later.txt" "$stage/share/licenses/iso-codes/"
while read -r lang; do
    case "$lang" in "" | "#"*) continue ;; esac
    for domain in iso_639-2 iso_3166-1; do
        # pt_PT falls back to iso-codes' pt; a language with neither stays in English.
        for source in "$lang" "${lang%%_*}"; do
            if [ -f "$work/$domain/$source.po" ]; then
                mkdir -p "$stage/share/locale/$lang/LC_MESSAGES"
                "$msgfmt" "$work/$domain/$source.po" -o "$stage/share/locale/$lang/LC_MESSAGES/$domain.mo"
                break
            fi
        done
    done
done < po/LINGUAS
