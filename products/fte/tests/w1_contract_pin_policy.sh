#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
checker="$repo_root/scripts/check-w1-contract-pin.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
lifecycle_revision="cbab33555ab9355a6ac453d659c55ec9e0666821"
vertical_revision="9fd803f5efcc46ac0256dab876e7c0b1f03bb448"
repository="https://github.com/delysis/w1-platform-contracts"

write_fixture() {
  rm -rf "$fixture"
  mkdir -p "$fixture/crate"
  printf '%s\n' \
    "W1_PLATFORM_CONTRACTS_REV=$lifecycle_revision" \
    "W1_VERTICAL_FIXTURES_REV=$vertical_revision" \
    > "$fixture/w1-contracts.env"
  printf '%s\n' "$1" > "$fixture/crate/Cargo.toml"
  printf '%s\n' \
    "source = \"git+$repository?rev=$lifecycle_revision#$lifecycle_revision\"" \
    "source = \"git+$repository?rev=$lifecycle_revision#$lifecycle_revision\"" \
    "source = \"git+$repository?rev=$vertical_revision#$vertical_revision\"" \
    "source = \"git+$repository?rev=$vertical_revision#$vertical_revision\"" \
    > "$fixture/Cargo.lock"
}

exact="platform-contract-testkit = { git = \"$repository\", rev = \"$lifecycle_revision\" }
platform-vertical-fixtures-v0 = { git = \"$repository\", rev = \"$vertical_revision\", optional = true }
platform-vertical-fixtures-v0 = { git = \"$repository\", rev = \"$vertical_revision\", optional = true }"
write_fixture "$exact"
"$checker" "$fixture"

write_fixture "${exact/ rev = \"$lifecycle_revision\"/ branch = \"main\", rev = \"$lifecycle_revision\"}"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "branch contract dependency unexpectedly passed" >&2
  exit 1
fi

write_fixture "${exact/ rev = \"$lifecycle_revision\"/ tag = \"v0\", rev = \"$lifecycle_revision\"}"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "tag contract dependency unexpectedly passed" >&2
  exit 1
fi

write_fixture "$exact
platform-contracts-v0 = { git = \"$repository\", rev = \"$vertical_revision\" }"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "duplicate contract pin unexpectedly passed" >&2
  exit 1
fi

echo "Wave 1 contract pin policy fixtures passed"
