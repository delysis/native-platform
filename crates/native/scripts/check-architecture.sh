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

for task_file in crates/mom-llama-runtime/src/*.rs
do
  case "$task_file" in
    */mcp.rs) continue ;;
  esac
  if rg -n 'std::net|tokio::net|std::process|Command::new|reqwest|ureq|TcpStream|127\.0\.0\.1|localhost|https?://' "$task_file"
  then
    echo "forbidden network or process authority in $task_file" >&2
    exit 1
  fi
done

if ! rg -q 'std::process' crates/mom-llama-runtime/src/mcp.rs
then
  echo "the explicit MCP adapter no longer declares its process boundary" >&2
  exit 1
fi

if rg -n 'std::net|tokio::net|reqwest|ureq|TcpStream|127\.0\.0\.1|localhost|https?://' crates/mom-llama-runtime/src/mcp.rs
then
  echo "network authority is not allowed in the native-local MCP adapter" >&2
  exit 1
fi

if rg -n "(^|[\"' /])llama-(cli|server)([\"' /]|\$)" crates apps/mom-llama/src-tauri/src
then
  echo "inference executable references are forbidden from product source" >&2
  exit 1
fi

echo "architecture ok: inference is in-process; MCP is the sole bounded process adapter"
