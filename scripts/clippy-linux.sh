#!/bin/sh
# Lints the workspace for aarch64 Linux from macOS, where WebKitGTK and the
# rest of the Linux libraries are missing. cargo clippy never links, so a
# stand-in pkg-config that reports every library present is enough, and
# zig compiles the C the build scripts carry (SQLite, aws-lc, gettext's
# probe). It runs no tests: those need Linux itself.
#
#   brew install zig && rustup target add aarch64-unknown-linux-gnu
#   scripts/clippy-linux.sh [-p mailrs]
set -eu
cd "$(dirname "$0")/.."
tools=$(mktemp -d)
trap 'rm -rf "$tools"' EXIT

cat > "$tools/pkg-config" <<'STUB'
#!/bin/sh
for a in "$@"; do
  case "$a" in
    --modversion) echo 99.0; exit 0 ;;
    --version) echo 0.29.2; exit 0 ;;
  esac
done
exit 0
STUB
# cc-rs adds a clang-style --target, which zig spells its own way.
cat > "$tools/cc" <<'STUB'
#!/bin/sh
for a in "$@"; do
  shift
  case "$a" in --target=*) ;; *) set -- "$@" "$a" ;; esac
done
exec zig cc -target aarch64-linux-gnu.2.39 "$@"
STUB
printf '#!/bin/sh\nexec zig ar "$@"\n' > "$tools/ar"
chmod +x "$tools"/*

PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG="$tools/pkg-config" \
CC_aarch64_unknown_linux_gnu="$tools/cc" AR_aarch64_unknown_linux_gnu="$tools/ar" \
CARGO_TARGET_DIR=target/linux-check \
    cargo clippy --workspace --all-targets --target aarch64-unknown-linux-gnu \
    --ignore-rust-version "$@" -- -D warnings
