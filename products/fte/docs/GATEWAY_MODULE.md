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

HTTP model IDs are opaque, including any slashes, and can be used unchanged
from `/v1/models`. Only the documented `local-only`, `hosted-only`,
`prefer-local`, and `auto` names select routing profiles. Canonical Rust callers
can separately select an exact backend and model through `ModelSelector`.

Anthropic streaming counts and starts generation under one route admission
before emitting `message_start`. An opted-in pre-output fallback obtains a new
exact count on its own route; it never reuses the preceding route's count.
Cancellation and the request deadline cover both phases. If the serving route
cannot supply an exact count, the request fails with a typed capability error.
Non-streaming Messages responses likewise require exact authoritative input
and output usage.

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

## Hosted response and stream contract

Hosted HTTP responses are parsed before entering the canonical Gateway. Chat
function calls retain their call IDs, names, and complete JSON arguments;
streamed argument fragments assemble into the same authoritative call. Responses,
Anthropic, and Gemini tools use that same typed output. Invalid or truncated
terminal tool JSON fails explicitly; it is never replaced with an empty object.
Gateway-owned tool execution remains separate from returning a client tool call.

Alternative choices retain their group index through text progress and final
projection. `GatewayResponse.output_groups` connects each alternative to its
Items and stop reason. `OutputItemAdded.group_index` supplies the corresponding
progress identity. Backends with one implicit result can omit group metadata.
The public Chat codec still rejects requested `n` values other than one.

The canonical result distinguishes ordinary stop, an exact stop sequence,
output-token/context limits, client tool handoff, content filtering, and refusal.
Token/context-limited results retain their output and have `Incomplete` status;
Chat/Completion clients receive `length`, Responses clients receive
`response.incomplete`, and Anthropic clients retain the relevant stop reason.
`GatewayEvent::Completed` delivers an authoritative result, whose status may be
`Incomplete`; it does not imply an ordinary model stop. Refusals retain their
text and refusal classification. Unknown provider stop states and malformed
results fail explicitly instead of becoming success. A single Anthropic message
cannot represent alternative candidates.

OpenAI Chat/Completion SSE requires `[DONE]` and a finish reason for every
observed choice. Responses requires an explicit terminal response, Anthropic
requires its stop reason and `message_stop`, and Gemini requires a finish reason
for every observed candidate. Ordinary EOF after partial output is an upstream
failure. Cancellation remains cancellation, including while waiting for a frame
delimiter or token-count response.

The incremental SSE decoder accepts LF, CRLF, and CR, including line endings and
UTF-8 characters split between transport chunks. It preserves multiline `data`
and ignores comments. Invalid UTF-8 and unterminated final events are rejected
or discarded according to the strict framing contract; neither can manufacture
a successful terminal. Bounds are 1 MiB per normalized frame, 8 MiB of aggregate
decoded event data, 16 MiB of transport data, and 256 output entries/indices.
Authoritative output and successful ordinary JSON (including a JSON response to
a streaming request and token-count responses) are independently bounded to
8 MiB. Oversized text or arguments fail explicitly rather than being truncated.

Local HTTP fixtures exercise the actual hosted backend, routing, and public
codecs without provider credentials or paid requests. They cover tools,
interleaved alternatives, incomplete/refused outcomes, premature EOF, framing,
size limits, cancellation, route changes, and recounted fallback. These are
transport conformance checks, not live-provider acceptance or model measurements.
The framing and stop mappings follow the
[WHATWG SSE contract](https://html.spec.whatwg.org/multipage/server-sent-events.html),
[OpenAI Chat reference](https://platform.openai.com/docs/api-reference/chat),
[Anthropic stop reasons](https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons),
and [Gemini candidate contract](https://ai.google.dev/api/generate-content).

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
