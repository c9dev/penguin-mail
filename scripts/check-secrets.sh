#!/bin/sh
# Checks that every package build hands the compiler the Microsoft client
# id from the MICROSOFT_CLIENT_ID secret, and that no file in the
# repository carries a value for it.
set -eu
cd "$(dirname "$0")/.."
status=0
if grep -n 'secrets.PENGUIN_MAIL_MICROSOFT_CLIENT_ID' .github/workflows/*.yml; then
    echo "The Microsoft client id's secret is MICROSOFT_CLIENT_ID." >&2
    status=1
fi
for file in snap/snapcraft.yaml packaging/flatpak/io.github.c9dev.penguin-mail.yml; do
    if ! grep -q 'PENGUIN_MAIL_MICROSOFT_CLIENT_ID: ""' "$file"; then
        echo "$file does not pass PENGUIN_MAIL_MICROSOFT_CLIENT_ID." >&2
        status=1
    fi
done
if [ "$(grep -c 'secrets.MICROSOFT_CLIENT_ID' .github/workflows/release.yml)" -lt 3 ]; then
    echo "release.yml must hand MICROSOFT_CLIENT_ID to the tree, the deb and the snap builds." >&2
    status=1
fi
exit $status
