#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

fail() {
  printf 'attachment boundary violation: %s\n' "$1" >&2
  exit 1
}

core="$repo_root/crates"

if rg -n '\b(axum|reqwest|ureq|hyper|tower_http|tauri)\b' \
  "$core" -g 'Cargo.toml' -g '*.rs'; then
  fail "core crates contain network, loopback, or Tauri authority"
fi

# Integration tests may launch the package's own CLI as an external observer;
# shipped crate sources may not acquire subprocess authority.
if rg -n 'std::process::Command|tokio::process::Command|Command::new' \
  "$core" -g '*.rs' -g '!**/tests/**'; then
  fail "core crates contain subprocess authority"
fi

if rg -n 'llama[-_]native|speech[-_]native|free_token_energy|fte[-_]' \
  "$core" -g 'Cargo.toml' -g '*.rs'; then
  fail "attachment core depends on a product, model, speech, or provider gateway"
fi

if rg --files-without-match '^#!\[forbid\(unsafe_code\)\]' \
  "$core" -g 'lib.rs' -g 'main.rs'; then
  fail "a crate root does not forbid unsafe code"
fi

printf 'attachment-native-kit boundaries verified\n'
