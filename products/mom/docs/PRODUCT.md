# Product contract

## Reusable runtime

Provide an elegant, portable and integrated llama.cpp runtime for native Rust
applications:

- direct in-process model loading;
- single and multi-sequence generation;
- independent cancellation;
- resident model budgeting;
- multimodal inputs through native llama.cpp APIs;
- tiered, fingerprint-bound persistent prefix caching;
- encrypted transactional state;
- typed CLI and Tauri adapters;
- evidence-gated readiness.

## Reference product: Mom Llama

Mom Llama is an upstream-faithful native port of the llama.cpp web UI with one
deliberate extension: clearly labeled virtual consult groups based on
Mom-curated public ideas and long prompt packs.

Consultants are AI reasoning perspectives. They are not the real public
figures, do not imply endorsement, and are not therapy or medical care.

## Frontend boundary

Rust owns authoritative state and rendered projections. A small local
JavaScript bridge may perform token insertion, DOM swaps, focus, keyboard,
clipboard, paste and drag/drop behavior. No frontend framework or browser
networking is required for core behavior.

## Release meaning

The app is not released until:

- the pinned upstream behavior ledger has no unacknowledged required gaps;
- same-state visual comparisons pass;
- an official Gemma 4 GGUF completes single-chat, four-seat consult,
  cancellation, cache-restart and persistence acceptance;
- a signed-development native bundle passes human inspection;
- Mom accepts the product.

