# Free Token Energy integration boundary

Free Token Energy owns routing, backend selection, HTTP compatibility, and
multi-backend policy. `llama-native-kit` supplies an in-process local llama.cpp
data plane through four product-neutral crates:

- `llama-native-types`: prompt, sampling, stream, output, descriptor, and error
  contracts;
- `llama-native-engine`: one loaded model and llama.cpp context on one owner
  worker thread;
- `llama-native-cache`: storage-neutral, fingerprint-bound prefix selection;
- `llama-native-host`: an application-owned registry with memory/slot bounds,
  lifecycle, cancellation, stream bridging, and injected cache dependencies.

## Prompt contract

`GenerationRequest.input` is mandatory and uses `GenerationInput`:

- `Chat { messages, template }` applies only the selected chat template.
- `Completion { prompts }` never applies a chat template. Text bytes are not
  trimmed, prefixed, or otherwise rewritten. The currently pinned safe binding
  always recognizes model special-token text, so the exposed text policies are
  explicitly `no_bos_parse_special` and `add_bos_parse_special`; a plaintext
  special-token mode is not advertised. `Tokens` are validated against the
  loaded vocabulary and consumed directly as llama token IDs.
- `FillInMiddle { prefix, suffix }` is a typed request form, but the current
  engine returns `unsupported_prompt_form`. FIM must not be advertised until
  the inspected GGUF supplies a verified model-specific prefix/suffix/middle
  token policy and real conformance tests pass.

Completion batches preserve their zero-based submitted order in
`input_index`, use stable `completion-{index}` branch IDs, and retain monotonic
per-input event indexes. Cancellation may target one branch without cancelling
its peers.

`NativeModelHandle::prepare_input` exposes the exact source hash, token policy,
and token IDs used by the loaded model. This is intended for conformance and
cache-key construction, not for routers to reproduce tokenization themselves.

## Discovery and identity

Every ready resident status contains a `NativeModelDescriptor`. Its
`stable_model_id` is based on GGUF content SHA-256 rather than a filesystem
path. The descriptor reports inspected name, architecture, parameter count,
context, sequence bound, backend, prompt forms, chat-template availability,
multimodal readiness, streaming/cancellation, and only sampler parameters the
engine currently implements.

The path-bearing `ModelFingerprint` remains an application diagnostic. Routers
should use the descriptor's path-independent stable identity.

## Cache contract

`CacheFingerprint` includes prompt form and token policy in addition to model,
binding/build, tokenizer, chat template, projector, context, batch, sequence,
device, RoPE, and KV-layout identity. Every entry also authenticates the exact
cached token IDs and their SHA-256. A mismatch returns no hit; it never performs
best-effort restore.

Persistent cache encryption and storage are injected through
`PrefixCacheStore`. The host itself does not choose a database, key manager, or
product namespace.

## Linking and optional isolation

The supported integration is library-first: construct and retain one
`NativeHost`, then submit typed requests. A future IPC adapter, if needed for
crash isolation, belongs in a separate crate and must preserve these DTOs and
error semantics. The inference crates contain no HTTP/TCP client, loopback
server, executable discovery, or subprocess fallback.
