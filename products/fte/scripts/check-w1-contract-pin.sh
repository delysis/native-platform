#!/usr/bin/env bash
set -euo pipefail

repo_root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
# shellcheck source=../w1-contracts.env
source "$repo_root/w1-contracts.env"

revision="$W1_PLATFORM_CONTRACTS_REV"
repository="https://github.com/delysis/w1-platform-contracts"
if [[ ! "$revision" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Wave 1 contract pin is not an exact 40-hex revision: $revision" >&2
  exit 1
fi

manifest_lines="$({
  find "$repo_root" -name Cargo.toml \
    -not -path '*/.git/*' \
    -not -path '*/target/*' \
    -exec grep -H -F "$repository" {} +
} 2>/dev/null || true)"
manifest_count="$(printf '%s\n' "$manifest_lines" | grep -c . || true)"
if [[ "$manifest_count" -ne 1 ]]; then
  echo "expected one Wave 1 contract dependency, found $manifest_count" >&2
  exit 1
fi
if printf '%s\n' "$manifest_lines" | grep -Eq 'branch[[:space:]]*=|tag[[:space:]]*='; then
  echo "Wave 1 contract dependency must not use a branch or tag" >&2
  exit 1
fi
if ! printf '%s\n' "$manifest_lines" | grep -Fq "rev = \"$revision\""; then
  echo "Wave 1 contract dependency does not use the approved revision" >&2
  exit 1
fi

lockfile="$repo_root/Cargo.lock"
lock_count="$(grep -Fc "?rev=$revision#$revision" "$lockfile" || true)"
if [[ "$lock_count" -ne 2 ]]; then
  echo "Cargo.lock has $lock_count Wave 1 package sources, expected 2" >&2
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

echo "Wave 1 contract pin coherent: $revision"
