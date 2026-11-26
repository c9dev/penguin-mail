#!/bin/bash
# Stages the Windows install tree and builds the Inno Setup installer.
#
# The tree mirrors a GTK prefix (bin/ beside lib/, share/ and etc/) so the
# libraries find their data by their own location, the same layout
# scripts/bundle-macos.py builds for macOS. The prefix to collect from is
# the one the build linked against: MSYS2's for a gnu build, gvsbuild's
# for an MSVC one.
#
#   scripts/package-windows.sh --exe target/.../penguin-mail.exe \
#       --prefix D:/gtk-msvc/prefix --busybox path/to/busybox64u.exe
#
# --msys2 names the MSYS2 UCRT64 prefix (default C:/msys64/ucrt64) that
# supplies gpgv.exe and pdftotext.exe. Each goes into its own folder under
# libexec/ with its own DLLs, so its GLib and cairo never meet the GTK
# DLLs in bin/.
#
# --stage-only writes the tree under dist/penguin-mail/ and stops.
set -euo pipefail
cd "$(dirname "$0")/.."

exe=
prefix=${PENGUIN_MAIL_GTK_PREFIX:-}
busybox=
msys2=C:/msys64/ucrt64
out=dist
installer=yes
while [ "$#" -gt 0 ]; do
    case "$1" in
        --exe)
            [ "$#" -ge 2 ] && [ -n "$2" ] && [[ "$2" != --* ]] || { echo "--exe needs a value" >&2; exit 2; }
            exe=$2; shift 2 ;;
        --exe=*) exe=${1#*=}; shift ;;
        --prefix)
            [ "$#" -ge 2 ] && [ -n "$2" ] && [[ "$2" != --* ]] || { echo "--prefix needs a value" >&2; exit 2; }
            prefix=$2; shift 2 ;;
        --prefix=*) prefix=${1#*=}; shift ;;
        --busybox)
            [ "$#" -ge 2 ] && [ -n "$2" ] && [[ "$2" != --* ]] || { echo "--busybox needs a value" >&2; exit 2; }
            busybox=$2; shift 2 ;;
        --busybox=*) busybox=${1#*=}; shift ;;
        --msys2)
            [ "$#" -ge 2 ] && [ -n "$2" ] && [[ "$2" != --* ]] || { echo "--msys2 needs a value" >&2; exit 2; }
            msys2=$2; shift 2 ;;
        --msys2=*) msys2=${1#*=}; shift ;;
        --out)
            [ "$#" -ge 2 ] && [ -n "$2" ] && [[ "$2" != --* ]] || { echo "--out needs a value" >&2; exit 2; }
            out=$2; shift 2 ;;
        --out=*) out=${1#*=}; shift ;;
        --stage-only) installer=; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[ -f "$exe" ] || { echo "--exe needs the built penguin-mail.exe" >&2; exit 2; }
[ -d "$prefix/bin" ] || { echo "--prefix needs a GTK prefix holding bin/" >&2; exit 2; }
msys2=$(cygpath -u "$msys2")
[ -d "$msys2/bin" ] || { echo "--msys2 needs the MSYS2 UCRT64 prefix holding bin/" >&2; exit 2; }

mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
stage=$work/penguin-mail
mkdir -p "$stage/bin" "$stage/lib" "$stage/share" "$stage/etc"
cp "$exe" "$stage/bin/penguin-mail.exe"

# Every DLL the tree links, found breadth-first through the import tables.
# Anything not in the prefix's bin or beside the exe is a system library.
exe_dir=$(cd "$(dirname "$exe")" && pwd)
declare -A copied=()
collect() {
    local file=$1 dll found dep
    while read -r dll; do
        found=
        for dir in "$exe_dir" "$prefix/bin"; do
            [ -f "$dir/$dll" ] && { found=$(cd "$dir" && pwd)/$dll; break; }
        done
        [ -n "$found" ] || continue
        dep=${found,,}
        [ -n "${copied[$dep]:-}" ] && continue
        copied[$dep]=1
        cp "$found" "$stage/bin/$dll"
        collect "$stage/bin/$dll"
    done < <(objdump -p "$file" | awk '/DLL Name/ {print $3}' | sort -u)
}
collect "$stage/bin/penguin-mail.exe"

