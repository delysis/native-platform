# Mom Llama

Mom Llama is the canonical native, local-first chat product. This repository
owns its Rust product runtime, CLI, command/effect contracts, evidence receipts
and Tauri/Maud interface.

It does **not** contain a copied llama.cpp engine or a generic provider gateway:

- [`delysis/llama-native-kit`](https://github.com/delysis/llama-native-kit)
  owns the in-process GGUF runtime.
- [`delysis/free-token-energy`](https://github.com/delysis/free-token-energy)
  owns protocol routing, hosted providers and optional loopback compatibility.
- [`delysis/speech-native-kit`](https://github.com/delysis/speech-native-kit)
  owns local STT/TTS contracts, routing, backends and optional Tauri IPC.
- [`delysis/attachment-native-kit`](https://github.com/delysis/attachment-native-kit)
  owns content-first recursive inspection, canonical attachment artifacts,
  provenance and capability-aware media/transform planning.
- [`delysis/capability-system-compiler`](https://github.com/delysis/capability-system-compiler)
  owns Loom compilation/specification and may test this CLI as a black box; it
  does not own another Mom Llama implementation.

See [`docs/MODULE_BOUNDARIES.md`](docs/MODULE_BOUNDARIES.md) for the complete
dependency graph and the exact present status of speech.

## Workspace

- `crates/mom-llama-runtime`: conversations, Skills, editable/versioned
  Personas, `@mention` dispatch, attachment lifecycle, storage, tools and
  product cache policy.
- `crates/mom-llama-cli`: the complete machine-exercisable product boundary.
- `apps/mom-llama`: the thin Maud/Tauri application.
- `contracts`: visible command, effect, settings and upstream-parity ledgers.
- `receipts`: preserved historical product evidence. A receipt counts as
  current proof only when it is explicitly source-bound; older path/date-only
  receipts remain informative.

Native-kit, attachment-native-kit and Free Token Energy are exact Git-revision
dependencies. Release manifests do not use sibling paths or patches.

## Gates

```sh
cargo fmt --all --check
cargo test --workspace --exclude mom-llama-app
cargo clippy --workspace --all-targets --exclude mom-llama-app -- -D warnings
cargo test -p mom-llama-app
cargo clippy -p mom-llama-app --all-targets -- -D warnings
node --check apps/mom-llama/ui/coop-hx.js
./scripts/check-architecture.sh
./scripts/check-contracts.sh
./scripts/check-persona-product-ux.sh
```

## Run the app

Set `MOM_LLAMA_MODEL_PATH` to a GGUF or select one in Settings, then:

```sh
cd apps/mom-llama/src-tauri
cargo tauri dev
```

The extraction deliberately retains the existing Tauri identifier, data paths,
environment variables and Keychain service. That preserves current local data
and avoids new credential prompts. Renaming those compatibility identifiers is
a separate additive migration, not part of repository cleanup.

Debug builds use the prompt-free development store unless
`LLAMA_NATIVE_KIT_SECURE_STORAGE=1` is set. Release builds retain the existing
Keychain-backed store.

Prompt caching remains a product runtime preference: `automatic` (conversation
checkpoints plus stable Persona/Skill prefixes), `prefixes-only`, or `off`.
Compatibility fingerprints and safety ceilings are enforced by native-kit.
