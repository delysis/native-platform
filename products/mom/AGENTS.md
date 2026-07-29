# Llama Native Kit implementation contract

This repository owns one reusable, process-free llama.cpp runtime and one
reference product: Mom Llama.

## Product hierarchy

1. `docs/PRODUCT.md`
2. `contracts/upstream-parity.json`
3. `contracts/effects.json`
4. `contracts/commands.json`
5. This file
6. Comments and historical receipts

## Non-negotiables

- Normal inference is in-process. Inference crates may not use networking,
  subprocesses, shell commands, or executable discovery.
- All llama.cpp objects stay on their owning worker thread. Binding objects and
  raw pointers never cross the public Rust API.
- Project-owned unsafe code is forbidden except in the documented native state
  buffer module, where every block must state its safety argument.
- Backend operations are CLI-exercisable before the GUI enables them.
- Fixture engines are permanently labeled fixtures and never unlock runtime
  readiness.
- Persistent sensitive content is authenticated and encrypted at rest.
- Cache reuse is an optimization. A fingerprint mismatch invalidates the cache
  and falls back to ordinary generation.
- Rust owns authoritative state, validation, persistence, policy, inference and
  receipts. The webview owns only transient presentation behavior.
- The upstream UI is pinned by commit. Parity claims require command tests and
  same-state visual evidence.
- Do not claim clinical validity, impersonation, endorsement, HIPAA compliance,
  user acceptance or release without the corresponding evidence.

## Verification

Run, at minimum:

```sh
cargo fmt --all --check
cargo test --workspace --exclude mom-llama-app
cargo clippy --workspace --all-targets --exclude mom-llama-app -- -D warnings
node --check apps/mom-llama/ui/coop-hx.js
./scripts/check-architecture.sh
./scripts/check-contracts.sh
```

Real-model and Tauri bundle checks are separate opt-in acceptance gates.

