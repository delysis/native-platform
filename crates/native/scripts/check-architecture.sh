#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

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
