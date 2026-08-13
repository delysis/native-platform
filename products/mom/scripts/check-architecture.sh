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

if ! rg -q 'source = "git\+https://github\.com/delysis/llama-native-kit\?rev=f7a69316c64d857b99bd847dd44cd852fc5b4ca4#f7a69316c64d857b99bd847dd44cd852fc5b4ca4"' Cargo.lock
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

if ! rg -q 'source = "git\+https://github\.com/delysis/free-token-energy\?rev=1b4cc9c830cf5593e73b3ca9349ce9ac77d7bf5a#1b4cc9c830cf5593e73b3ca9349ce9ac77d7bf5a"' Cargo.lock
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

contract_rev=cbab33555ab9355a6ac453d659c55ec9e0666821
vertical_rev=fc24ffff08c52690390b4460f44617d5d9732563
contract_url=https://github.com/delysis/w1-platform-contracts
contract_tomls=$(rg -l 'w1-platform-contracts' . --glob 'Cargo.toml' --glob '!target/**')
if [ "$(printf '%s\n' "$contract_tomls" | sed '/^$/d' | wc -l | tr -d ' ')" -ne 1 ]; then
  echo "Wave 1 contract source must be declared once at the workspace boundary" >&2
  exit 1
fi
if [ "$(rg -F "git = \"$contract_url\", rev = \"$contract_rev\"" Cargo.toml | wc -l | tr -d ' ')" -ne 2 ]; then
  echo "Wave 1 lifecycle contract and testkit must use the accepted exact revision exactly twice" >&2
  exit 1
fi
if [ "$(rg -F "git = \"$contract_url\", rev = \"$vertical_rev\"" Cargo.toml | wc -l | tr -d ' ')" -ne 2 ]; then
  echo "Wave 1 vertical contract and fixture adapter must use the accepted exact revision exactly twice" >&2
  exit 1
fi
if rg -n 'w1-platform-contracts.*(branch|tag)[[:space:]]*=' . --glob 'Cargo.toml'; then
  echo "moving Wave 1 contract dependency found" >&2
  exit 1
fi
if [ "$(rg -F "source = \"git+$contract_url?rev=$contract_rev#$contract_rev\"" Cargo.lock | wc -l | tr -d ' ')" -ne 2 ]; then
  echo "Wave 1 lifecycle lock must contain exactly the contract and testkit packages at the accepted revision" >&2
  exit 1
fi
if [ "$(rg -F "source = \"git+$contract_url?rev=$vertical_rev#$vertical_rev\"" Cargo.lock | wc -l | tr -d ' ')" -ne 2 ]; then
  echo "Wave 1 vertical lock must contain exactly the contract and fixture packages at the accepted revision" >&2
  exit 1
fi
if rg 'w1-platform-contracts.*rev[[:space:]]*=' Cargo.toml | rg -v "$contract_rev|$vertical_rev"; then
  echo "unreviewed Wave 1 contract revision found" >&2
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
