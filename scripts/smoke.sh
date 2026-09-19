#!/usr/bin/env bash
# Manual smoke test against a real Gmail account. Run it by hand, never in CI.
# Needs ~/.config/penguin-mail/config.toml and at least one account; see docs/setup.md.
set -euo pipefail

cd "$(dirname "$0")/.."
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
