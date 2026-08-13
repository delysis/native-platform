#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
manifest="$repo_root/crates/speech-native-host/Cargo.toml"
lockfile="$repo_root/Cargo.lock"

fail() {
  printf 'W1 contract pin violation: %s\n' "$1" >&2
  exit 1
}

path='../../../../platform/contracts/crates'
for declaration in \
  "platform-contract-testkit = { path = \"$path/platform-contract-testkit\", optional = true }" \
  "platform-contracts-v0 = { path = \"$path/platform-contracts-v0\", optional = true }" \
  "platform-vertical-fixtures-v0 = { path = \"$path/platform-vertical-fixtures-v0\", optional = true }"
do
  grep -Fqx "$declaration" "$manifest" \
    || fail "host manifest is not rebound to local W1 package: $declaration"
done
if grep -R -n -F --include='Cargo.toml' --include='Cargo.lock' \
  'github.com/delysis/w1-platform-contracts' "$repo_root"; then
  fail "external W1 contract source remains"
fi

grep -q '^unstable-w1-contract-tests = \[' "$manifest" \
  || fail "contract dependencies must remain behind the unstable test feature"
grep -q '^unstable-w1-vertical-tests = \[' "$manifest" \
  || fail "vertical protocol dependency must remain behind the unstable test feature"
grep -q '#\[cfg(feature = "unstable-w1-contract-tests")\]' \
  "$repo_root/crates/speech-native-host/src/lib.rs" \
  || fail "contract adapter source must remain feature-gated"

printf 'W1 contract dependencies resolve from the imported local source\n'
