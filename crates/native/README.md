# Llama Native Kit

Reusable, product-neutral Rust crates for loading GGUF models through llama.cpp
inside the caller's process.

This repository is deliberately **not an application**. Mom Llama was
history-preservingly extracted to the private
[`delysis/mom-llama`](https://github.com/delysis/mom-llama) repository. Routing,
hosted providers, compatibility APIs, loopback listeners, STT and TTS live in
[`delysis/free-token-energy`](https://github.com/delysis/free-token-energy).
The old Mom Llama experiment in capability-system-compiler is non-canonical.

The runtime does not use `llama-server`, `llama-cli`, localhost, HTTP, TCP or
SSE for inference. It owns only model loading, scheduling, tokenization,
streaming, cancellation and cache-safe native state.

## Raw generation families

`GenerationBatchRequest` is the product-neutral batch boundary for local raw
generation. Each ordered `GenerationCase` owns its exact `GenerationInput`,
sampler/seed, cancellation identity and optional sequence state. Completion
cases accept exact text or token IDs without a chat template; one case always
produces one ordered output. The engine detects token-exact shared prefixes
without changing prompt semantics.

Outputs preserve sampled token IDs and per-case cache accounting. Rich token
observations are optional and probability records carry an explicit
`raw_model`, `post_constraint` or `post_sampler` stage. Backends leave
unsupported observations absent. Exact inspected capabilities are published
alongside the legacy summary fields for a compatibility window.

## Workspace

- `llama-native-types`: stable public DTOs.
- `llama-native-engine`: a model-owning worker around the pinned llama.cpp
  binding.
- `llama-native-cache`: fingerprint-bound memory, disk and model-state cache
  tiers.
- `llama-native-host`: application-owned resident-model registry, memory/slot
  budgeting, cancellation, lifecycle, and injected cache persistence.
- `command-evidence`: source- and runtime-fingerprint-bound readiness receipts.

[`docs/FREE_TOKEN_ENERGY_INTEGRATION.md`](docs/FREE_TOKEN_ENERGY_INTEGRATION.md)
documents the typed adapter boundary. See [`docs/MODULE_BOUNDARIES.md`](docs/MODULE_BOUNDARIES.md)
for the authoritative repository and dependency map.

## Gates

Run the required local gates:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check-architecture.sh
```
