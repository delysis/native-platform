# Free Token Energy — contributor guidance

## Product direction

Build a high-performance, local-first Rust gateway that makes multiple
provider free tiers usable through one predictable API. Favor reusable
provider, routing, streaming, and persistence components.

## Non-negotiable principles

- Privacy first: no telemetry; keys and operational metadata remain local.
- No fake data: never seed or display invented usage, benchmarks, latency,
  health, or quotas. Use measured observations, documented limits, or an
  explicit unknown state.
- One source of truth: model IDs, capabilities, policies, and quota limits live
  in the model catalog.
- Compatibility without ambiguity: preserve public model aliases while
  translating provider-specific requests, streams, responses, and errors.
- Safe local service: bind the proxy only to loopback, bound inputs and error
  bodies, and avoid panics on recoverable runtime failures.

## Current architecture

- Reusable gateway workspace under `crates/`; canonical contracts live only in
  `fte-types`, while public protocol translation stays in `fte-protocols`
- `fte-router` owns privacy/capability gates, affinity, admission, and routing;
  providers and native adapters never reroute themselves
- `fte-backend-llama` is the sole native-kit bridge and may contain no network,
  process, shell, or executable-discovery authority
- `tauri-plugin-free-token-energy` is Rust-only and text/model-only; the
  webview receives typed IPC and never owns credentials, routing state, or
  model state
- STT/TTS is owned by the independently versioned `speech-native-kit`. FTE may
  add an optional hosted-provider or `/v1/audio/*` bridge, but the core plugin
  and desktop must not compile, install, or authorize local speech by default
- Tauri 2 desktop shell with a vanilla HTML/CSS/JavaScript webview
- one `GatewayRuntimeOwner` in `src-tauri/src/gateway_runtime.rs` composes the
  reusable Gateway for desktop commands and the authenticated loopback plugin
- SQLite in `src-tauri/src/db.rs` stores only non-secret profile, bounded
  request-log state, and the selected local-model path plus optional expected
  digest; provider credentials live in the OS credential store
- `src-tauri/src/credential_migration.rs` is the sole compatibility-window
  reader for a pre-existing legacy plaintext credential table and retires it
  only after exact OS-store readback
- model and provider presentation metadata lives in `src-tauri/src/catalog.rs`;
  protocol codecs, routing, hosted adapters, loopback, and native inference
  remain in their respective reusable workspace crates

Implemented providers are OpenRouter, Groq, Anthropic, Google Gemini, Mistral,
NVIDIA NIM, and Cerebras.

## Routing

Routes are filtered by requested model, backend readiness, any required
credentials, declared capabilities, and locally tracked finite quota. Ranking
weights are:

```text
0.35 headroom + 0.30 evaluation + 0.20 capability + 0.15 latency
```

Only documented quota limits and measured evaluation/latency values may affect
their respective score. Missing observations use a neutral value.

## Required verification

Before committing:

```sh
npm test
cargo fmt --all --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
./scripts/check-module-boundaries.sh
```

For native cache or adapter changes, also run the ignored real-GGUF proof with
`MOM_LLAMA_MODEL_PATH`. A cache metadata round-trip is not sufficient: the
proof must show a cold checkpoint, a later hit, and real in-process inference.

When changing provider transforms or streams, add fixture-style regression
tests. When changing SQLite schema or migrations, add a reopen/migration test.
Never commit the cloned repositories beneath `research/provider-gateways/`.

Local inference backends must not require placeholder credentials, masquerade
as chat for raw completion, or receive invented quota limits. Register only
explicitly selected or inspected local models, and keep product-specific state
outside the reusable backend adapter.

## Remaining roadmap

1. Live-provider end-to-end tests with user-owned credentials
2. Disposable real-OS credential migration/readback acceptance
3. Real evaluation-result ingestion and display
4. Usage visualizations sourced from request logs
5. Additional explicitly contracted transports such as Ollama and Bedrock
