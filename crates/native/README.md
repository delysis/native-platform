# Llama Native Kit

Reusable, product-neutral Rust crates for loading GGUF models through llama.cpp
inside the caller's process.

These product-neutral crates are part of the `delysis/native-platform`
monorepo, not a separate application or the active standalone repository.
Mom's retained chat implementation lives in `products/mom`; the canonical Loom
executable selects that chat shell or the normal Tauri document shell.
`products/fte` owns hosted routing and compatibility APIs. Shared Speech lives
in `crates/services/speech`; it is not owned by FTE. Mom calls Native directly
for local generation, and its application root composes Speech and Information.
The old standalone repositories and capability-system-compiler experiment are
historical, not the source of current product ownership.

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

## Saved prefix lifetime

Native sequence payloads use shared immutable storage, so branching and cache
hits do not copy opaque KV bytes. Memory lookup borrows already validated token
metadata; products still enforce owner leases and invalidation independently of
payload ownership.

A raw snapshot can accelerate disk spill only while its exporting worker retains
its bounded receipt. Persistent indexes also hold exact tokens and the small
model/context binding envelope. On worker replacement, shutdown, or receipt
eviction, stores reconstruct from that metadata and atomically compact the
expired spill without reading its raw bytes. Replay still checks the model and
context binding. A live receipt is only a storage hint: the native importer
independently verifies every byte and token against the worker's receipt before
calling the native state parser.

## Residual training

`NativeModelHandle::train_residual` learns frozen-model hidden-state contrast
directions and bounded layer gains in-process. It accepts exact-token training
and held-out pairs, executes on the existing owner worker with isolated contexts,
and supports cancellation and joined-owner verification. Gemma 4 is the initial
supported architecture. Output includes model/execution fingerprints, a hashed
request, fitted directions, an optimization trace, and held-out metrics.

This is contrast-subspace fitting, not autograd or weight fine-tuning. A successful
fit does not authorize controlled generation or establish generalization.
See [the algorithm, experiment card, and limits](docs/RESIDUAL_TRAINING.md).

## Workspace

- `llama-native-types`: stable public DTOs.
- `llama-native-engine`: a model-owning worker around the pinned llama.cpp
  binding.
- `llama-native-cache`: fingerprint-bound memory, disk and model-state cache
  tiers.
- `llama-native-host`: application-owned resident-model registry, memory/slot
  budgeting, cancellation, lifecycle, and injected cache persistence.

[`docs/FREE_TOKEN_ENERGY_INTEGRATION.md`](docs/FREE_TOKEN_ENERGY_INTEGRATION.md)
records the typed FTE adapter boundary. Imported documents may retain historical
standalone paths; the root workspace manifest, root architecture checks and
actual product composition roots determine current ownership. Native remains
usable without FTE, HTTP or a loopback listener.

## Gates

Run the required local gates:

```sh
# From the native-platform repository root:
cargo fmt --all --check
cargo test --offline --locked -p llama-native-types -p llama-native-cache -p llama-native-engine -p llama-native-host
cargo clippy --offline --locked -p llama-native-types -p llama-native-cache -p llama-native-engine -p llama-native-host --all-targets -- -D warnings
```
