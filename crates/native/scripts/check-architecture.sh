#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

w1_contract_rev='cbab33555ab9355a6ac453d659c55ec9e0666821'
w1_vertical_rev='9fd803f5efcc46ac0256dab876e7c0b1f03bb448'
w1_contract_url='https://github.com/delysis/w1-platform-contracts.git'
w1_manifest='crates/llama-native-engine/Cargo.toml'

contract_manifests="$(rg -l 'w1-platform-contracts' Cargo.toml crates/*/Cargo.toml || true)"
if [ "$contract_manifests" != "$w1_manifest" ]
then
  echo "W1 contract dependency must have exactly one manifest owner: $w1_manifest" >&2
  exit 1
fi
if ! rg -Fq "git = \"$w1_contract_url\", rev = \"$w1_contract_rev\", optional = true" "$w1_manifest"
then
  echo "W1 contract dependency must use the approved exact optional revision" >&2
  exit 1
fi
if rg -n 'w1-platform-contracts[^\n]*(branch|tag)[[:space:]]*=' Cargo.toml crates/*/Cargo.toml
then
  echo "moving W1 contract branch or tag dependencies are forbidden" >&2
  exit 1
fi
if ! rg -Fq "platform-vertical-fixtures-v0 = { git = \"$w1_contract_url\", rev = \"$w1_vertical_rev\", optional = true }" "$w1_manifest"
then
  echo "W1 vertical dependency must use the approved exact optional revision" >&2
  exit 1
fi
if ! rg -Fq "platform-contracts-v0-vertical = { package = \"platform-contracts-v0\", git = \"$w1_contract_url\", rev = \"$w1_vertical_rev\", optional = true }" "$w1_manifest"
then
  echo "W1 vertical contract types must use the matching approved exact optional revision" >&2
  exit 1
fi
if [ "$(rg -o 'w1-platform-contracts' Cargo.toml crates/*/Cargo.toml | wc -l | tr -d ' ')" -ne 3 ]
then
  echo "unexpected W1 dependency declaration count" >&2
  exit 1
fi
if ! rg -Fq "git+$w1_contract_url?rev=$w1_contract_rev#$w1_contract_rev" Cargo.lock
then
  echo "Cargo.lock must resolve the exact approved W1 contract revision" >&2
  exit 1
fi
if ! rg -Fq "git+$w1_contract_url?rev=$w1_vertical_rev#$w1_vertical_rev" Cargo.lock
then
  echo "Cargo.lock must resolve the exact approved W1 vertical revision" >&2
  exit 1
fi

for task_dir in \
  crates/llama-native-types/src \
  crates/llama-native-engine/src \
  crates/llama-native-cache/src \
  crates/llama-native-host/src
do
  if rg -n 'std::net|tokio::net|std::process|Command::new|reqwest|ureq|TcpStream|127\.0\.0\.1|localhost|https?://' "$task_dir"
  then
    echo "forbidden network or process authority in $task_dir" >&2
    exit 1
  fi
done

if rg -n 'mom-llama|free-token-energy|fte-|tauri' Cargo.toml crates/*/Cargo.toml
then
  echo "product, gateway, or Tauri dependencies are forbidden in the native runtime" >&2
  exit 1
fi

if find . -maxdepth 3 -type d \( -name 'mom-llama*' -o -path './apps/*' \) | grep -q .
then
  echo "product source is forbidden in the runtime repository" >&2
  exit 1
fi

if rg -n "(^|[\"' /])llama-(cli|server)([\"' /]|\$)" crates
then
  echo "inference executable references are forbidden from native source" >&2
  exit 1
fi

echo "architecture ok: this repository contains only in-process native runtime crates"
