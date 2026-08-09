# Free Token Energy Gateway Module

Free Token Energy now has a reusable, Rust-first gateway independent of its
desktop dashboard. A Tauri application can embed the gateway directly, add an
in-process llama.cpp host, add hosted providers, and optionally expose the same
service through an authenticated loopback listener.

## Crates

- `fte-types`: canonical requests, typed Items/events, usage, errors, routing,
  storage, deadline, tool, and cache policy.
- `fte-protocols`: strict OpenAI Completions, Chat Completions, Responses, and
  Anthropic Messages/count-token codecs. Unknown fields fail before routing.
- `fte-router`: privacy/capability gates, response affinity, route scoring,
  bounded backend admission, full-lifecycle deadlines, explicit pre-output
  fallback, and setup-failure circuit breakers.
- `fte-providers`: protocol-native OpenAI Responses, Anthropic Messages,
  Gemini GenerateContent, and explicitly compatible OpenAI provider adapters.
- `fte-store`: additive SQLite Responses state and injected secret boundary.
- `fte-backend-llama`: the only bridge between the gateway and the versioned
  llama-native host. Local execution is in-process and credentialless.
- `fte-loopback`: authenticated Axum REST/SSE edge, disabled until started.
- `tauri-plugin-free-token-energy`: Rust-only text/model gateway commands,
  loopback lifecycle, and managed state. It contains no speech dependency or
  permission.

Speech is an independently versioned service in
[`delysis/speech-native-kit`](https://github.com/delysis/speech-native-kit).
See [Module and Repository Map](MODULE_MAP.md).

The native kit does not depend on Free Token Energy. Product state remains in
the embedding application.

The native host passed to the plugin is explicitly **borrowed**. Gateway
shutdown closes FTE admission, cancels every FTE request, and waits for model
acquisition, tokenization, cache, provider, bridge, and token-count work to
finish. It never unloads or closes the application-owned host. On Tauri exit,
the plugin drain runs before the embedding application's `App::run` callback;
only that application callback may perform and retain the native host's final
joined process-exit shutdown fact.

## Embedding

```rust,ignore
tauri::Builder::default().plugin(
    tauri_plugin_free_token_energy::Builder::new()
        .with_store(response_store)
        .with_secret_resolver(secret_resolver)
        .with_native_host(native_host)?
        .register_native_model(model_profile)?
        .with_default_loopback()
        .build(),
)
```

`with_default_loopback` makes a hardened configuration available; it does not
open a port. A caller must explicitly invoke `loopback_start`. Mom Llama
registers only its product-owned native backend, so its route set remains
local-only even when no model can load.

Loopback start, stop, and token rotation participate in the same cleanup
coordinator as generation. Exit first closes lifecycle admission, waits for
any in-flight listener ownership transfer, quiesces the gateway and listener
concurrently, then bounds Axum's graceful connection wait. A stalled client
cannot retain a listener or native request indefinitely during process exit.

## Loopback

When explicitly enabled, the module exposes:

- `GET /healthz`
- `GET /v1/models`
- `POST /v1/completions`
- `POST /v1/chat/completions`
- `POST /v1/responses`
- `GET|DELETE /v1/responses/{id}`
- `POST /v1/responses/{id}/cancel`
- `POST /v1/messages`
- `POST /v1/messages/count_tokens`

It binds only IPv4/IPv6 loopback, rejects untrusted Host and Origin values,
requires a random 256-bit installation token, bounds bodies/headers/active
requests/stream lifetimes, and never exposes hosted credentials. The local
token lives in an app-private file so ordinary SDK clients do not cause
Keychain prompts. Provider secrets remain behind an injected resolver.

Anthropic streaming obtains an exact token count from the selected route
before emitting `message_start`. If that route cannot supply an exact count,
the request fails with a typed capability error instead of fabricating zero
usage. Non-streaming Messages responses likewise require exact authoritative
input and output usage.

Stored Responses retain exact backend/model affinity across process restarts.
`previous_response_id` restores that affinity from SQLite before routing; an
unavailable original route fails instead of switching provider or model.

Request deadlines are enforced by the reusable `GatewayTicket`, not only by
the HTTP edge. The total budget bounds queue admission, model load/provider
connect, first output, idle stream time, and authoritative completion. Timeout
paths cancel the selected backend and expose exactly one typed terminal event.
Profile requests may opt into `retry_before_output`; exact routes, stored
response continuations, gateway-owned tool requests, non-retryable failures,
and any request that has already received a ticket are never rerouted. Three
consecutive retryable setup failures open that backend's circuit for 30
seconds, without weakening privacy or capability filtering.

## Local execution and cache hierarchy

The llama adapter never shells out and never uses HTTP. Expensive model load,
tokenization, and prefix state work runs on blocking workers while llama.cpp
objects remain on their owner threads. Backend admission is bounded and held
until the authoritative backend result resolves, even when a consumer drops or
a deadline wrapper returns earlier. Dropping a consumer cancels only that
request. A reserved event-channel permit guarantees one terminal event without
allowing a full ordinary-event queue to pin cancellation or shutdown.

Cache precedence is:

```text
request > named profile > persisted runtime settings > embedding defaults
```

The native tiers are resident sequence state, a byte-bounded memory LRU, an
optional persistent prefix store, and caller-owned stable prefix packs.
`StablePrefix` requests must provide an owner namespace, owner version, and an
exact count of leading canonical chat Items. The adapter renders and tokenizes
that prefix before generation, verifies it is a strict token prefix of the
actual request, and excludes every later host/request Item. It never snapshots
generated answer tokens into a reusable prompt prefix.

All cache entries bind the exact model/build/binding/tokenizer/template/device/
context/batch/sequence/KV-layout fingerprint plus token IDs and caller version.
A mismatch is a miss followed by normal generation. Required caching fails
closed. Provider-native cache controls remain separate and are preserved only
for providers that advertise them.

## Development and release dependency boundary

`fte-backend-llama` is the sole cross-repository dependency. The committed
manifests pin `llama-native-kit` by immutable Git revision. A developer who is
changing both repositories may copy `.cargo/local-native-kit.toml.example` to
`.cargo/local-native-kit.toml` and pass `--config .cargo/local-native-kit.toml`
to Cargo. The local override is ignored by Git and is never a release input.

## Verification

The deterministic workspace suite covers strict parsing, protocol event order,
privacy gates, route affinity, storage, loopback security, bounded admission,
consumer cancellation, queue/startup/first-output/idle/total deadlines,
pre-output retry pinning, circuit breaking, bounded-channel backpressure, and
cache-policy validation. It also covers concurrent/idempotent gateway cleanup,
borrowed-host preservation, dropped blocking futures, configuration rebinds,
full-channel terminal delivery, listener lifecycle races, and forced listener
closure for a non-reading SSE client. An ignored real-GGUF adapter test proves
cold prefix creation, second-request restoration, raw Completion input, one
resident model across those requests, explicit adapter drain, and final joined
host shutdown using `MOM_LLAMA_MODEL_PATH`.
