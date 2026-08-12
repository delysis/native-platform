#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

w1_contract_rev='cbab33555ab9355a6ac453d659c55ec9e0666821'
w1_contract_url='https://github.com/delysis/w1-platform-contracts.git'
w1_manifest='crates/loom-host/Cargo.toml'

contract_manifests="$(rg -l 'w1-platform-contracts' Cargo.toml crates/*/Cargo.toml || true)"
if [[ "$contract_manifests" != "$w1_manifest" ]]; then
  printf '%s\n' "W1 contract dependency must have exactly one manifest owner: $w1_manifest" >&2
  exit 1
fi

if ! rg -Fq "git = \"$w1_contract_url\", rev = \"$w1_contract_rev\", optional = true" "$w1_manifest"; then
  printf '%s\n' 'W1 contract dependency must use the approved exact optional revision' >&2
  exit 1
fi

if rg -n 'w1-platform-contracts[^\n]*(branch|tag)[[:space:]]*=' Cargo.toml crates/*/Cargo.toml; then
  printf '%s\n' 'moving W1 contract branch or tag dependencies are forbidden' >&2
  exit 1
fi

if [[ "$(rg -o 'w1-platform-contracts' Cargo.toml crates/*/Cargo.toml | wc -l | tr -d ' ')" -ne 1 ]]; then
  printf '%s\n' 'duplicate W1 contract dependency declarations are forbidden' >&2
  exit 1
fi

if ! rg -Fq "source = \"git+$w1_contract_url?rev=$w1_contract_rev#$w1_contract_rev\"" Cargo.lock; then
  printf '%s\n' 'Cargo.lock must resolve the exact approved W1 contract revision' >&2
  exit 1
fi
