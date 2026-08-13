#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

w1_contract_path='../../crates/platform/contracts/crates/platform-contract-testkit'
w1_protocol_path='../../crates/platform/contracts/crates/platform-contracts-v0'
w1_vertical_path='../../crates/platform/contracts/crates/platform-vertical-fixtures-v0'
w1_contract_manifest='crates/loom-host/Cargo.toml'
w1_vertical_manifests='crates/loom-backend-llama/Cargo.toml
crates/loom-store/Cargo.toml
crates/tauri-plugin-loom/Cargo.toml'

for declaration in \
  "platform-contract-testkit = { path = \"$w1_contract_path\" }" \
  "platform-contracts-v0-vertical = { package = \"platform-contracts-v0\", path = \"$w1_protocol_path\" }" \
  "platform-vertical-fixtures-v0 = { path = \"$w1_vertical_path\" }"
do
  if ! rg -Fqx "$declaration" Cargo.toml; then
    printf '%s\n' "missing imported W1 workspace dependency: $declaration" >&2
    exit 1
  fi
done

if ! rg -Fqx 'platform-contract-testkit = { workspace = true, optional = true }' "$w1_contract_manifest"; then
  printf '%s\n' 'W1 contract dependency must inherit the imported workspace path' >&2
  exit 1
fi

if rg -n 'github\.com/delysis/(w1-platform-contracts|llama-native-kit)' Cargo.toml crates/*/Cargo.toml Cargo.lock; then
  printf '%s\n' 'retired W1 or Native Git sources are forbidden after W7 cutover' >&2
  exit 1
fi

while IFS= read -r manifest; do
  if ! rg -Fqx 'platform-vertical-fixtures-v0 = { workspace = true, optional = true }' "$manifest"; then
    printf '%s\n' "W1 vertical dependency must inherit the imported workspace path: $manifest" >&2
    exit 1
  fi
done <<EOF
$w1_vertical_manifests
EOF

for manifest in crates/loom-backend-llama/Cargo.toml crates/tauri-plugin-loom/Cargo.toml; do
  if ! rg -Fqx 'platform-contracts-v0-vertical = { workspace = true, optional = true }' "$manifest"; then
    printf '%s\n' "W1 protocol dependency must inherit the imported workspace path: $manifest" >&2
    exit 1
  fi
done

if [[ "$(rg -l 'platform-(contract|vertical)' Cargo.toml crates/*/Cargo.toml | sort | wc -l | tr -d ' ')" -ne 5 ]]; then
  printf '%s\n' 'unexpected W1 dependency owner count' >&2
  exit 1
fi
