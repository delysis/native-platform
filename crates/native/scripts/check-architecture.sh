#!/bin/sh
set -eu

repo_root="$(git rev-parse --show-toplevel)"
native_root="$repo_root/crates/native"
cd "$repo_root"

reject_matches() {
  boundary_message=$1
  shift
  if "$@"
  then
    printf '%s\n' "$boundary_message" >&2
    exit 1
  else
    scan_status=$?
    if [ "$scan_status" -ne 1 ]
    then
      printf 'architecture scan did not complete (%s exit %s)\n' "$1" "$scan_status" >&2
      exit 1
    fi
  fi
}

for task_dir in \
  crates/native/crates/llama-native-types/src \
  crates/native/crates/llama-native-engine/src \
  crates/native/crates/llama-native-cache/src \
  crates/native/crates/llama-native-host/src
do
  reject_matches "forbidden network or process authority in $task_dir" \
    grep -REn 'std::net|tokio::net|std::process|Command::new|reqwest|ureq|TcpStream|127\.0\.0\.1|localhost|https?://' "$task_dir"
done

reject_matches "product, gateway, or Tauri dependencies are forbidden in the native runtime" \
  grep -En 'mom-llama|free-token-energy|fte-|tauri' crates/native/crates/*/Cargo.toml

if forbidden_product_paths=$(find "$native_root" -maxdepth 3 -type d \( -name 'mom-llama*' -o -path '*/apps/*' \))
then
  if [ -n "$forbidden_product_paths" ]
  then
    echo "product source is forbidden in the runtime repository" >&2
    exit 1
  fi
else
  scan_status=$?
  printf 'architecture directory scan did not complete (find exit %s)\n' "$scan_status" >&2
  exit 1
fi

reject_matches "inference executable references are forbidden from native source" \
  grep -REn "(^|[\"' /])llama-(cli|server)([\"' /]|\$)" crates/native/crates

echo "architecture ok: this repository contains only in-process native runtime crates"
