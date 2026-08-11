#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

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

if rg -n '^\[patch\.|(llama|attachment)-native-[a-z-]+\s*=\s*\{\s*path\s*=' --glob 'Cargo.toml' .
then
  echo "release manifests must use immutable native and attachment Git revisions" >&2
  exit 1
fi

if rg -n '(llama|attachment)-native-[a-z-]+\s*=\s*\{\s*path\s*=\s*"\.\.' --glob 'Cargo.toml' .
then
  echo "sibling native or attachment paths are forbidden" >&2
  exit 1
fi

cargo metadata --locked --format-version 1 >/dev/null

native_sources=$(rg -o 'source = "git\+https://github\.com/delysis/llama-native-kit[^\"]+"' Cargo.lock | sort -u | wc -l | tr -d ' ')
if [ "$native_sources" -ne 1 ]
then
  echo "the locked graph must contain exactly one llama-native-kit source" >&2
  exit 1
fi

if ! rg -q 'source = "git\+https://github\.com/delysis/llama-native-kit\?rev=2d69f086e922ed7bdfd6236baf5a1ad0ed568360#' Cargo.lock
then
  echo "the locked native-kit source does not match the reviewed boundary" >&2
  exit 1
fi

fte_sources=$(rg -o 'source = "git\+https://github\.com/delysis/free-token-energy[^\"]+"' Cargo.lock | sort -u | wc -l | tr -d ' ')
if [ "$fte_sources" -ne 1 ]
then
  echo "the locked graph must contain exactly one Free Token Energy source" >&2
  exit 1
fi

if ! rg -q 'source = "git\+https://github\.com/delysis/free-token-energy\?rev=d86edfef40771589af4cd8cd72907fa6d3ce6e96#' Cargo.lock
then
  echo "the locked Free Token Energy source does not match the reviewed boundary" >&2
  exit 1
fi

attachment_sources=$(rg -o 'source = "git\+https://github\.com/delysis/attachment-native-kit[^"]+"' Cargo.lock | sort -u | wc -l | tr -d ' ')
if [ "$attachment_sources" -ne 1 ]
then
  echo "the locked graph must contain exactly one attachment-native-kit source" >&2
  exit 1
fi

if ! rg -q 'source = "git\+https://github\.com/delysis/attachment-native-kit\?rev=472900732ded5bcfb5cc639c49b3a4f77feece27#' Cargo.lock
then
  echo "the locked attachment-native-kit source does not match the reviewed boundary" >&2
  exit 1
fi

if rg -n '^name = "(fte-speech-|speech-native-|tauri-plugin-(free-token-energy-speech|speech-native))' Cargo.lock
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

echo "architecture ok: Mom Llama owns product code; native-kit, attachment-native-kit, and FTE are pinned boundaries"
