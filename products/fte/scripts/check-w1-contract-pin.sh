#!/usr/bin/env bash
set -euo pipefail

product_root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
lockfile="${2:-$(cd "$product_root/../.." && pwd)/Cargo.lock}"
# shellcheck source=../w1-contracts.env
source "$product_root/w1-contracts.env"

accepted_lifecycle_revision="cbab33555ab9355a6ac453d659c55ec9e0666821"
accepted_vertical_revision="fc24ffff08c52690390b4460f44617d5d9732563"
lifecycle_revision="$W1_PLATFORM_CONTRACTS_REV"
vertical_revision="$W1_VERTICAL_FIXTURES_REV"
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
  find "$product_root" \
    \( -path '*/.git' -o -path '*/target' \) -prune -o \
    -name Cargo.toml \
    -exec grep -H -E 'platform-(contract-testkit|vertical-fixtures-v0)[[:space:]]*=[[:space:]]*\{[[:space:]]*path[[:space:]]*=' {} +
} 2>/dev/null || true)"
manifest_count="$(printf '%s\n' "$manifest_lines" | grep -c . || true)"
if [[ "$manifest_count" -ne 4 ]]; then
  echo "expected four Wave 1 test-only dependency declarations, found $manifest_count" >&2
  exit 1
fi
if grep -R -n -F --include='Cargo.toml' --include='Cargo.lock' \
  'github.com/delysis/w1-platform-contracts' "$product_root" "$lockfile"; then
  echo "external Wave 1 contract source remains" >&2
  exit 1
fi
lifecycle_declaration='platform-contract-testkit = { path = "../../../../crates/platform/contracts/crates/platform-contract-testkit", optional = true }'
vertical_declaration='platform-vertical-fixtures-v0 = { path = "../../../../crates/platform/contracts/crates/platform-vertical-fixtures-v0", optional = true }'
app_vertical_declaration='platform-vertical-fixtures-v0 = { path = "../../../crates/platform/contracts/crates/platform-vertical-fixtures-v0", optional = true }'
if printf '%s\n' "$manifest_lines" | grep -Eq '^[^:]+:[[:space:]]*#'; then
  echo "commented Wave 1 dependencies do not satisfy the pin policy" >&2
  exit 1
fi
if [[ "$(printf '%s\n' "$manifest_lines" | grep -Fc "$lifecycle_declaration")" -ne 1 ]]; then
  echo "Wave 1 lifecycle dependency does not use its accepted revision exactly once" >&2
  exit 1
fi
if [[ "$(printf '%s\n' "$manifest_lines" | grep -Fc "$vertical_declaration")" -ne 2 ]] \
  || [[ "$(printf '%s\n' "$manifest_lines" | grep -Fc "$app_vertical_declaration")" -ne 1 ]]; then
  echo "Wave 1 vertical dependencies are not rebound to the imported local package" >&2
  exit 1
fi
if printf '%s\n' "$manifest_lines" \
  | grep -Fv "$lifecycle_declaration" \
  | grep -Fv "$vertical_declaration" \
  | grep -Fv "$app_vertical_declaration" \
  | grep -q .; then
  echo "unrecognized Wave 1 dependency declaration" >&2
  exit 1
fi

if find "$product_root" \
  \( -path '*/.git' -o -path '*/target' \) -prune -o \
  \( -name Cargo.toml -o -name Cargo.lock -o -name w1-contracts.env \) \
  -exec grep -qs 'efbbe' {} +; then
  echo "withdrawn Wave 1 revision prefix efbbe remains" >&2
  exit 1
fi

echo "Wave 1 contracts are local; historical lifecycle=$lifecycle_revision vertical=$vertical_revision"
