# Free Token Energy integration contract for llama-native-kit

## Goal

Make `llama-native-kit` the reusable local-inference data plane for Free Token
Energy, Mom Llama, and future native applications. Free Token Energy will own
multi-backend routing and HTTP compatibility. Mom Llama will remain a product
layer rather than a dependency of the gateway.

The gateway side of this boundary is implemented in
`src-tauri/src/backend.rs`. A local adapter can declare itself credentialless,
report runtime readiness independently from configuration, register inspected
model routes at startup, and implement chat and native text-completion methods.

## Required llama-native-kit work

### 1. Make prompt semantics explicit

`llama-native-types::GenerationRequest` currently contains only chat messages,
and `llama-native-engine` renders them with the GGUF chat template. Add a
first-class input enum so raw completion can never accidentally pass through a
chat template:

```rust
pub enum GenerationInput {
    Chat {
        messages: Vec<ChatMessage>,
        template: ChatTemplatePolicy,
    },
    Completion {
        prompts: Vec<CompletionPrompt>,
    },
    FillInMiddle {
        prefix: String,
        suffix: String,
    },
}

pub enum CompletionPrompt {
    Text(String),
    Tokens(Vec<i32>),
}
```

Requirements:

- Preserve raw text byte-for-byte, including leading and trailing whitespace.
- Tokenize `CompletionPrompt::Text` directly with the selected model tokenizer.
- Decode `CompletionPrompt::Tokens` directly after validating token IDs.
- Do not call `chat_template`, `apply_chat_template`, or add an assistant turn
  for completion input.
- Keep FIM distinct from direct continuation. Do not emulate FIM by inventing a
  chat conversation.
- Support batched prompts either natively or through bounded host scheduling;
  preserve stable choice indexes.
- Reject unsupported parameters explicitly. Never discard an option and then
  claim compatibility.

Initially required sampling fields are `max_tokens`, `temperature`, `top_p`,
`seed`, `stop`, and streaming. Advertise `echo`, `logprobs`, multiple choices,
and grammar only after they have fixture and real-engine coverage.

### 2. Extract a reusable host from mom-llama-runtime

Create a product-neutral `llama-native-host` crate around
`llama-native-engine` and `llama-native-cache`. Move reusable parts of the
resident model registry, memory budgeting, model lifecycle, cancellation,
stream bridging, and cache orchestration into an owned host instance.

The reusable host must not:

- depend on `mom-llama-runtime`;
- read `MOM_LLAMA_*` environment variables;
- choose an application data directory or keychain namespace;
- own conversations, personas, consults, Skills, MCP policy, or UI state;
- use network access or subprocesses for ordinary inference.

Inject paths, storage, clock, and product namespace from the application edge.
Avoid a process-global resident registry: two app contexts and isolated tests
must be able to construct independent hosts.

A suitable public surface is conceptually:

```rust
pub struct NativeHost { /* owned registry, scheduler, cache */ }

impl NativeHost {
    pub fn models(&self) -> Vec<NativeModelDescriptor>;
    pub fn readiness(&self, model: &ModelId) -> NativeReadiness;
    pub fn load(&self, config: NativeModelConfig) -> Result<ResidentModelStatus, NativeError>;
    pub fn generate(&self, request: GenerationRequest) -> Result<GenerationTicket, NativeError>;
    pub fn cancel(&self, request_id: &str, branch_id: Option<&str>) -> usize;
    pub fn unload(&self, model: &ModelId) -> Result<(), NativeError>;
}
```

The engine may keep its current worker-thread and crossbeam design. The host or
the Free Token Energy adapter can bridge bounded engine events into an async
stream; llama.cpp objects must remain on their owning worker.

### 3. Publish truthful model capabilities

Expose an inspected model descriptor with a stable, path-independent identity:

- model and tokenizer fingerprints;
- display name and architecture;
- chat-template availability;
- supported operations: chat, direct completion, FIM, vision;
- supported prompt forms and generation parameters;
- context and resident-sequence limits;
- readiness state: not configured, loading, ready, or unavailable.

Do not infer that an instruction-tuned model is completion-incompatible merely
because it has a chat template. Direct token continuation is an engine
capability. Conversely, do not claim chat support when no valid template is
available unless the caller supplies an explicit, validated template policy.

Free Token Energy will translate this descriptor into catalog entries and call
`Router::add_model_routes`. Historical `provider_id` field names remain on the
gateway wire and database surfaces for compatibility; the value for this
adapter should be stable, such as `llama-native`.

### 4. Bind cache entries to prompt mode

Raw completions and rendered chats must never share a cache entry solely
because their visible text happens to match. Extend the cache identity with:

- generation-input kind;
- exact prompt token IDs and prefix hash;
- BOS/EOS and special-token policy;
- model, tokenizer, runtime, rope, adapter, multimodal, and KV-layout
  fingerprints already tracked by the kit.

A mismatch must invalidate reuse and fall back to ordinary generation. Cache
reuse remains an optimization, not a correctness requirement.

### 5. Keep deployment library-first

The first integration should link `llama-native-host` into the Tauri process.
Do not make Free Token Energy spawn `llama-server` or translate local inference
through HTTP internally.

An optional companion service can be added later for sharing one resident model
between multiple desktop apps. Put IPC authority in a separate edge crate and
use the same typed host contract over Unix-domain sockets or named pipes. Keep
the inference crates themselves process-free and network-free.

## Error and stream contract

Use typed, bounded errors with stable codes for at least:

- model missing or invalid;
- model loading, unavailable, or memory-budget blocked;
- unsupported operation, prompt form, or parameter;
- context overflow;
- cancellation;
- worker stopped;
- incompatible cache state;
- internal failure.

Every generation stream needs stable request and choice/branch IDs, monotonic
event indexes, text deltas, one terminal state, a finish reason, and final usage
when measured. A `started` event must not claim that the real engine ran until
model loading and invocation actually occurred.

## Acceptance tests before handoff

Add a backend conformance suite that exercises the real public host API:

1. Raw text completion preserves whitespace and bypasses chat templating.
2. Raw token input reaches decoding without text re-tokenization.
3. Chat input still uses the selected template exactly once.
4. Batched prompts retain input and choice indexes.
5. Streaming concatenates to the non-streaming result under deterministic
   sampling.
6. Cancellation produces one terminal cancelled state.
7. Unsupported options fail before engine invocation.
8. A raw-prefix cache survives restart and is rejected after any fingerprint or
   prompt-mode change.
9. CPU-only operation uses an actually empty accelerator device list.
10. A real GGUF smoke records `real_engine_invoked: true` and
    `fake_fixture: false`.

Retain the existing architecture, contract, clippy, and real-cache-restart
gates. Tag a compatible `llama-native-kit` revision once these tests pass; Free
Token Energy should consume versioned crates rather than copy source or depend
on `mom-llama-runtime`.

## Expected Free Token Energy adapter

Once the host contract is available, the gateway maintainer will add a small
`LlamaNativeBackend` that:

- returns `BackendKind::LocalEmbedded`;
- returns `CredentialRequirement::NotRequired`;
- maps native readiness into `BackendReadiness`;
- translates chat only into native chat input;
- translates `/v1/completions` only into native completion input;
- maps native events, finish reasons, usage, and errors without fabricated
  fields;
- registers only explicitly selected or inspected local models;
- uses neutral quota headroom and measured latency/evaluation data under the
  existing routing formula.

That adapter should be the only cross-repository translation layer.
