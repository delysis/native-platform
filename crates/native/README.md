# Llama Native Kit

A reusable, local-first Rust integration for llama.cpp, with Mom Llama as its
reference Tauri application.

The runtime loads GGUF models directly in-process. It does not use
`llama-server`, `llama-cli`, localhost, HTTP, TCP or SSE for inference. Safe
Rust owns model scheduling, streaming, cancellation, encrypted persistence,
cache policy, consult groups and readiness evidence.

## Workspace

- `llama-native-types`: stable public DTOs.
- `llama-native-engine`: a model-owning worker around the pinned llama.cpp
  binding.
- `command-evidence`: source- and runtime-fingerprint-bound readiness receipts.
- `mom-llama-runtime`: conversations, Skills, consults, encrypted storage and
  cache policy.
- `mom-llama-cli`: the complete machine-exercisable command surface.
- `apps/mom-llama`: Maud/Tauri application with a thin native interaction
  bridge.

The project is under active consolidation. `docs/PRODUCT.md` and the contract
ledgers distinguish implemented behavior from release requirements.

