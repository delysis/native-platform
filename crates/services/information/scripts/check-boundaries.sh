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

zim_crate="$repo_dir/crates/information-native-backend-zim"
if [ -d "$zim_crate" ]; then
    if rg -n '^(xz2|memmap|memmap2|libzim)[[:space:]]*=' "$zim_crate/Cargo.toml"; then
        echo "forbidden mmap, FFI, or xz2 dependency found in OpenZIM backend" >&2
        exit 1
    fi
    if rg -n '\b(reqwest|std::net|std::process|Command::new)\b' "$zim_crate/src"; then
        echo "network or sidecar authority found in OpenZIM backend" >&2
        exit 1
    fi
fi

overture_crate="$repo_dir/crates/information-native-backend-overture"
if [ -d "$overture_crate" ]; then
    if rg -n '\b(reqwest|rusqlite|tauri|std::net|std::process|Command::new)\b' \
        "$overture_crate/src"; then
        echo "network, process, SQLite, or renderer authority found in Overture backend" >&2
        exit 1
    fi
fi

attachment_bridge="$repo_dir/crates/information-native-attachment-bridge"
if [ -d "$attachment_bridge" ]; then
    if rg -n '\b(reqwest|rusqlite|tauri|std::fs|std::net|std::path|std::process|Command::new)\b' \
        "$attachment_bridge/src"; then
        echo "ambient path, network, process, SQLite, or renderer authority found in Attachment bridge" >&2
        exit 1
    fi
fi

if rg -n '(^|[^[:alnum:]_])unsafe([[:space:]]|\{|fn|trait|impl)' \
    "$repo_dir/crates" --glob '*.rs' --glob '!**/target/**'; then
    echo "unsafe Rust found in workspace" >&2
    exit 1
fi

echo "information-native authority boundaries passed"
