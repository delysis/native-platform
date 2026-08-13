#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
checker="$repo_root/scripts/check-w1-contract-pins.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

copy_fixture() {
  rm -rf "$fixture"
  mkdir -p "$fixture"
  cp "$repo_root/Cargo.toml" "$fixture/Cargo.toml"
  cp "$repo_root/Cargo.lock" "$fixture/Cargo.lock"
  cp "$repo_root/w1-contracts.env" "$fixture/w1-contracts.env"
}

copy_fixture
"$checker" "$fixture"

sed -i.bak 's/cbab33555ab9355a6ac453d659c55ec9e0666821/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/g' "$fixture/Cargo.toml" "$fixture/Cargo.lock" "$fixture/w1-contracts.env"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "self-consistent lifecycle mutation unexpectedly passed" >&2
  exit 1
fi

copy_fixture
sed -i.bak 's/fc24ffff08c52690390b4460f44617d5d9732563/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/g' "$fixture/Cargo.toml" "$fixture/Cargo.lock" "$fixture/w1-contracts.env"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "self-consistent vertical mutation unexpectedly passed" >&2
  exit 1
fi

copy_fixture
sed -i.bak 's#../../platform/contracts/crates/platform-vertical-fixtures-v0#../../platform/contracts-wrong/crates/platform-vertical-fixtures-v0#' "$fixture/Cargo.toml"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "wrong local vertical path unexpectedly passed" >&2
  exit 1
fi

copy_fixture
printf '%s\n' 'source = "git+https://github.com/delysis/w1-platform-contracts?branch=main"' >> "$fixture/Cargo.lock"
if "$checker" "$fixture" >/dev/null 2>&1; then
  echo "external moving dependency unexpectedly passed" >&2
  exit 1
fi

echo "Wave 1 pin policy fixtures passed"
