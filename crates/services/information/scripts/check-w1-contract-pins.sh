#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
# shellcheck source=../w1-contracts.env
source "$repo_root/w1-contracts.env"

accepted_lifecycle="cbab33555ab9355a6ac453d659c55ec9e0666821"
accepted_vertical="fc24ffff08c52690390b4460f44617d5d9732563"

if [[ "$W1_PLATFORM_CONTRACTS_REV" != "$accepted_lifecycle" ]]; then
  echo "Wave 1 lifecycle revision is not accepted" >&2
  exit 1
fi
if [[ "$W1_VERTICAL_FIXTURES_REV" != "$accepted_vertical" ]]; then
  echo "Wave 1 vertical revision is not accepted" >&2
  exit 1
fi

lifecycle='platform-contracts-v0 = { path = "../../platform/contracts/crates/platform-contracts-v0" }'
vertical='platform-vertical-fixtures-v0 = { path = "../../platform/contracts/crates/platform-vertical-fixtures-v0" }'
fixture='platform-contracts-fixture-v0 = { package = "platform-contracts-v0", path = "../../platform/contracts/crates/platform-contracts-v0" }'
if [[ "$(grep -Fxc "$lifecycle" "$repo_root/Cargo.toml")" -ne 1 ]]; then
  echo "Wave 1 lifecycle dependency declaration is not exact" >&2
  exit 1
fi
if [[ "$(grep -Fxc "$vertical" "$repo_root/Cargo.toml")" -ne 1 ]] \
  || [[ "$(grep -Fxc "$fixture" "$repo_root/Cargo.toml")" -ne 1 ]]; then
  echo "Wave 1 vertical dependency declaration is not exact" >&2
  exit 1
fi
if rg -n -F 'github.com/delysis/w1-platform-contracts' "$repo_root" \
  --glob 'Cargo.toml' --glob 'Cargo.lock' --glob '!target/**'; then
  echo "external Wave 1 dependency found" >&2
  exit 1
fi

echo "Wave 1 contracts are local; historical lifecycle=$accepted_lifecycle vertical=$accepted_vertical"
