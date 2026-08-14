#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

fail() {
  printf 'speech boundary violation: %s\n' "$1" >&2
  exit 1
}

if grep -R -n -E --include='*.rs' --include='Cargo.toml' \
  'fte[-_](types|router|providers|store|protocols|loopback|backend)|free_token_energy' \
  "$repo_root/crates"; then
  fail "speech-native-kit depends on the Free Token Energy text/provider gateway"
fi

if grep -R -n -E --include='*.rs' --include='Cargo.toml' \
  '(^|[^[:alnum:]_])(axum|reqwest|hyper|tower_http)([^[:alnum:]_]|$)' \
  "$repo_root/crates"; then
  fail "speech-native-kit contains network or loopback authority"
fi

printf 'speech-native-kit boundaries verified\n'
