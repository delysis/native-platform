# Llama Native Kit

A reusable, local-first Rust integration for llama.cpp, with Mom Llama as its
reference Tauri application.

The runtime loads GGUF models directly in-process. It does not use
`llama-server`, `llama-cli`, localhost, HTTP, TCP or SSE for inference. Safe
Rust owns model scheduling, streaming, cancellation, encrypted persistence,
cache policy, chat-native Personas, consult groups and readiness evidence.

## Workspace

- `llama-native-types`: stable public DTOs.
- `llama-native-engine`: a model-owning worker around the pinned llama.cpp
  binding.
- `llama-native-cache`: fingerprint-bound memory, disk and model-state cache
  tiers.
- `llama-native-host`: application-owned resident-model registry, memory/slot
  budgeting, cancellation, lifecycle, and injected cache persistence.
- `command-evidence`: source- and runtime-fingerprint-bound readiness receipts.
- `mom-llama-runtime`: conversations, Skills, versioned Personas, `@mention`
  routing, encrypted storage and cache policy.
- `mom-llama-cli`: the complete machine-exercisable command surface.
- `apps/mom-llama`: Maud/Tauri application with a thin native interaction
  bridge.

`docs/PRODUCT.md` and the contract ledgers distinguish implemented native
behavior from web-specific or high-authority surfaces that are explicitly
superseded, deferred, or rejected by constraint.

[`docs/FREE_TOKEN_ENERGY_INTEGRATION.md`](docs/FREE_TOKEN_ENERGY_INTEGRATION.md)
documents the product-neutral linking boundary, explicit chat/raw/token prompt
semantics, model descriptors, cache identity, and the intentionally blocked FIM
surface.

The Mom Llama application pins the private Free Token Energy gateway by
immutable Git revision. Free Token Energy pins the published native runtime in
the same way; the workspace patch in the root manifest deliberately resolves
that native dependency back to this checkout so the app has one `NativeHost`
and one native type identity. CI uses repository-scoped read-only deploy keys,
not a personal token or an unversioned sibling path.

`docs/PERSONA_LIBRARY.md` documents the exact 14 built-in Persona templates.
Groups are user-configured in Settings and invoked from the ordinary composer;
there is no separate Consult dashboard and no application-seeded group pattern.

## Contracts and gates

- `contracts/commands.json`: CLI/Tauri/view mappings for every visible command.
- `contracts/effects.json`: filesystem, model, memory and bounded MCP authority.
- `contracts/upstream-parity.json`: behavior ledger pinned to the inspected
  llama.cpp UI revision. The current ledger has no `p0_required` rows: every
  upstream family is implemented, superseded with native behavior, or
  explicitly deferred/rejected with a reason.

Run the required local gates:

```sh
cargo fmt --all --check
cargo test --workspace --exclude mom-llama-app
cargo clippy --workspace --all-targets --exclude mom-llama-app -- -D warnings
cargo test -p mom-llama-app
cargo clippy -p mom-llama-app --all-targets -- -D warnings
node --check apps/mom-llama/ui/coop-hx.js
./scripts/check-architecture.sh
./scripts/check-contracts.sh
```

With a real local model, exercise encrypted native-state save and a fresh-process
restore:

```sh
MOM_LLAMA_MODEL_PATH=/path/to/model.gguf ./scripts/prove-native-cache-restart.sh
```

Prompt caching has one runtime setting with three modes. New installations use
`Automatic`: reusable Persona/Skill prefixes and per-conversation checkpoints
are enabled. `Prefixes only` keeps reusable Persona/Skill prompt packs but does
not persist conversation checkpoints. `Off` performs no cache lookup, creation,
promotion, or restore; existing entries are retained until explicitly cleared,
so turning caching back on is non-destructive. The CLI exposes the same choices
as `--kv-cache-policy automatic|prefixes-only|off`.

The operational mode is a runtime preference. Memory and disk ceilings,
fingerprint requirements, corruption handling, and fallback behavior are
compile-time safety policy. This gives normal users a small, safe control while
keeping resource and compatibility invariants out of the UI.

## Run Mom Llama Native Kit

Configure a local GGUF with `MOM_LLAMA_MODEL_PATH`. Application data is kept
under the standalone `llama-native-kit/mom-llama` data directory; override it
with `LLAMA_NATIVE_KIT_DATA_DIR` for an isolated smoke. macOS uses the Keychain
for the installation key and attempts resolution at most once during each app
process, including when access is denied.
See [`docs/SECURITY.md`](docs/SECURITY.md) for the exact threat model and cache
boundaries. Non-Keychain test hosts must provide a 32-byte key as
64 hexadecimal characters in `LLAMA_NATIVE_KIT_STORE_KEY_HEX`.

Build a real macOS app bundle:

```sh
cd apps/mom-llama/src-tauri
MACOSX_DEPLOYMENT_TARGET=13.0 cargo tauri build --debug --bundles app
```

Debug builds use the isolated `mom-llama-development` data directory and a
predictable local development key. They never contact macOS Keychain. This is
intentionally not secure storage; it preserves the production database shape
while making rapid rebuilds prompt-free. Release builds continue to use the
separate `mom-llama` directory and a Keychain-backed installation key. Set
`LLAMA_NATIVE_KIT_SECURE_STORAGE=1` to exercise that secure path in a debug
build.

The debug bundle is emitted at:

`target/debug/bundle/macos/Mom Llama Native Kit.app`
