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

if rg -n -F 'github.com/delysis/w1-platform-contracts' "$repo_dir" \
    --glob 'Cargo.toml' --glob 'Cargo.lock' --glob '!target/**'; then
    echo "external Wave 1 contract source remains in Information" >&2
    exit 1
fi

echo "information-native authority boundaries passed"