# stage_tool EXE DEST-DIR DLL-DIR copies one program and every DLL it imports
# from DLL-DIR into DEST-DIR and nowhere else. Anything DLL-DIR lacks is a
# system library.
stage_tool() {
    local tool=$1 dest=$2 dlls=$3
    [ -f "$tool" ] || { echo "no $tool" >&2; exit 1; }
    mkdir -p "$dest"
    cp "$tool" "$dest/"
    stage_imports "$dest/$(basename "$tool")" "$dest" "$dlls"
}
stage_imports() {
    local file=$1 dest=$2 dlls=$3 dll
    while read -r dll; do
        [ -f "$dlls/$dll" ] || continue
        [ -f "$dest/$dll" ] && continue
        cp "$dlls/$dll" "$dest/"
        stage_imports "$dest/$dll" "$dest" "$dlls"
    done < <(objdump -p "$file" | awk '/DLL Name/ {print $3}' | sort -u)
}

# GLib starts the session bus through gdbus.exe, which single-instance
# needs; nothing imports it, so the walk above never finds it.
[ -f "$prefix/bin/gdbus.exe" ] || { echo "no gdbus.exe in $prefix/bin" >&2; exit 1; }
cp "$prefix/bin/gdbus.exe" "$stage/bin/"
collect "$stage/bin/gdbus.exe"

