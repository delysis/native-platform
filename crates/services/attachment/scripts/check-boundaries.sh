#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

fail() {
  printf 'attachment boundary violation: %s\n' "$1" >&2
  exit 1
}

reject_matches() {
  local boundary_message=$1
  shift
  if "$@"; then
    fail "$boundary_message"
  else
    local scan_status=$?
    if (( scan_status != 1 )); then
      fail "the boundary scan did not complete ($1 exit $scan_status)"
    fi
  fi
}

core="$repo_root/crates"

reject_matches "core crates contain network, loopback, or Tauri authority" \
  rg -n '\b(axum|reqwest|ureq|hyper|tower_http|tauri)\b' \
  "$core" -g 'Cargo.toml' -g '*.rs'

# Integration tests may launch the package's own CLI as an external observer;
# shipped crate sources may not acquire subprocess authority.
reject_matches "core crates contain subprocess authority" \
  rg -n 'std::process::Command|tokio::process::Command|Command::new' \
  "$core" -g '*.rs' -g '!**/tests/**'

reject_matches "attachment core depends on a product, model, speech, or provider gateway" \
  rg -n 'llama[-_]native|speech[-_]native|free_token_energy|fte[-_]' \
  "$core" -g 'Cargo.toml' -g '*.rs'

reject_matches "a crate root does not forbid unsafe code" \
  rg --files-without-match '^#!\[forbid\(unsafe_code\)\]' \
  "$core" -g 'lib.rs' -g 'main.rs'

printf 'attachment-native-kit boundaries verified\n'
