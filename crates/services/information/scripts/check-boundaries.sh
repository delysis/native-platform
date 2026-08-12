#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

pure_crates="
$repo_dir/crates/information-native-types
$repo_dir/crates/information-native-catalog
$repo_dir/crates/information-native-retrieval
"

for crate_dir in $pure_crates; do
    if [ ! -d "$crate_dir" ]; then
        continue
    fi
    if rg -n '\b(reqwest|rusqlite|tauri|tokio|std::fs|std::net|std::process)\b' "$crate_dir/src"; then
        echo "ambient-authority dependency found in pure crate: $crate_dir" >&2
        exit 1
    fi
done

if [ -d "$repo_dir/crates/information-native-store/src" ] \
    && rg -n '\b(reqwest|std::net|TcpStream|UdpSocket)\b' \
        "$repo_dir/crates/information-native-store/src"; then
    echo "network authority found outside information-native-acquire" >&2
    exit 1
fi

if rg -n '(^|[^[:alnum:]_])unsafe([[:space:]]|\{|fn|trait|impl)' \
    "$repo_dir/crates" --glob '*.rs' --glob '!**/target/**'; then
    echo "unsafe Rust found in workspace" >&2
    exit 1
fi

contract_rev=da22fa893ac183c5d9df972a7e67215c0d92b383
contract_url=https://github.com/delysis/w1-platform-contracts
contract_tomls=$(rg -l 'w1-platform-contracts' "$repo_dir" --glob 'Cargo.toml' --glob '!target/**')
if [ "$(printf '%s\n' "$contract_tomls" | sed '/^$/d' | wc -l | tr -d ' ')" -ne 1 ]; then
    echo "Wave 1 contract source must be declared once at the workspace boundary" >&2
    exit 1
fi
if ! rg -F "git = \"$contract_url\", rev = \"$contract_rev\"" "$repo_dir/Cargo.toml" >/dev/null; then
    echo "Wave 1 contract dependency must use the accepted exact revision" >&2
    exit 1
fi
if rg -n 'w1-platform-contracts.*(branch|tag)[[:space:]]*=' "$repo_dir" --glob 'Cargo.toml'; then
    echo "moving Wave 1 contract dependency found" >&2
    exit 1
fi

echo "information-native authority boundaries passed"