# The updater checks a release's signature with gpgv, and the assistant reads
# PDFs with pdftotext. Neither is on a Windows machine, so both ship here.
# shortcut: pdftotext gets no poppler-data, so text in a PDF that uses an
# unembedded Chinese, Japanese or Korean font may not extract; add
# poppler-data when someone reports it.
stage_tool "$msys2/bin/gpgv.exe" "$stage/libexec/gnupg" "$msys2/bin"
stage_tool "$msys2/bin/pdftotext.exe" "$stage/libexec/poppler" "$msys2/bin"
mkdir -p "$stage/share/licenses"
# Every MSYS2 package that owns a file under libexec/ brings its licence
# folder. A package that ships none, such as poppler, is named in
# helpers.txt, and its GPL text is the LICENSE beside it.
command -v pacman >/dev/null || { echo "staging the helpers needs MSYS2's pacman for their licences" >&2; exit 1; }
declare -A licensed=()
unlicensed=()
for file in "$stage"/libexec/*/*; do
    owner=$(pacman -Qqo "$(cygpath -u "$msys2")/bin/$(basename "$file")" 2>/dev/null) || { echo "no MSYS2 package owns $(basename "$file")" >&2; exit 1; }
    [ -n "${licensed[$owner]:-}" ] && continue
    licensed[$owner]=1
    # A package's licence folder need not carry its name: openjpeg2's is openjpeg.
    folders=$(pacman -Ql "$owner" | sed -n 's|^[^ ]* /ucrt64/share/licenses/\([^/]\{1,\}\)/$|\1|p' | sort -u)
    if [ -n "$folders" ]; then
        for folder in $folders; do
            cp -r "$(cygpath -u "$msys2")/share/licenses/$folder" "$stage/share/licenses/"
        done
    else
        unlicensed+=("$owner $(pacman -Qi "$owner" | sed -n 's/^Licenses *: //p')")
    fi
done
cat > "$stage/share/licenses/helpers.txt" <<'NOTE'
libexec\gnupg and libexec\poppler hold gpgv.exe and pdftotext.exe with the
libraries they load, unchanged from the MSYS2 packages
mingw-w64-ucrt-x86_64-gnupg and mingw-w64-ucrt-x86_64-poppler. Their source
is at https://github.com/msys2/MINGW-packages and the upstream projects:
https://gnupg.org and https://poppler.freedesktop.org.

These packages ship no licence files of their own; their licences are
named here, and the GPL text is in LICENSE at the top of this folder:
NOTE
printf '%s\n' "${unlicensed[@]}" >> "$stage/share/licenses/helpers.txt"

# Pieces the import walk cannot see: WebView2Loader is delay-loaded on
# MSVC, and the VC++ runtime DLLs ride along so a machine without the
# redistributable still starts. Each may sit beside the exe, in the
# prefix, or in System32; whichever build does not need one simply
# finds it nowhere.
for extra in WebView2Loader.dll vcruntime140.dll vcruntime140_1.dll; do
    for dir in "$exe_dir" "$prefix/bin" /c/Windows/System32; do
        [ -f "$dir/$extra" ] && { cp "$dir/$extra" "$stage/bin/"; break; }
    done
done

# gdk-pixbuf loads its loaders at runtime; the import walk never sees them.
loaders_src=$prefix/lib/gdk-pixbuf-2.0/2.10.0/loaders
loaders_dst=$stage/lib/gdk-pixbuf-2.0/2.10.0/loaders
mkdir -p "$loaders_dst"
for module in "$loaders_src"/*.dll; do
    cp "$module" "$loaders_dst/"
    collect "$loaders_dst/$(basename "$module")"
done
# The cache names each loader by bare filename; gdk-pixbuf resolves those
# against the loaders' own folder, which keeps the tree relocatable.
"$prefix/bin/gdk-pixbuf-query-loaders.exe" "$loaders_dst"/*.dll \
    | sed 's|"[^"]*[\\/]\([^\\/]*\.dll\)|"\1|g' \
    | grep -v '^#' > "$loaders_dst/../loaders.cache"
grep -q svg "$loaders_dst/../loaders.cache" || echo "warning: no SVG pixbuf loader staged" >&2

schemas=$stage/share/glib-2.0/schemas
mkdir -p "$schemas"
cp "$prefix/share/glib-2.0/schemas"/*.xml "$schemas/"
"$prefix/bin/glib-compile-schemas.exe" "$schemas"

mkdir -p "$stage/share/icons"
for theme in Adwaita hicolor; do
    [ -d "$prefix/share/icons/$theme" ] && cp -r "$prefix/share/icons/$theme" "$stage/share/icons/"
done
cp -r app/data/icons/. "$stage/share/icons/hicolor/"

for po in po/*.po; do
    lang=$(basename "$po" .po)
    mkdir -p "$stage/share/locale/$lang/LC_MESSAGES"
    "$prefix/bin/msgfmt.exe" "$po" -o "$stage/share/locale/$lang/LC_MESSAGES/penguin-mail.mo"
done
# The toolkit's own catalogues; the app's languages bound which to copy.
for catalog in "$prefix/share/locale"/*/LC_MESSAGES/{glib20,gtk40,gtk40-properties,libadwaita,gdk-pixbuf,gspell-1,iso_639}.mo; do
    [ -f "$catalog" ] || continue
    rel=${catalog#"$prefix/share/locale/"}
    mkdir -p "$stage/share/locale/$(dirname "$rel")"
    cp "$catalog" "$stage/share/locale/$rel"
done

cp -rL "$prefix/etc/fonts" "$stage/etc/"

scripts/stage-windows-data.sh "$stage" "$prefix/bin/msgfmt.exe"

cp LICENSE "$stage/"
[ -d "$prefix/share/licenses" ] && cp -r "$prefix/share/licenses" "$stage/share/"

[ -n "$busybox" ] && cp "$busybox" "$stage/bin/"

if [ -n "$installer" ]; then
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    # An ls over candidates would exit nonzero when one is missing, and
    # pipefail turns that into a dead script before the value is used.
    iscc=$(command -v ISCC.exe || command -v iscc || true)
    if [ -z "$iscc" ]; then
        for candidate in "$LOCALAPPDATA/Programs/Inno Setup 6/ISCC.exe" "/c/Program Files (x86)/Inno Setup 6/ISCC.exe"; do
            [ -f "$candidate" ] && { iscc=$candidate; break; }
        done
    fi
    [ -n "$iscc" ] || { echo "Inno Setup 6 not found; stage is at $stage" >&2; exit 1; }
    webview2=$work/MicrosoftEdgeWebview2Setup.exe
    # Windows' own curl consults the system certificate store; an MSYS2
    # curl with an unpopulated CA bundle cannot verify the download.
    curl=/c/Windows/System32/curl.exe
    [ -x "$curl" ] || curl=curl
    "$curl" -fsSL -o "$webview2" "https://go.microsoft.com/fwlink/p/?LinkId=2124703" || webview2=
    keep=$out/penguin-mail
    rm -rf "$keep"
    cp -r "$stage" "$keep"
    stage=$keep
    # ARG_CONV_EXCL keeps the bash-flavoured path mangler from rewriting
    # /D into a Windows path before ISCC sees it.
    args=(/D"AppVersion=$version" /D"SourceDir=$(cygpath -w "$stage")" /D"OutputDir=$(cygpath -w "$out")")
    [ -n "$webview2" ] && args+=(/D"WebView2Bootstrapper=$(cygpath -w "$webview2")")
    MSYS2_ARG_CONV_EXCL="*" "$iscc" "${args[@]}" "$(cygpath -w "$PWD/packaging/windows/penguin-mail.iss")"
else
    rm -rf "$out/penguin-mail"
    cp -r "$stage" "$out/"
fi
echo "Staged install tree at $stage"
