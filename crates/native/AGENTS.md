# Llama Native Kit implementation contract

This repository owns only the reusable, process-free llama.cpp runtime.
Product behavior belongs in downstream repositories. In particular, Mom Llama
belongs in `delysis/mom-llama`, and routing/protocol/speech behavior belongs in
`delysis/free-token-energy`.

## Non-negotiables

- Normal inference is in-process. Inference crates may not use networking,
  subprocesses, shell commands, or executable discovery.
- All llama.cpp objects stay on their owning worker thread. Binding objects and
  raw pointers never cross the public Rust API.
- Project-owned unsafe code is forbidden except in the documented native state
  buffer module, where every block must state its safety argument.
- Fixture engines are permanently labeled fixtures and never unlock runtime
  readiness.
- Cache reuse is an optimization. A fingerprint mismatch invalidates the cache
  and falls back to ordinary generation.
- The host accepts injected cache storage. It does not choose product databases,
  key managers, routing profiles, UI policy or protocol compatibility layers.
- `llama-native-*` crates may not depend on Free Token Energy, Mom Llama, Tauri,
  HTTP clients, network servers or process execution.

## Verification

Run, at minimum:

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check-architecture.sh
```
