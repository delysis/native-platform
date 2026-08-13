#!/bin/sh
set -eu

repo_root="$(git rev-parse --show-toplevel)"
native_root="$repo_root/crates/native"
cd "$repo_root"

w1_manifest='crates/native/crates/llama-native-engine/Cargo.toml'
w1_path='../../../platform/contracts/crates'

for declaration in \
  "platform-contract-testkit = { path = \"$w1_path/platform-contract-testkit\", optional = true }" \
  "platform-contracts-v0-vertical = { package = \"platform-contracts-v0\", path = \"$w1_path/platform-contracts-v0\", optional = true }" \
  "platform-vertical-fixtures-v0 = { path = \"$w1_path/platform-vertical-fixtures-v0\", optional = true }"
do
  if ! grep -Fqx "$declaration" "$w1_manifest"; then
    echo "W1 contract dependency is not rebound to the imported local package: $declaration" >&2
    exit 1
  fi
done
if grep -R -n -F 'github.com/delysis/w1-platform-contracts' \
  crates/native --include='Cargo.toml' --include='Cargo.lock'; then
  echo "external W1 contract source remains in Native" >&2
  exit 1
fi

for task_dir in \
  crates/native/crates/llama-native-types/src \
  crates/native/crates/llama-native-engine/src \
  crates/native/crates/llama-native-cache/src \
  crates/native/crates/llama-native-host/src
do
  if grep -REn 'std::net|tokio::net|std::process|Command::new|reqwest|ureq|TcpStream|127\.0\.0\.1|localhost|https?://' "$task_dir"
  then
    echo "forbidden network or process authority in $task_dir" >&2
    exit 1
  fi
done

if grep -En 'mom-llama|free-token-energy|fte-|tauri' crates/native/crates/*/Cargo.toml
then
  echo "product, gateway, or Tauri dependencies are forbidden in the native runtime" >&2
  exit 1
fi

if find "$native_root" -maxdepth 3 -type d \( -name 'mom-llama*' -o -path '*/apps/*' \) | grep -q .
then
  echo "product source is forbidden in the runtime repository" >&2
  exit 1
fi

if grep -REn "(^|[\"' /])llama-(cli|server)([\"' /]|\$)" crates/native/crates
then
  echo "inference executable references are forbidden from native source" >&2
  exit 1
fi

echo "architecture ok: this repository contains only in-process native runtime crates"
