#!/usr/bin/env bash
# Builds GTK 4 and libadwaita from source with gvsbuild, with
# packaging/windows/gtk4-zero-monitor-popup.patch applied to GTK. The prefix lands where gvsbuild
# puts it, C:\gtk-build\gtk\x64\release, ready for scripts/build-windows.sh --prefix.
#
#   scripts/build-gtk-windows.sh PATH/TO/GVSBUILD/CHECKOUT
#
# gvsbuild takes patches only from its own patches folder and its own project list, so the script
# copies ours in and adds one line to the GTK 4 project. Run it from Git Bash or MSYS2 with uv on
# PATH. The checkout is the CI's pinned gvsbuild release.
set -euo pipefail
cd "$(dirname "$0")/.."

gvsbuild=${1:?usage: build-gtk-windows.sh GVSBUILD_CHECKOUT}
gtk_py=$gvsbuild/gvsbuild/projects/gtk.py
# The SHA-256 of the GTK 4.24.1 tarball, which the patch was written and tested against. A gvsbuild
# release that moves GTK 4 to another version stops here until the patch is checked against it.
gtk4_hash=e98abe720e16129c8c0f50761dee0a2e9ae2478055e31018f6cf977fcf6513e9

[ "$(grep -c "hash=\"$gtk4_hash\"," "$gtk_py")" = 1 ] || {
    echo "gvsbuild no longer builds GTK 4.24.1; check the patch against its GTK and update gtk4_hash" >&2
    exit 1
}
mkdir -p "$gvsbuild/gvsbuild/patches/gtk4"
cp packaging/windows/gtk4-zero-monitor-popup.patch "$gvsbuild/gvsbuild/patches/gtk4/"
sed -i "/hash=\"$gtk4_hash\",/a\\            patches=[\"gtk4-zero-monitor-popup.patch\"]," "$gtk_py"
grep -q 'patches=\["gtk4-zero-monitor-popup.patch"\]' "$gtk_py" || { echo "the patch line did not land in $gtk_py" >&2; exit 1; }

cd "$gvsbuild"
# -j2 keeps the build inside a hosted runner's memory, as gvsbuild's own CI does.
exec uv run --no-dev gvsbuild build --ninja-opts -j2 gtk4 libadwaita adwaita-icon-theme
