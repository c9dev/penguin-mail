#!/usr/bin/env bash
# Publishes a new version of Penguin Mail. Bumps the version, writes its
# changelog section, brings the translation template up to the new version,
# runs the gate, commits "Release X.Y.Z", tags vX.Y.Z,
# and pushes. The pushed tag starts .github/workflows/release.yml, which
# builds the .deb, the tarball and the zip and publishes the release.
#
#   scripts/release.sh              0.1.0 -> 0.1.1
#   scripts/release.sh minor        0.1.0 -> 0.2.0
#   scripts/release.sh major        0.1.0 -> 1.0.0
#   scripts/release.sh 0.1.0        exactly this version, even the current one
#   scripts/release.sh --dry-run    everything up to the commit, then put it all back
set -euo pipefail

cd "$(dirname "$0")/.."

bump=patch
dry=
for arg in "$@"; do
    case $arg in
    --dry-run) dry=1 ;;
    patch | minor | major) bump=$arg ;;
    [0-9]*.[0-9]*.[0-9]*) bump=$arg ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

fail() { echo "release: $*" >&2; exit 1; }

[ -z "$(git status --porcelain)" ] || fail "the tree has uncommitted changes"
[ "$(git branch --show-current)" = main ] || fail "releases come from main"
git fetch -q origin main --tags
[ -z "$(git log --oneline HEAD..origin/main)" ] || fail "origin/main has commits this branch lacks; pull first"

current=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
IFS=. read -r major minor patch <<<"$current"
case $bump in
patch) version="$major.$minor.$((patch + 1))" ;;
minor) version="$major.$((minor + 1)).0" ;;
major) version="$((major + 1)).0.0" ;;
*) version=$bump ;;
esac
git rev-parse -q --verify "refs/tags/v$version" >/dev/null && fail "v$version is already tagged"

# Whatever stops the script from here on puts these files back.
restore() { git checkout -q -- Cargo.toml Cargo.lock CHANGELOG.md po; }
trap restore EXIT

sed -i "/^\[workspace.package\]/,/^\[/s/^version = \".*\"/version = \"$version\"/" Cargo.toml
cargo update -q --workspace
# The translation template names the version in its header.
scripts/update-po.sh >/dev/null

# Changes are written up for people as they land, under "## Unreleased".
# That section is the draft; the commits since the last release sit below
# it, commented out, for anything the write-up missed.
notes=$(mktemp)
{
    echo "# What changed in Penguin Mail $version, for the people who use it. One"
    echo "# short line per change, in plain words, under ### New, ### Improved or"
    echo "# ### Fixed. Lines starting with \"# \" are dropped. With no line starting"
    echo "# \"- \" left, the release stops."
    echo "#"
    scripts/changelog.sh section Unreleased || true
    echo
    echo "# Commits since the last release, for reference:"
    scripts/changelog.sh draft | sed 's/^/# /'
} > "$notes"
"${VISUAL:-${EDITOR:-nano}}" "$notes"
body=$(grep -v -E '^#( |$)' "$notes" | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}')
rm -f "$notes"
grep -q '^- ' <<<"$body" || fail "the changelog lists no changes"

# The release takes the Unreleased section's place, and an empty Unreleased
# heading goes back on top for the next round.
section="## $version ($(date +%Y-%m-%d))

$body"
awk -v s="$section" '
    /^## Unreleased/ { print; print ""; print s; print ""; skipping = 1; placed = 1; next }
    skipping && /^## / { skipping = 0 }
    skipping { next }
    !placed && /^## / { print "## Unreleased"; print ""; print s; print ""; placed = 1 }
    { print }
    END { if (!placed) { print ""; print "## Unreleased"; print ""; print s } }
' CHANGELOG.md > CHANGELOG.md.new
mv CHANGELOG.md.new CHANGELOG.md

echo "Running the gate for $version."
cargo test --workspace >/dev/null || fail "cargo test failed"
cargo clippy --workspace --all-targets -- -D warnings >/dev/null 2>&1 || fail "clippy failed"
scripts/update-po.sh --check || fail "the translation template is stale; run scripts/update-po.sh and commit"

if [ -n "$dry" ]; then
    echo
    echo "Would commit \"Release $version\" and tag v$version with:"
    echo
    scripts/changelog.sh section "$version"
    exit 0
fi

git add Cargo.toml Cargo.lock CHANGELOG.md po
git commit -q -m "Release $version"
trap - EXIT
git tag -a "v$version" -F <(scripts/changelog.sh section "$version")
git push -q origin main "v$version"
echo "Pushed v$version. GitHub builds and publishes it:"
echo "  https://github.com/c9dev/penguin-mail/actions/workflows/release.yml"
