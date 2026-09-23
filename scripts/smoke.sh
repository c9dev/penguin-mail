#!/usr/bin/env bash
# Manual smoke test against a real Gmail account. Run it by hand, never in CI.
# Needs at least one account; see docs/setup.md.
set -euo pipefail

cd "$(dirname "$0")/.."
# An account on the built-in client syncs only in a build that carries it.
if [ -f packaging/secrets.env ]; then
    set -a
    # shellcheck source=/dev/null
    . packaging/secrets.env
    set +a
fi
cargo build --quiet -p mailrs-cli
cli=target/debug/penguin-mail-cli

echo "== accounts"
"$cli" account list

echo "== syncing for 60 seconds"
timeout --signal=INT 60 "$cli" sync || true

echo "== unified inbox"
"$cli" threads --limit 10

echo
echo "Pick a thread id from the list and try:"
echo "  $cli show <email> <thread-id>"
echo "  $cli triage <email> <thread-id> star"
echo "  $cli triage <email> <thread-id> unstar"
