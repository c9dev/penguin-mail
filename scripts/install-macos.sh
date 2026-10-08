#!/bin/sh
# Builds Penguin Mail and installs it as an app on this Mac. The app links
# Homebrew's GTK, libadwaita and gettext where they are installed, so it is
# for the Mac that built it, not a bundle to hand to anyone else.
#
#   scripts/install-macos.sh [cargo flags, such as --ignore-rust-version]
#
# It goes to /Applications, or to ~/Applications when that is not
# writable. APP_DIR names another folder.
set -eu

cd "$(dirname "$0")/.."
brew=$(brew --prefix)
for formula in gtk4 libadwaita adwaita-icon-theme gettext librsvg; do
    if ! brew list --versions "$formula" >/dev/null; then
        echo "install-macos.sh needs Homebrew's $formula: brew install $formula" >&2
        exit 1
    fi
done

if [ -f packaging/secrets.env ]; then
    set -a
    # shellcheck source=/dev/null
    . packaging/secrets.env
    set +a
fi

cargo build --release -p mailrs "$@"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
folder=${APP_DIR:-/Applications}
if [ ! -w "$folder" ]; then
    folder=$HOME/Applications
    mkdir -p "$folder"
fi
app="$folder/Penguin Mail.app"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
bundle="$work/Penguin Mail.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"

cp target/release/penguin-mail "$bundle/Contents/Resources/penguin-mail"

# The Finder starts an app with a bare environment. GTK finds Adwaita's
# icons and GLib's settings schemas through XDG_DATA_DIRS, which then
# leaves out Homebrew's share folder.
cat > "$bundle/Contents/MacOS/penguin-mail-launcher" <<LAUNCHER
#!/bin/sh
export XDG_DATA_DIRS="\${XDG_DATA_DIRS:-$brew/share:/usr/local/share:/usr/share}"
exec "\$(dirname "\$0")/../Resources/penguin-mail" "\$@"
LAUNCHER
chmod +x "$bundle/Contents/MacOS/penguin-mail-launcher"

cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Penguin Mail</string>
  <key>CFBundleDisplayName</key><string>Penguin Mail</string>
  <key>CFBundleIdentifier</key><string>io.github.c9dev.PenguinMail</string>
  <key>CFBundleExecutable</key><string>penguin-mail-launcher</string>
  <key>CFBundleIconFile</key><string>PenguinMail</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>NSUserNotificationAlertStyle</key><string>alert</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# The icon, from the same drawings the Linux packages use: the 16 px copy
# drawn on whole pixels for the smallest size, the scalable one for the rest.
icons=app/data/icons
set_dir="$work/PenguinMail.iconset"
mkdir -p "$set_dir"
for size in 16 32 64 128 256 512 1024; do
    source=$icons/scalable/apps/io.github.c9dev.PenguinMail.svg
    [ "$size" = 16 ] && source=$icons/16x16/apps/io.github.c9dev.PenguinMail.svg
    rsvg-convert -w "$size" -h "$size" "$source" -o "$set_dir/$size.png"
done
cp "$set_dir/16.png" "$set_dir/icon_16x16.png"
cp "$set_dir/32.png" "$set_dir/icon_16x16@2x.png"
cp "$set_dir/32.png" "$set_dir/icon_32x32.png"
cp "$set_dir/64.png" "$set_dir/icon_32x32@2x.png"
cp "$set_dir/128.png" "$set_dir/icon_128x128.png"
cp "$set_dir/256.png" "$set_dir/icon_128x128@2x.png"
cp "$set_dir/256.png" "$set_dir/icon_256x256.png"
cp "$set_dir/512.png" "$set_dir/icon_256x256@2x.png"
cp "$set_dir/512.png" "$set_dir/icon_512x512.png"
cp "$set_dir/1024.png" "$set_dir/icon_512x512@2x.png"
rm "$set_dir"/[0-9]*.png
iconutil -c icns "$set_dir" -o "$bundle/Contents/Resources/PenguinMail.icns"

# An ad hoc signature gives the app one identity for the Keychain and
# for notification permission, rather than none.
codesign --force --sign - "$bundle"
# A copy already running keeps its files open; replacing the bundle under
# it is fine, and the next start takes the new one.
rm -rf "$app"
mv "$bundle" "$app"
# Tells the Dock and the Finder the icon changed.
touch "$app"
echo "Installed $app"
