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

Local STT/TTS lives in the shared
[Speech service](../../crates/services/speech).
FTE may consume it through optional provider/protocol bridges, but
does not compile, install, or authorize speech by default. See
[Module and Repository Map](docs/MODULE_MAP.md).

The desktop commands, Rust-only plugin, and optional loopback API now share one
application-owned `Gateway`. [Robustness and unification audit](docs/ROBUSTNESS_AUDIT.md)
records the verified boundary and the live acceptance work still required.

## What works

- OpenRouter, Groq, Anthropic, Google Gemini, Mistral, NVIDIA NIM, and Cerebras
- Native request and stream normalization for Anthropic Messages and Gemini
  `generateContent`
- OpenAI-compatible chat, native legacy text completions, responses, model
  listing, and SSE
- Measured local request totals, token usage, latency, and provider outcomes
- A desktop dashboard, provider setup, chat playground, and activity log
- A native file picker for selecting a local GGUF, with restart-safe local
  configuration and explicit missing-file status
- A transport-neutral backend boundary that distinguishes authenticated remote
  APIs from credentialless embedded or companion-process inference runtimes

The model catalog uses current provider model IDs. Published free-tier limits
are tracked locally where a provider documents them; account-specific or
unknown limits are shown as unknown and receive a neutral routing score. The
application does not invent benchmark, latency, usage, or quota data.

## Run locally

Use the pinned Rust and pnpm versions and Tauri prerequisites described in the
[workspace README](../../README.md). Run these commands from the repository root:

```sh
pnpm install --frozen-lockfile
pnpm --filter free-token-energy dev
```

The authenticated loopback API is disabled until explicitly started. By
default it binds an OS-assigned loopback port; callers may request a fixed port
through the plugin command and should read the returned address.

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
- OpenRouter's dynamic `openrouter/free` alias is deliberately excluded from raw
  completion routing because it cannot guarantee stable model or template
  semantics
- Current Claude catalog models are excluded because they do not have a
  supported prompt-completion contract
- Codestral FIM is not advertised. It may return only with a dedicated typed
  request, response, and streaming contract on the sole production route.

Groq, Gemini, hosted NVIDIA NIM, and `mistral-small-latest` are chat-only in the
catalog. They are never used as fallbacks for `/v1/completions`. Model objects
include an `x_free_token_energy` extension describing their supported surfaces
and prompt semantics.

## Routing

Eligible routes must satisfy privacy, model, capability, and backend-readiness
requirements. The Gateway uses only descriptor observations that actually
exist; missing quota, evaluation, and latency inputs remain unknown rather than
being fabricated. Request outcomes and usage are recorded in the bounded
nonsecret activity log.

## Native llama.cpp integration

`fte-backend-llama` is a real credentialless local backend over the reusable
llama-native host. Chat, raw text Completion, exact token Completion,
streaming, cancellation, resident-model reuse, and fingerprinted prefix caches
run in process. There is no `llama-cli`, `llama-server`, subprocess, or
loopback hop between the router and local inference.

In the desktop app, open **Providers** and choose a GGUF file. The sole
application-owned Gateway validates and registers the file, while SQLite keeps
only its non-secret canonical path and optional expected SHA-256 for startup
restoration. The webview receives the filename and readiness state, not the
full local path. A moved or deleted file is reported as needing attention and
is never presented as a usable route.

The real-GGUF integration test distinguishes actual in-process evidence from
fixtures and proves both cold checkpoint creation and a second-request stable
prefix hit. The native crates are workspace dependencies under `crates/native`;
the root lockfile pins the external llama.cpp wrapper by immutable Git revision.

## Speech composition

<!-- current-service-surface: speech -->
<!-- retired-edge-parent: 84f65a9c1313bc0e4218156507cedd9fd905903f -->

Local speech execution is currently owned by the transport-neutral
`crates/services/speech/crates/speech-native-host`; FTE does not currently
compose it and no generic
`crates/services/speech/crates/tauri-plugin-speech-native` crate is shipped.
The deleted plugin remains inspectable only at exact historical parent
[`84f65a9c1313bc0e4218156507cedd9fd905903f`](https://github.com/delysis/native-platform/tree/84f65a9c1313bc0e4218156507cedd9fd905903f/crates/services/speech/crates/tauri-plugin-speech-native).
Future OpenAI-compatible `/v1/audio/*` codecs or hosted adapters would require
a new, explicitly composed FTE bridge that depends on Speech contracts;
neither core owns the other today.

## Privacy and security

There is no telemetry. The loopback edge is disabled until explicitly
started, binds only loopback, validates Host/Origin, and requires an app-private
256-bit token. Hosted keys never cross that interface and are loaded through an
injected OS credential-store resolver. Fresh databases never create plaintext
credential storage. The database has an explicit application/schema identity;
unversioned, foreign, and legacy plaintext stores are rejected before schema
mutation and are never imported. Read
[workspace security policy](../../SECURITY.md) for credential modes and platform
limits. Private file storage and loopback-token creation refuse unsupported
platforms before writing protected state.

## Development

Follow [the workspace contribution guide](../../CONTRIBUTING.md). The FTE
desktop selection is `node scripts/ci/cargo-group.mjs test product-fte` (use
`gateway` for the reusable gateway crates); frontend-only validation
is `pnpm --filter free-token-energy test:frontend`. Full CI owns cross-platform
workspace qualification, browser interaction, and packaging.

Provider research notes and machine-readable policies are kept in
`research/provider-gateways/`. Local cloned reference repositories under that
directory are intentionally ignored rather than vendored.

## Request activity storage

Current request activity storage uses schema v2, with nullable `tokens_used`
when usage is unknown. Fresh and exact current stores are supported; prior,
unversioned, foreign, future, and altered schemas are rejected before schema or
journal mutation. Incompatible databases are preserved without automatic
conversion. Recorded token aggregates sum stored counts,
not a claim that every request reported usage. Both desktop and authenticated
loopback generation outcomes now enter the same metadata-only terminal observer.
