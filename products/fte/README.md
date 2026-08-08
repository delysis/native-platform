# Free Token Energy

Free Token Energy is a local-first desktop AI gateway built with Tauri 2, Rust,
SQLite, and a dependency-light webview. It presents several provider accounts
through one OpenAI-compatible loopback API and can select an available route
for each request.

The repository also contains reusable Rust services and a Rust-only Tauri 2
plugin. The model gateway is protocol-neutral
internally, supports native OpenAI Responses Items/events and Anthropic
Messages blocks, and can route to
an in-process llama.cpp host or hosted providers without changing caller
shape. See [Gateway Module](docs/GATEWAY_MODULE.md).

Local STT/TTS lives in the independent
[`delysis/speech-native-kit`](https://github.com/delysis/speech-native-kit)
repository. FTE may consume it through optional provider/protocol bridges, but
does not compile, install, or authorize speech by default. See
[Module and Repository Map](docs/MODULE_MAP.md).

The reusable gateway is substantially ahead of the historical desktop UI
runtime. [Robustness and unification audit](docs/ROBUSTNESS_AUDIT.md) states
the verified boundary and the remaining breaking migration work; the two paths
are not represented as unified until that migration is complete.

## What works

- OpenRouter, Groq, Anthropic, Google Gemini, Mistral, NVIDIA NIM, and Cerebras
- Native request and stream normalization for Anthropic Messages and Gemini
  `generateContent`
- OpenAI-compatible chat, native legacy text completions, responses, model
  listing, and SSE
- Atomic sliding-window request reservations that survive application restarts
- Measured local request totals, token usage, latency, and provider outcomes
- A desktop dashboard, provider setup, chat playground, activity log, and
  persistent proxy-port settings
- A transport-neutral backend boundary that distinguishes authenticated remote
  APIs from credentialless embedded or companion-process inference runtimes

The model catalog uses current provider model IDs. Published free-tier limits
are tracked locally where a provider documents them; account-specific or
unknown limits are shown as unknown and receive a neutral routing score. The
application does not invent benchmark, latency, usage, or quota data.

## Run locally

Requirements: a current Rust toolchain, Node.js 22 or later, and the
platform-specific Tauri prerequisites.

```sh
npm ci
npm test
npm run dev
```

The proxy starts on `127.0.0.1:1337` by default. Its port can be changed from
Settings without restarting the desktop application.

## API surface

- `GET /v1/models`
- `POST /v1/completions`
- `POST /v1/chat/completions`
- `POST /v1/responses`
- `GET /v1/responses/{id}`
- `DELETE /v1/responses/{id}`
- `POST /v1/responses/{id}/cancel`
- `POST /v1/messages`
- `POST /v1/messages/count_tokens`
- `POST /v1beta/models/{model}:generateContent`
- `POST /v1beta/models/{model}:streamGenerateContent`

Use `model: "auto"` to let the router choose among configured, capable models,
or use a public model ID returned by `/v1/models`.

### Legacy text completions

`POST /v1/completions` is a first-class prompt-based path. It preserves string,
string-array, token-array, and token-batch prompts and never converts them into
chat messages. Routing is restricted to catalog entries with a native text
completion transport and rejects unsupported prompt types or parameters instead
of silently dropping them.

Current native transports are:

- Cerebras `/v1/completions` for documented direct continuation with
  `gpt-oss-120b`
- OpenRouter `/api/v1/completions` is implemented in the provider adapter, but
  the dynamic `openrouter/free` catalog alias is deliberately excluded from raw
  routing because it cannot guarantee stable model or template semantics
- Anthropic's legacy `/v1/complete` transport is implemented and fixture-tested,
  but current Claude catalog models are deliberately excluded because Anthropic
  now directs integrations to Messages and does not document those models for
  the legacy endpoint
- Mistral `/v1/fim/completions` with `codestral-latest`, marked as FIM
  continuation

Groq, Gemini, hosted NVIDIA NIM, and `mistral-small-latest` are chat-only in the
catalog. They are never used as fallbacks for `/v1/completions`. Model objects
include an `x_free_token_energy` extension describing their supported surfaces
and prompt semantics.

## Routing

Eligible routes must have a configured key, support the requested model and
capabilities, and have local quota remaining. They are ranked with:

```text
0.35 × documented-limit headroom
+ 0.30 × measured evaluation score
+ 0.20 × declared capability breadth
+ 0.15 × observed latency
```

Unknown quota, evaluation, and latency inputs are neutral rather than
fabricated. Request counts are reserved atomically before dispatch; token usage
is recorded when it becomes available.

Only finite documented quota windows are reserved and persisted. Local or
otherwise unmetered backends still contribute measured request, token, and
latency observations without being assigned invented quota limits.

## Native llama.cpp integration

`fte-backend-llama` is a real credentialless local backend over the reusable
llama-native host. Chat, raw text Completion, exact token Completion,
streaming, cancellation, resident-model reuse, and fingerprinted prefix caches
run in process. There is no `llama-cli`, `llama-server`, subprocess, or
loopback hop between the router and local inference.

The real-GGUF integration test distinguishes actual in-process evidence from
fixtures and proves both cold checkpoint creation and a second-request stable
prefix hit. Release manifests pin the native kit by immutable Git revision.
For coordinated local development, copy
`.cargo/local-native-kit.toml.example` to `.cargo/local-native-kit.toml` and
pass `--config .cargo/local-native-kit.toml` to Cargo; that override is ignored
by Git.

## Speech composition

Local speech execution is owned by
[`speech-native-kit`](https://github.com/delysis/speech-native-kit). A product
may embed that Tauri plugin directly without importing FTE's hosted providers
or loopback server. Future OpenAI-compatible `/v1/audio/*` codecs and hosted
speech adapters belong in a thin optional FTE bridge that depends on speech
contracts; neither core owns the other.

## Privacy and security

There is no telemetry. The reusable loopback edge is disabled until explicitly
started, binds only loopback, validates Host/Origin, and requires an app-private
256-bit token. Hosted keys never cross that interface and are loaded through an
injected secret resolver. The older desktop proxy remains a separate migration
surface; read [SECURITY.md](SECURITY.md) before using valuable credentials.

## Development

```sh
npm run check
npm run test:rust
cargo check --all-targets --all-features --manifest-path src-tauri/Cargo.toml
```

Provider research notes and machine-readable policies are kept in
`research/provider-gateways/`. Local cloned reference repositories under that
directory are intentionally ignored rather than vendored.
