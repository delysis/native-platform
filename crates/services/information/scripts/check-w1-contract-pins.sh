#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
# shellcheck source=../w1-contracts.env
source "$repo_root/w1-contracts.env"

accepted_lifecycle="cbab33555ab9355a6ac453d659c55ec9e0666821"
accepted_vertical="fc24ffff08c52690390b4460f44617d5d9732563"
repository="https://github.com/delysis/w1-platform-contracts"

if [[ "$W1_PLATFORM_CONTRACTS_REV" != "$accepted_lifecycle" ]]; then
  echo "Wave 1 lifecycle revision is not accepted" >&2
  exit 1
fi
if [[ "$W1_VERTICAL_FIXTURES_REV" != "$accepted_vertical" ]]; then
  echo "Wave 1 vertical revision is not accepted" >&2
  exit 1
fi

lifecycle="platform-contracts-v0 = { git = \"$repository\", rev = \"$accepted_lifecycle\" }"
vertical="platform-vertical-fixtures-v0 = { git = \"$repository\", rev = \"$accepted_vertical\" }"
if [[ "$(grep -Fxc "$lifecycle" "$repo_root/Cargo.toml")" -ne 1 ]]; then
  echo "Wave 1 lifecycle dependency declaration is not exact" >&2
  exit 1
fi
if [[ "$(grep -Fxc "$vertical" "$repo_root/Cargo.toml")" -ne 1 ]]; then
  echo "Wave 1 vertical dependency declaration is not exact" >&2
  exit 1
fi
if rg -n 'w1-platform-contracts.*(branch|tag)[[:space:]]*=' "$repo_root" \
  --glob 'Cargo.toml' --glob '!target/**'; then
  echo "moving Wave 1 dependency found" >&2
  exit 1
fi

lockfile="$repo_root/Cargo.lock"
if [[ "$(grep -Fc "?rev=$accepted_lifecycle#$accepted_lifecycle" "$lockfile")" -ne 1 ]]; then
  echo "Cargo.lock lifecycle source count is not exact" >&2
  exit 1
fi
if [[ "$(grep -Fc "?rev=$accepted_vertical#$accepted_vertical" "$lockfile")" -ne 2 ]]; then
  echo "Cargo.lock vertical source count is not exact" >&2
  exit 1
fi

echo "Wave 1 pins coherent: lifecycle=$accepted_lifecycle vertical=$accepted_vertical"
