#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
checker="$repo_root/scripts/check-w1-contract-pin.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
revision="da22fa893ac183c5d9df972a7e67215c0d92b383"
repository="https://github.com/delysis/w1-platform-contracts"

write_fixture() {
  rm -rf "$fixture"
  mkdir -p "$fixture/crate"
  printf '%s\n' "W1_PLATFORM_CONTRACTS_REV=$revision" > "$fixture/w1-contracts.env"
  printf '%s\n' "$1" > "$fixture/crate/Cargo.toml"
  printf '%s\n' \
    "source = \"git+$repository?rev=$revision#$revision\"" \
    "source = \"git+$repository?rev=$revision#$revision\"" \
    > "$fixture/Cargo.lock"
}

exact="platform-contract-testkit = { git = \"$repository\", rev = \"$revision\" }"
write_fixture "$exact"
"$checker" "$fixture"

write_fixture "platform-contract-testkit = { git = \"$repository\", branch = \"main\", rev = \"$revision\" }"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "branch contract dependency unexpectedly passed" >&2
  exit 1
fi

write_fixture "platform-contract-testkit = { git = \"$repository\", tag = \"v0\", rev = \"$revision\" }"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "tag contract dependency unexpectedly passed" >&2
  exit 1
fi

write_fixture "$exact
platform-contracts-v0 = { git = \"$repository\", rev = \"$revision\" }"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "duplicate contract pin unexpectedly passed" >&2
  exit 1
fi

echo "Wave 1 contract pin policy fixtures passed"
