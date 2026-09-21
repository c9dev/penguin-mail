#!/usr/bin/env bash
# Reads and drafts CHANGELOG.md sections.
#
#   scripts/changelog.sh draft          bullets for the commits since the last tag
#   scripts/changelog.sh section 0.2.0  the body of that version's section
#
# A draft leaves out merges, release commits, and commits that touch only
# po/ or docs/: translation line numbers and notes mean nothing to someone
# installing the app. Commit subjects already say what changed, so a draft
# is mostly a matter of deleting lines.
set -euo pipefail

cd "$(dirname "$0")/.."

case ${1:-} in
draft)
    last=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
    range=${last:+$last..}HEAD
    git log --no-merges --format='%H %s' "$range" | while read -r sha subject; do
        case $subject in "Release "*) continue ;; esac
        if git diff-tree --no-commit-id --name-only -r "$sha" | grep -qvE '^(po|docs)/'; then
            echo "- $subject"
        fi
    done
    ;;
section)
    version=${2:?usage: scripts/changelog.sh section <version>}
    # Everything between this version's heading and the next one.
    awk -v v="$version" '
        /^## / { if (found) exit; if ($2 == v) { found = 1; next } }
        found { print }
        END { if (!found) exit 1 }
    ' CHANGELOG.md | sed -e '/./,$!d' | sed -e ':a' -e '/^\n*$/{$d;N;ba' -e '}'
    ;;
*)
    echo "usage: scripts/changelog.sh draft | section <version>" >&2
    exit 2
    ;;
esac
