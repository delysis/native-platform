#!/bin/sh
set -eu

repo_root="$(git rev-parse --show-toplevel)"
native_root="$repo_root/crates/native"
cd "$repo_root"

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
