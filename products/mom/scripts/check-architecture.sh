#!/bin/sh
set -eu

product_root=$(cd "$(dirname "$0")/.." && pwd)
repo_root=$(git -C "$product_root" rev-parse --show-toplevel)
root_manifest="$repo_root/Cargo.toml"
root_lock="$repo_root/Cargo.lock"
cd "$product_root"

if find crates -mindepth 1 -maxdepth 1 -type d -name 'llama-native-*' | grep -q .
then
  echo "copied llama-native crates are forbidden; use the pinned runtime dependency" >&2
  exit 1
fi

if find crates -mindepth 1 -maxdepth 1 -type d -name 'attachment-native-*' | grep -q .
then
  echo "copied attachment-native crates are forbidden; use the pinned attachment dependency" >&2
  exit 1
fi

if rg -n '(fte-speech-|speech-native-|tauri-plugin-(fte-speech|free-token-energy-speech|speech-native))' --glob 'Cargo.toml' .
then
  echo "speech dependencies require deliberate product UX and a separate permission review" >&2
  exit 1
fi

if rg -n '(llama|attachment)-native-[a-z-]+\s*=\s*\{\s*path\s*=\s*"\.\.' --glob 'Cargo.toml' .
then
  echo "Mom child manifests must inherit imported dependencies from the root workspace" >&2
  exit 1
fi

cargo metadata --locked --format-version 1 >/dev/null

for dependency in \
  'llama-native-cache = { path = "crates/native/crates/llama-native-cache" }' \
  'llama-native-engine = { path = "crates/native/crates/llama-native-engine" }' \
  'llama-native-host = { path = "crates/native/crates/llama-native-host" }' \
  'llama-native-types = { path = "crates/native/crates/llama-native-types" }' \
  'fte-router = { path = "products/fte/crates/fte-router" }' \
  'fte-store = { path = "products/fte/crates/fte-store" }' \
  'fte-types = { path = "products/fte/crates/fte-types" }' \
  'attachment-native-host = { path = "crates/services/attachment/crates/attachment-native-host" }' \
  'attachment-native-types = { path = "crates/services/attachment/crates/attachment-native-types" }'
do
  if ! grep -Fqx "$dependency" "$root_manifest"
  then
    echo "missing exact imported root dependency: $dependency" >&2
    exit 1
  fi
done

if rg -n 'source = "git\+https://github\.com/delysis/(mom-llama|llama-native-kit|free-token-energy|attachment-native-kit)' "$root_lock"
then
  echo "the root lock retains a retired first-party Git source used by Mom" >&2
  exit 1
fi

mom_tree=$(cargo tree --locked -p mom-llama-app --prefix none)
if printf '%s\n' "$mom_tree" | rg -n '^(fte-speech-|speech-native-|tauri-plugin-(free-token-energy-speech|speech-native))'
then
  echo "Mom Llama must not resolve speech packages without deliberate speech UX" >&2
  exit 1
fi

for native_package in \
  llama-native-cache \
  llama-native-engine \
  llama-native-host \
  llama-native-types
do
  if ! cargo tree --locked -p mom-llama-app -i "$native_package@0.1.0" >/dev/null
  then
    echo "the native package identity is missing or ambiguous: $native_package" >&2
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

echo "architecture ok: Mom owns product code and resolves Native, Attachment, and FTE from imported root paths"
