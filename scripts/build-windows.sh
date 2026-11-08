#!/usr/bin/env bash
# Builds the Windows release and its installer, end to end:
#
#   scripts/build-windows.sh --prefix D:/gtk-msvc/prefix
#
# --prefix names the GTK prefix the build links against: the gvsbuild
# GTK4 zip unpacked anywhere, or a prefix gvsbuild built from source.
# PENGUIN_MAIL_GTK_PREFIX works instead of the flag. The zip's
# lib/pkgconfig/*.pc files bake in C:/gtk-build/...; the script rewrites
# the prefix= lines to the real location itself.
#
# The compiler is whatever Visual Studio 2022 is installed — Build
# Tools, Community or another edition — found through vswhere, so a
# machine does not have to name its edition. --busybox and --stage-only
# pass through to scripts/package-windows.sh, which this calls once the
# exe is built; with no --busybox a copy of BusyBox is fetched once into
# the cache folder.
#
# Run it from an MSYS2 or Git Bash shell.
set -euo pipefail
cd "$(dirname "$0")/.."

prefix=${PENGUIN_MAIL_GTK_PREFIX:-}
busybox=
package_args=()
for arg in "$@"; do case $arg in
    --prefix=*) prefix=${arg#*=} ;;
    --busybox=*) busybox=${arg#*=} ;;
    --stage-only) package_args+=(--stage-only) ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
esac; done

[ -d "$prefix/bin" ] || {
    echo "--prefix needs a GTK prefix holding bin/ (the gvsbuild zip unpacked)" >&2
    exit 2
}
prefix=$(cd "$prefix" && pwd)

# The prefix= lines of every .pc must name where the prefix really sits.
# A first file still pointing at its build location speaks for them all.
pc=$(ls "$prefix/lib/pkgconfig/"*.pc 2>/dev/null | head -1) || true
if [ -n "$pc" ] && ! grep -q "^prefix=$(cygpath -m "$prefix")" "$pc"; then
    sed -i "s|^prefix=.*|prefix=$(cygpath -m "$prefix")|" "$prefix/lib/pkgconfig/"*.pc
fi

# MSVC's own variables arrive through vcvarsall, dumped to `set` by a
# throwaway batch file and imported one by one. vswhere locates the
# installation without knowing the edition.
if [ -z "${VCToolsInstallDir:-}" ]; then
    vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
    [ -x "$vswhere" ] || { echo "vswhere not found; is Visual Studio 2022 installed?" >&2; exit 1; }
    vs=$("$vswhere" -latest -products "*" \
        -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
        -property installationPath | tr -d '\r')
    vcvars=$(cygpath -u "$vs")/VC/Auxiliary/Build/vcvarsall.bat
    [ -f "$vcvars" ] || { echo "vcvarsall.bat not found under $vs" >&2; exit 1; }
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    cat > "$work/env.bat" <<EOF
@echo off
call "$(cygpath -w "$vcvars")" x64 >nul 2>nul
set
EOF
    while IFS='=' read -r k v; do
        v="${v%$'\r'}"
        case "$k" in
            PATH | Path)
                # vcvars rewrites Path entirely; the unix form is what
                # this shell uses, and children get Windows paths back.
                export PATH="$(cygpath -pu "$v")"
                ;;
            "" | *[^A-Za-z0-9_]*) ;;
            *) export "$k=$v" ;;
        esac
    done < <(cmd //c "$(cygpath -w "$work/env.bat")")
fi

export PKG_CONFIG="$(cygpath -w "$prefix")\\bin\\pkg-config.exe"
export PKG_CONFIG_PATH="$(cygpath -w "$prefix")\\lib\\pkgconfig;$(cygpath -w "$prefix")\\share\\pkgconfig"
export GETTEXT_DIR="$(cygpath -w "$prefix")"
export GETTEXT_LIB_DIR="$(cygpath -w "$prefix")\\lib"
export GETTEXT_INCLUDE_DIR="$(cygpath -w "$prefix")\\include"
export RUSTFLAGS="-L native=$(cygpath -w "$prefix")\\lib"
export PATH="$prefix/bin:$PATH"

cargo build --release --target x86_64-pc-windows-msvc -p mailrs

exe=target/x86_64-pc-windows-msvc/release/penguin-mail.exe
# webview2-com-sys keeps the delay-loaded loader in its OUT_DIR; beside
# the exe is where the packager looks for it first.
loader=$(ls "$PWD/target/x86_64-pc-windows-msvc/release/build/"webview2-com-sys-*/out/x64/WebView2Loader.dll 2>/dev/null | head -1) || true
if [ -n "$loader" ]; then
    cp "$loader" "$(dirname "$exe")/"
fi

# BusyBox gives the skill sandbox its shell. Kept in the cache so a
# rebuild does not fetch it again; Windows' curl verifies against the
# system certificate store, which an MSYS2 curl may not have populated.
if [ -z "$busybox" ]; then
    busybox=${XDG_CACHE_HOME:-$HOME/.cache}/penguin-mail/busybox64u.exe
    if [ ! -f "$busybox" ]; then
        mkdir -p "$(dirname "$busybox")"
        curl=/c/Windows/System32/curl.exe
        [ -x "$curl" ] || curl=curl
        "$curl" -fsSL -o "$busybox" "https://frippery.org/files/busybox/busybox64u.exe"
    fi
fi
"$busybox" true >/dev/null 2>&1 || { echo "$busybox does not run" >&2; exit 1; }

scripts/package-windows.sh --exe="$exe" --prefix="$prefix" --busybox="$busybox" "${package_args[@]}"
