#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
# shellcheck source=../w1-contracts.env
source "$repo_root/w1-contracts.env"

lifecycle_revision="$W1_PLATFORM_CONTRACTS_REV"
vertical_revision="$W1_VERTICAL_FIXTURES_REV"
repository="https://github.com/delysis/w1-platform-contracts"
for revision in "$lifecycle_revision" "$vertical_revision"; do
  if [[ ! "$revision" =~ ^[0-9a-f]{40}$ ]]; then
    echo "Wave 1 pin is not an exact 40-hex revision: $revision" >&2
    exit 1
  fi
done
if [[ "$lifecycle_revision" == "$vertical_revision" ]]; then
  echo "lifecycle and vertical fixtures must retain distinct accepted revisions" >&2
  exit 1
fi

manifest_lines="$({
  find "$repo_root" -name Cargo.toml \
    -not -path '*/.git/*' \
    -not -path '*/target/*' \
    -exec grep -H -F "$repository" {} +
} 2>/dev/null || true)"
manifest_count="$(printf '%s\n' "$manifest_lines" | grep -c . || true)"
if [[ "$manifest_count" -ne 3 ]]; then
  echo "expected three Wave 1 test-only dependency declarations, found $manifest_count" >&2
  exit 1
fi
if printf '%s\n' "$manifest_lines" | grep -Eq 'branch[[:space:]]*=|tag[[:space:]]*='; then
  echo "Wave 1 contract dependency must not use a branch or tag" >&2
  exit 1
fi
if [[ "$(printf '%s\n' "$manifest_lines" | grep -Fc "platform-contract-testkit = { git = \"$repository\", rev = \"$lifecycle_revision\"")" -ne 1 ]]; then
  echo "Wave 1 lifecycle dependency does not use its accepted revision exactly once" >&2
  exit 1
fi
if [[ "$(printf '%s\n' "$manifest_lines" | grep -Fc "platform-vertical-fixtures-v0 = { git = \"$repository\", rev = \"$vertical_revision\"")" -ne 2 ]]; then
  echo "Wave 1 vertical dependency does not use its protocol revision exactly twice" >&2
  exit 1
fi

lockfile="$repo_root/Cargo.lock"
lifecycle_lock_count="$(grep -Fc "?rev=$lifecycle_revision#$lifecycle_revision" "$lockfile" || true)"
if [[ "$lifecycle_lock_count" -ne 2 ]]; then
  echo "Cargo.lock has $lifecycle_lock_count lifecycle package sources, expected 2" >&2
  exit 1
fi
vertical_lock_count="$(grep -Fc "?rev=$vertical_revision#$vertical_revision" "$lockfile" || true)"
if [[ "$vertical_lock_count" -ne 2 ]]; then
  echo "Cargo.lock has $vertical_lock_count vertical package sources, expected 2" >&2
  exit 1
fi

if find "$repo_root" \
  \( -name Cargo.toml -o -name Cargo.lock -o -name w1-contracts.env \) \
  -not -path '*/.git/*' \
  -not -path '*/target/*' \
  -exec grep -qs 'efbbe' {} +; then
  echo "withdrawn Wave 1 revision prefix efbbe remains" >&2
  exit 1
fi

echo "Wave 1 pins coherent: lifecycle=$lifecycle_revision vertical=$vertical_revision"
