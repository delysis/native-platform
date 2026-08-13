#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
checker="$repo_root/scripts/check-w1-contract-pin.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
lifecycle_revision="cbab33555ab9355a6ac453d659c55ec9e0666821"
vertical_revision="fc24ffff08c52690390b4460f44617d5d9732563"

write_fixture() {
  rm -rf "$fixture"
  mkdir -p "$fixture/crate"
  printf '%s\n' \
    "W1_PLATFORM_CONTRACTS_REV=$lifecycle_revision" \
    "W1_VERTICAL_FIXTURES_REV=$vertical_revision" \
    > "$fixture/w1-contracts.env"
  printf '%s\n' "$1" > "$fixture/crate/Cargo.toml"
  printf '%s\n' \
    'name = "platform-contract-testkit"' \
    'name = "platform-contracts-v0"' \
    'name = "platform-vertical-fixtures-v0"' \
    > "$fixture/Cargo.lock"
}

exact='platform-contract-testkit = { path = "../../../../crates/platform/contracts/crates/platform-contract-testkit", optional = true }
platform-vertical-fixtures-v0 = { path = "../../../../crates/platform/contracts/crates/platform-vertical-fixtures-v0", optional = true }
platform-vertical-fixtures-v0 = { path = "../../../../crates/platform/contracts/crates/platform-vertical-fixtures-v0", optional = true }
platform-vertical-fixtures-v0 = { path = "../../../crates/platform/contracts/crates/platform-vertical-fixtures-v0", optional = true }'
write_fixture "$exact"
"$checker" "$fixture" "$fixture/Cargo.lock"

printf '%s\n' \
  "W1_PLATFORM_CONTRACTS_REV=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" \
  "W1_VERTICAL_FIXTURES_REV=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" \
  > "$fixture/w1-contracts.env"
if "$checker" "$fixture" "$fixture/Cargo.lock" >/dev/null 2>&1; then
  echo "self-consistent but unaccepted revisions unexpectedly passed" >&2
  exit 1
fi
write_fixture "$exact"

write_fixture "${exact/platform-contract-testkit\", optional/platform-contract-testkit-wrong\", optional}"
if "$checker" "$fixture" "$fixture/Cargo.lock" >/dev/null 2>&1; then
  echo "wrong local contract path unexpectedly passed" >&2
  exit 1
fi

write_fixture "$exact
source = \"git+https://github.com/delysis/w1-platform-contracts?rev=$lifecycle_revision#$lifecycle_revision\""
if "$checker" "$fixture" "$fixture/Cargo.lock" >/dev/null 2>&1; then
  echo "external contract source unexpectedly passed" >&2
  exit 1
fi

write_fixture "${exact/, optional = true/}"
if "$checker" "$fixture" "$fixture/Cargo.lock" >/dev/null 2>&1; then
  echo "non-optional contract dependency unexpectedly passed" >&2
  exit 1
fi

write_fixture "$(printf '%s\n' "$exact" | sed 's/^/# /')"
if "$checker" "$fixture" "$fixture/Cargo.lock" >/dev/null 2>&1; then
  echo "commented dependency declarations unexpectedly passed" >&2
  exit 1
fi

write_fixture "$exact
platform-contract-testkit = { path = \"../../../../crates/platform/contracts/crates/platform-contract-testkit\", optional = true }"
if "$checker" "$fixture" "$fixture/Cargo.lock" >/dev/null 2>&1; then
  echo "duplicate contract pin unexpectedly passed" >&2
  exit 1
fi

echo "Wave 1 contract pin policy fixtures passed"
