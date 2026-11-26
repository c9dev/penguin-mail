#!/usr/bin/env bash
# Builds the Windows release and its installer, end to end, with MSYS2 UCRT64:
#
#   scripts/build-windows.sh [--msys2 C:/msys64/ucrt64] [--busybox PATH] [--stage-only]
#
# --msys2 names the UCRT64 prefix that supplies GTK, libadwaita and the helpers (default
# C:/msys64/ucrt64). The release CI installs a gtk4 package carrying the monitor patch first
# (scripts/build-gtk4-msys2.sh); a stock gtk4 builds too, without the patch. --busybox and
# --stage-only pass through to scripts/package-windows.sh, which this calls once the exe is built;
# with no --busybox a copy of BusyBox is fetched once into the cache folder.
#
# Run it from an MSYS2 UCRT64 shell with the GNU Rust toolchain installed.
set -euo pipefail
cd "$(dirname "$0")/.."

msys2=C:/msys64/ucrt64
busybox=
package_args=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --msys2)
            [ "$#" -ge 2 ] && [ -n "$2" ] && [[ "$2" != --* ]] || { echo "--msys2 needs a value" >&2; exit 2; }
            msys2=$2; shift 2 ;;
        --msys2=*) msys2=${1#*=}; shift ;;
        --busybox)
            [ "$#" -ge 2 ] && [ -n "$2" ] && [[ "$2" != --* ]] || { echo "--busybox needs a value" >&2; exit 2; }
            busybox=$2; shift 2 ;;
        --busybox=*) busybox=${1#*=}; shift ;;
        --stage-only) package_args+=(--stage-only); shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[ -d "$msys2/bin" ] && [ -d "$msys2/lib/pkgconfig" ] || {
    echo "--msys2 needs an MSYS2 UCRT64 prefix holding bin/ and lib/pkgconfig/" >&2
    exit 2
}
prefix=$(cd "$msys2" && pwd)

export PKG_CONFIG_PATH="$(cygpath -m "$prefix")/lib/pkgconfig"
export X86_64_PC_WINDOWS_GNU_MSYSTEM_PREFIX="$(cygpath -m "$prefix")"
export GETTEXT_SYSTEM=1
export PATH="$prefix/bin:$PATH"

cargo build --release --target x86_64-pc-windows-gnu -p mailrs
exe=target/x86_64-pc-windows-gnu/release/penguin-mail.exe

# BusyBox gives the skill sandbox its shell. Kept in the cache so a
# rebuild does not fetch it again; Windows' curl verifies against the
# system certificate store, which an MSYS2 curl may not have populated.
# The same FRP-6354 release the Windows CI builds from source.
busybox_sha256=36b266923fc5765b42f5f981319f39568498fba60b53ccf26e8fc1e575e2a9a9
if [ -z "$busybox" ]; then
    busybox=${XDG_CACHE_HOME:-$HOME/.cache}/penguin-mail/busybox64u.exe
    if [ ! -f "$busybox" ]; then
        mkdir -p "$(dirname "$busybox")"
        curl=/c/Windows/System32/curl.exe
        [ -x "$curl" ] || curl=curl
        "$curl" -fsSL -o "$busybox" "https://frippery.org/files/busybox/busybox-w64u-FRP-6354-g5f09fb446.exe"
    fi
    if ! echo "$busybox_sha256  $busybox" | sha256sum -c - >/dev/null 2>&1; then
        echo "$busybox does not match the pinned SHA-256; delete it or pass --busybox" >&2
        exit 1
    fi
fi
"$busybox" true >/dev/null 2>&1 || { echo "$busybox does not run" >&2; exit 1; }

scripts/package-windows.sh --exe="$exe" --prefix="$prefix" --busybox="$busybox" --msys2="$(cygpath -m "$prefix")" "${package_args[@]}"
