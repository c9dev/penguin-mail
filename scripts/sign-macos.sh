#!/bin/bash
# Missing Apple settings produce an ad hoc signed archive, never a failed release.
# Once configured, signing or notarization failures stop publication.
set -euo pipefail
app=${1:?usage: sign-macos.sh app output-directory}
out=${2:?usage: sign-macos.sh app output-directory}
work=$(mktemp -d)
keychain=
cleanup() {
    [ -z "$keychain" ] || security delete-keychain "$keychain" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
identity=-
configured=1
for setting in MACOS_CERTIFICATE_P12 MACOS_CERTIFICATE_PASSWORD APPLE_ID APPLE_TEAM_ID APPLE_APP_SPECIFIC_PASSWORD; do
    if [ -z "${!setting:-}" ]; then configured=0; fi
done
options=(--timestamp=none)
if [ "$configured" = 1 ]; then
    keychain="$work/signing.keychain-db"
    password=$(openssl rand -hex 24)
    printf '%s' "$MACOS_CERTIFICATE_P12" | base64 --decode > "$work/certificate.p12"
    security create-keychain -p "$password" "$keychain"
    security set-keychain-settings -lut 21600 "$keychain"
    security unlock-keychain -p "$password" "$keychain"
    security import "$work/certificate.p12" -P "$MACOS_CERTIFICATE_PASSWORD" \
        -T /usr/bin/codesign -k "$keychain" >/dev/null
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
    identity=$(security find-identity -v -p codesigning "$keychain" | \
        awk '/Developer ID Application:/ {print $2; exit}')
    if [ -z "$identity" ]; then
        echo 'No valid Developer ID Application identity in MACOS_CERTIFICATE_P12.' >&2
        exit 1
    fi
    options=(--keychain "$keychain" --timestamp --options runtime)
else
    echo '::warning::Apple signing is not fully configured; creating an unsigned test archive.'
fi
while IFS= read -r -d '' binary; do
    codesign --force --sign "$identity" "${options[@]}" "$binary"
done < <(find "$app/Contents/Frameworks" -type f -print0)
codesign --force --sign "$identity" "${options[@]}" "$app"
codesign --verify --deep --strict "$app"
version=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$app/Contents/Info.plist")
arch=$(uname -m)
suffix=-unsigned
if [ "$configured" = 1 ]; then
    ditto -c -k --keepParent "$app" "$work/notarize.zip"
    xcrun notarytool submit "$work/notarize.zip" --apple-id "$APPLE_ID" \
        --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD" \
        --wait --timeout 30m --output-format json > "$work/notary.json"
    python3 - "$work/notary.json" <<'PY'
import json, sys
answer = json.load(open(sys.argv[1]))
if answer.get("status") != "Accepted":
    sys.exit(f"Notarization {answer.get('id')} ended with {answer.get('status')}")
PY
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
    spctl --assess --type execute --verbose "$app"
    suffix=
fi
archive="$out/penguin-mail-$version-macos-$arch$suffix.zip"
ditto -c -k --keepParent "$app" "$archive"
echo "Created $archive"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    printf 'macOS %s: `%s` (Developer ID and notarization: %s).\n' \
        "$arch" "$(basename "$archive")" "$configured" >> "$GITHUB_STEP_SUMMARY"
fi
