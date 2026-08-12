#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
# shellcheck source=../w1-contracts.env
source "$repo_root/w1-contracts.env"

accepted_lifecycle_revision="cbab33555ab9355a6ac453d659c55ec9e0666821"
accepted_vertical_revision="fc24ffff08c52690390b4460f44617d5d9732563"
lifecycle_revision="$W1_PLATFORM_CONTRACTS_REV"
vertical_revision="$W1_VERTICAL_FIXTURES_REV"
repository="https://github.com/delysis/w1-platform-contracts"
for revision in "$lifecycle_revision" "$vertical_revision"; do
  if [[ ! "$revision" =~ ^[0-9a-f]{40}$ ]]; then
    echo "Wave 1 pin is not an exact 40-hex revision: $revision" >&2
    exit 1
  fi
done
if [[ "$lifecycle_revision" != "$accepted_lifecycle_revision" ]]; then
  echo "Wave 1 lifecycle pin is not the accepted revision: $lifecycle_revision" >&2
  exit 1
fi
if [[ "$vertical_revision" != "$accepted_vertical_revision" ]]; then
  echo "Wave 1 vertical pin is not the accepted revision: $vertical_revision" >&2
  exit 1
fi
if [[ "$lifecycle_revision" == "$vertical_revision" ]]; then
  echo "lifecycle and vertical fixtures must retain distinct accepted revisions" >&2
  exit 1
fi

manifest_lines="$({
  find "$repo_root" \
    \( -path '*/.git' -o -path '*/target' \) -prune -o \
    -name Cargo.toml \
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
lifecycle_declaration="platform-contract-testkit = { git = \"$repository\", rev = \"$lifecycle_revision\", optional = true }"
vertical_declaration="platform-vertical-fixtures-v0 = { git = \"$repository\", rev = \"$vertical_revision\", optional = true }"
if printf '%s\n' "$manifest_lines" | grep -Eq '^[^:]+:[[:space:]]*#'; then
  echo "commented Wave 1 dependencies do not satisfy the pin policy" >&2
  exit 1
fi
if [[ "$(printf '%s\n' "$manifest_lines" | grep -Fc "$lifecycle_declaration")" -ne 1 ]]; then
  echo "Wave 1 lifecycle dependency does not use its accepted revision exactly once" >&2
  exit 1
fi
if [[ "$(printf '%s\n' "$manifest_lines" | grep -Fc "$vertical_declaration")" -ne 2 ]]; then
  echo "Wave 1 vertical dependency does not use its protocol revision exactly twice" >&2
  exit 1
fi
if printf '%s\n' "$manifest_lines" \
  | grep -Fv "$lifecycle_declaration" \
  | grep -Fv "$vertical_declaration" \
  | grep -q .; then
  echo "unrecognized Wave 1 dependency declaration" >&2
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
  \( -path '*/.git' -o -path '*/target' \) -prune -o \
  \( -name Cargo.toml -o -name Cargo.lock -o -name w1-contracts.env \) \
  -exec grep -qs 'efbbe' {} +; then
  echo "withdrawn Wave 1 revision prefix efbbe remains" >&2
  exit 1
fi

echo "Wave 1 pins coherent: lifecycle=$lifecycle_revision vertical=$vertical_revision"
