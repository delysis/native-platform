#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
core_plugin="$repo_root/crates/tauri-plugin-free-token-energy"
speech_plugin="$repo_root/crates/tauri-plugin-fte-speech"

fail() {
  printf 'module boundary violation: %s\n' "$1" >&2
  exit 1
}

if rg -n 'fte[-_]speech|speech_(status|plan|synthesize|transcribe|cancel)' \
  "$core_plugin/Cargo.toml" "$core_plugin/src" "$core_plugin/permissions"; then
  fail "the core Free Token Energy plugin contains speech dependencies, commands, or permissions"
fi

if rg -n 'fte[-_](types|router|store|loopback|protocols|providers|backend[-_]llama)|tauri_plugin_free_token_energy::' \
  "$speech_plugin/Cargo.toml" "$speech_plugin/src"; then
  fail "the speech plugin depends on the text gateway"
fi

rg -q '^name = "tauri-plugin-free-token-energy-speech"$' \
  "$speech_plugin/Cargo.toml" \
  || fail "the speech package name no longer yields the free-token-energy-speech permission namespace"

rg -q 'PluginBuilder::new\("free-token-energy-speech"\)' \
  "$speech_plugin/src/lib.rs" \
  || fail "the speech Tauri runtime namespace changed"

rg -q '"free-token-energy-speech:default"' \
  "$repo_root/src-tauri/capabilities/default.json" \
  || fail "the desktop app does not explicitly authorize the speech plugin"

rg -q 'tauri_plugin_fte_speech::Builder::new' \
  "$repo_root/src-tauri/src/lib.rs" \
  || fail "the desktop app does not explicitly install the speech plugin"

printf 'module boundaries verified: text gateway and speech Tauri plugins are independent\n'
