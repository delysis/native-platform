# Product contract

## Reusable runtime

Provide an elegant, portable and integrated llama.cpp runtime for native Rust
applications:

- direct in-process model loading;
- explicit chat, raw-text completion, and exact-token completion semantics;
- single and multi-sequence generation;
- independent cancellation;
- resident model budgeting;
- multimodal inputs through native llama.cpp APIs;
- three managed, bounded, fingerprint-bound prefix-cache tiers plus live
  resident llama.cpp sequence state;
- encrypted transactional state;
- typed CLI and Tauri adapters;
- evidence-gated readiness.

The reusable boundary is the owned `llama-native-host` object. It owns model
slots, memory budgeting, lifecycle, cancellation, stream tickets, and the
bounded in-memory prefix tier. It accepts injected clock and persistent-cache
interfaces and has no Mom Llama settings, environment lookup, Keychain,
networking, subprocess, or process-global model registry.

Prompt modes are never inferred. Chat input is rendered through an explicit
model-default or frozen template. Completion text is tokenized exactly as
submitted without chat rendering; exact token-ID input bypasses decoding and
re-tokenization. Fill-in-middle is represented in the public type system but
remains unavailable unless a loaded model exposes and passes a verified FIM
token contract.

## Reference product: Mom Llama

Mom Llama is an upstream-faithful native port of the llama.cpp web UI with one
general extension: any frozen conversation template or live chat can be invited
into an ordinary chat by its unique `@handle`. A Persona is not a special agent
runtime. It is a versioned conversation branch plus an explicit execution
profile: model, optional system message and projector, sampler, chat-template
policy, context budgets, and allowlisted local tools.

Consult groups are ordered Settings records containing one to four Persona
references. Selecting a group inserts its handle into the normal composer;
responses appear as ordinary, attributed, independently editable messages.
There is no separate Consult transcript, result-card dashboard, or source-chat
writeback. The exact user-supplied therapeutic Persona templates are documented
in `PERSONA_LIBRARY.md`; no groups are seeded on the user's behalf.

Therapeutic Personas are private, user-configured conversation templates for a
licensed mental-health professional. Their supplied names and system prompts
are preserved exactly rather than rewritten into abstract labels.

## Frontend boundary

Rust owns authoritative state and rendered projections. A small local
JavaScript bridge may perform token insertion, DOM swaps, focus, keyboard,
clipboard, paste and drag/drop behavior. No frontend framework or browser
networking is required for core behavior.

## Release meaning

The app is not released until:

- `scripts/check-persona-product-ux.sh` passes from an empty store, proving the
  exact supplied Persona catalog, zero application-seeded groups, a visible
  Persona start path, and transcript-position preservation;
- the pinned upstream behavior ledger has no unacknowledged required gaps;
- same-state visual comparisons pass;
- an official Gemma 4 GGUF completes single chat, direct Persona mention,
  four-Persona group mention, independent cancellation, synthesis,
  cache-restart and persistence acceptance;
- a signed-development native bundle passes human inspection;
- the first-run walkthrough can find a saved Persona and start its chat without
  opening Settings, and a completed response does not move the reader's place;
- Mom accepts the product.
