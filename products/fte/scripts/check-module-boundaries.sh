#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
core_plugin="$repo_root/crates/tauri-plugin-free-token-energy"

fail() {
  printf 'module boundary violation: %s\n' "$1" >&2
  exit 1
}

if grep -ERn 'fte[-_]speech|speech[-_]native|speech_(status|plan|synthesize|transcribe|cancel)' \
  "$core_plugin/Cargo.toml" "$core_plugin/src" \
  "$core_plugin/permissions" "$repo_root/src-tauri/Cargo.toml" \
  "$repo_root/src-tauri/src" "$repo_root/src-tauri/capabilities"; then
  fail "the core Free Token Energy plugin contains speech dependencies, commands, or permissions"
fi

printf 'module boundaries verified: FTE contains no local speech runtime or authority\n'
